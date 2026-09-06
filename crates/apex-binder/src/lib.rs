//! The symbol-table binder: turns "we can parse and walk Apex" into "we
//! can answer real LSP questions." [`BoundProgram::from_files`] walks a
//! whole project (via `apex_discover::discover`) through three passes,
//! each one run in parallel (`rayon`) wherever the work is genuinely
//! per-item independent, with a fast sequential pass merging the
//! results back into shared state where it isn't (see each stage's
//! comments below for exactly where and why):
//!
//! 1. **Collect** (`collect`): every file's declarations
//!    (`apex_syntax::ast::decl`) become [`symbol::Symbol`]s in one
//!    project-wide [`symbol_table::SymbolTable`].
//! 2. **Inherit** (`inherit`): each type's `extends`/`implements` chain
//!    is resolved now that the whole project's declarations exist.
//! 3. **Resolve** (`resolve`/`soql`): every method/constructor/property-
//!    accessor body and field/property initializer is walked, building
//!    per-body [`scope::ScopeTree`]s and populating a
//!    [`reference_table::ReferenceTable`] with what every name reference
//!    resolved to.
//!
//! v1's scope is deliberately narrower than full semantic analysis --
//! see the project's design plan for the complete rationale. In short:
//! declaration-site binding is unconditionally precise; reference-site
//! resolution is precise for unqualified names (locals/params/fields/
//! types, which Apex guarantees resolve unambiguously by simple name)
//! and calls go through real (arity-exact, best-effort type-narrowed)
//! overload resolution (`crate::resolve::narrow_by_overload`). What's
//! left honestly imprecise is exactly what would require a full
//! expression-type-inference engine v1 doesn't have: an argument whose
//! type is a literal or an unmodeled system/library type can't be used
//! to disambiguate an overload, and a qualified access only chains past
//! `Unresolved` when its target's type is already known one hop away
//! (`Resolution::Candidates`/`Unresolved` rather than a guessed single
//! answer in those cases).

mod call_hierarchy;
mod ci_key;
mod collect;
mod completion;
mod conversions;
mod dead_code;
mod db;
mod file_id;
mod file_table;
mod generics;
mod incremental;
mod inherit;
mod label_index;
mod page_index;
mod ptr;
mod reference_table;
mod resolve;
#[cfg(test)]
mod salsa_stage1_dual_run;
#[cfg(test)]
mod salsa_stage2_dual_run;
mod schema_index;
mod scope;
mod soql;
mod stdlib_index;
mod symbol;
mod symbol_table;
mod ty;
mod visibility_narrowing;

pub use call_hierarchy::{incoming_calls, is_callable, outgoing_calls, IncomingCall, OutgoingCall};
pub use completion::{complete_at, CompletionCandidate, CompletionCandidateKind, CompletionContext};
pub use dead_code::{
    kind_label, dead_symbols_in_file, is_platform_invoked_test_method, is_test_class, DeadSymbol,
};
pub use file_id::FileId;
pub use incremental::BindCache;
pub use ptr::{AstPtr, SyntaxPtr};
pub use reference_table::{
    ExternalKey, LabelRef, ReferenceTable, Resolution, SchemaObjectRef, StdlibMemberRef,
    UnknownSchemaRef, VisualforcePageRef,
};
pub use resolve::TypeMismatch;
pub use label_index::LabelIndex;
pub use page_index::{PageIndex, VisualforcePage};
pub use schema_index::SchemaIndex;
pub use stdlib_index::StdlibIndex;
pub use scope::{Scope, ScopeId, ScopeKind, ScopeTree};
pub use symbol::{ModifierSet, Sharing, Symbol, SymbolId, SymbolKind, Visibility};
pub use symbol_table::SymbolTable;
pub use visibility_narrowing::{
    narrowing_candidates_in_file, type_narrowing_candidates_in_file, NarrowingCandidate,
};
pub use apex_parser::ParseError;

use apex_parser::Parse;
use apex_syntax::ast::decl::{
    ConstructorDecl, MethodDecl, PropertyDecl, TriggerUnit, VarDeclarator,
};
use apex_syntax::SyntaxNode;
use incremental::{FileBodies, Freshness};
use rayon::prelude::*;
use rowan::ast::AstNode;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A whole bound project: every file's parse tree, the project-wide
/// symbol table, every reference's resolution, the Salesforce object/
/// field schema index consulted along the way, and every body's scope
/// tree (via [`Self::scope_tree`], keyed by that body's own
/// `Block`/`TriggerBlock` [`SyntaxPtr`] -- not by symbol, since a
/// property's two accessors share one symbol but have two independent
/// bodies). An independent, owned snapshot -- it doesn't borrow from
/// whatever [`BindCache`] produced it, so a caller (`apexls-server`) can
/// hold one `BoundProgram` while a background rebuild computes the next
/// one.
///
/// `bodies` is keyed and `Arc`-shared per file (rather than one flat
/// merged `ReferenceTable`/scope map, which this type exposed in an
/// earlier revision) specifically so assembling this snapshot in
/// `Self::from_files_cached` only costs a pointer clone per *unaffected*
/// file, not a deep copy of every reference/scope in the whole project
/// -- see `crate::symbol_table`'s module doc comment for the same
/// reasoning applied to `symbols`, and the measured regression that
/// motivated both.
pub struct BoundProgram {
    files: FxHashMap<FileId, PathBuf>,
    /// Reverse of `files` -- an LSP request only ever names a file by its
    /// URI/path, never by `FileId`, so this is what `Self::file_id` looks
    /// up through. Small (one entry per file) and cheap to keep alongside
    /// `files` rather than searched linearly per request.
    file_ids: FxHashMap<PathBuf, FileId>,
    parses: FxHashMap<FileId, Parse>,
    /// Every current file's source text, independent of whether its
    /// `Parse` is still resident in `parses` (ticket 07,
    /// `.scratch/apex-performance/`: `BindCache::file_parses` is now
    /// eviction-capped, so a file untouched for a while may be missing from
    /// `parses` even though it's still part of the project). `Self::syntax`
    /// falls back to re-parsing from here on a `parses` miss -- an `Arc<str>`
    /// clone per file (ticket 11 made this a pointer bump, not a byte copy),
    /// so retaining this for every file costs only what ticket 03 already
    /// measured raw source text costs (~7% of the total footprint), never
    /// the far larger green tree an evicted file no longer pays for.
    texts: FxHashMap<FileId, Arc<str>>,
    pub symbols: SymbolTable,
    /// `Arc`-wrapped for the same reason `symbols`/`bodies` are cheap to
    /// clone into each call's snapshot: `crate::db::schema_index` only
    /// ever recomputes when the directory walk itself is redone (Wayfinder
    /// `apex-diagnostics` map, ticket 26's salsa-backed memoization -- see
    /// `Self::from_files_cached`), so the common case is a pointer clone,
    /// not re-parsing every SFDX metadata XML file.
    pub schema: Arc<SchemaIndex>,
    /// Bundled standard-library class/method/property schema
    /// (`apex_stdlib::standard_classes`) -- unlike `schema`, this has no
    /// project-specific data at all (never merges with anything
    /// discovered from `root`), so it's a single process-wide singleton
    /// (`global_stdlib_index`) rather than something `BindCache` rebuilds
    /// alongside a fresh directory walk.
    pub stdlib: Arc<StdlibIndex>,
    /// Every project-declared custom label (`.labels-meta.xml`), keyed by
    /// `full_name` -- what `Label.xxx`/`System.Label.xxx` references
    /// resolve against. `Arc`-wrapped and rebuilt alongside `schema` for
    /// the same reason: derived from the same directory walk, only
    /// redone on the same `need_fresh_discovery` trigger.
    pub labels: Arc<LabelIndex>,
    /// Every project-declared Visualforce page, keyed by file-stem name --
    /// what a `Page.<name>` reference resolves against. `Arc`-wrapped and
    /// rebuilt alongside `schema`/`labels` for the same reason.
    pub pages: Arc<PageIndex>,
    bodies: FxHashMap<FileId, Arc<FileBodies>>,
    /// Every class name (lowercased) a real `.page` file names as its
    /// `controller`/`extensions` -- `crate::dead_code`'s Visualforce-
    /// exposure check. `Arc`-wrapped for the same reason `schema` is:
    /// rebuilt only alongside it, on the same `need_fresh_discovery`
    /// trigger, so the common case across calls is a pointer clone.
    pub vf_referenced_classes: Arc<std::collections::HashSet<String>>,
}

/// Rayon lazily builds its global thread pool (default stack size, ~1
/// MiB on this project's Windows dev machine) on first use unless
/// something builds it explicitly first -- called once, before any
/// `par_iter()` below, to give every worker thread enough stack to
/// safely *drop* a deeply left-nested expression tree (see
/// `apex_parser`'s module doc comment's "deep-tree stack safety
/// caveat"; every parsed file's tree is ultimately dropped on one of
/// these workers, whether immediately or much later as part of
/// `BoundProgram` itself being dropped).
static ENSURE_LARGE_WORKER_STACKS: std::sync::Once = std::sync::Once::new();

/// The bundled standard-library index, built once per process and
/// shared (via cheap `Arc` clones) across every `BoundProgram` --
/// unlike `SchemaIndex`, nothing about it depends on `root`, so there's
/// no per-project staleness to track the way `crate::db::schema_index`
/// has.
fn global_stdlib_index() -> Arc<StdlibIndex> {
    static STDLIB: std::sync::OnceLock<Arc<StdlibIndex>> = std::sync::OnceLock::new();
    Arc::clone(STDLIB.get_or_init(|| Arc::new(StdlibIndex::new())))
}

fn ensure_large_worker_stacks() {
    ENSURE_LARGE_WORKER_STACKS.call_once(|| {
        // Ignore failure: it only means the global pool was already
        // built by something else (an earlier call in this process, or
        // a host application's own rayon use) before we got here --
        // that pool's stack size is then outside this crate's control,
        // a broader application-level concern rather than something to
        // panic over.
        let _ = rayon::ThreadPoolBuilder::new()
            .stack_size(apex_parser::RECOMMENDED_MIN_STACK_SIZE)
            .build_global();
    });
}

impl BoundProgram {
    /// Discovers, parses, and binds every `.cls`/`.trigger` file under
    /// `root`, plus its SFDX object/field metadata. Always starts from a
    /// cold [`BindCache`] -- see [`Self::from_files_cached`] for a
    /// version that reuses a persistent cache across repeated calls
    /// (what an editor-backed caller wants; `from_files` itself exists
    /// mainly for one-shot batch tools like `apexls-cli` and tests/
    /// benches that don't need incrementality).
    pub fn from_files(root: impl AsRef<Path>) -> Self {
        Self::from_files_with_overrides(root, &HashMap::new())
    }

    /// Like [`Self::from_files`], but `overrides` (keyed by the same
    /// paths `apex_discover::discover` would report) supplies a file's
    /// content directly instead of reading it from disk -- the hook an
    /// editor-backed caller (`apexls-server`) needs so an *unsaved*
    /// buffer's content participates in binding instead of stale
    /// on-disk content. A path with no entry in `overrides` is read from
    /// disk as normal.
    pub fn from_files_with_overrides(
        root: impl AsRef<Path>,
        overrides: &HashMap<PathBuf, String>,
    ) -> Self {
        let mut cache = BindCache::default();
        Self::from_files_cached(root, overrides, &mut cache)
    }

    /// Like [`Self::from_files_with_overrides`], but reuses and patches
    /// `cache` (a [`BindCache`]) instead of recomputing the whole
    /// project from nothing. Three things get skipped when they safely
    /// can, each cheaper than the last to check:
    ///
    /// 1. **Reparsing**: a file whose content is byte-for-byte identical
    ///    to `cache`'s last-seen copy reuses that `Parse` outright.
    /// 2. **Rebuilding `SymbolTable`'s derived indices and `crate::inherit`'s
    ///    inheritance chains**: skipped entirely unless some file's
    ///    *declared shape* actually changed this call (a dirty file's
    ///    new declarations are compared against its previous ones by
    ///    `declarations_equivalent` -- kind/name/container/type/
    ///    modifiers only, deliberately ignoring byte ranges, which shift
    ///    on *any* edit earlier in the file even when nothing declared
    ///    did).
    /// 3. **Pass 2 (body resolution)**: reruns for *every* file if any
    ///    file's declarations changed (conservative but safe -- v1
    ///    doesn't track fine-grained per-reference dependencies, see
    ///    `BACKLOG.md` §2's honest scope note), but reruns for *only the
    ///    dirty files* otherwise -- the common "typing inside a method
    ///    body" case, which never touches declarations at all.
    ///
    /// `SymbolId`/`FileId` stability (`crate::symbol::SymbolId`,
    /// `crate::file_table::FileTable`) is what makes patching file-by-
    /// file sound instead of stale: an unrelated file's ids never shift
    /// just because another file's declaration count changed.
    pub fn from_files_cached(
        root: impl AsRef<Path>,
        overrides: &HashMap<PathBuf, String>,
        cache: &mut BindCache,
    ) -> Self {
        ensure_large_worker_stacks();
        let root = root.as_ref();

        // Stage -1: reuse the cached directory walk (`Discovery`) and
        // `SchemaIndex` unless something suggests they're stale, rather
        // than re-walking the whole tree (and re-parsing every SFDX
        // metadata XML file) unconditionally on every call. There's no
        // way to detect "a file appeared/disappeared on disk" short of
        // watching the filesystem (not wired up yet -- an honest,
        // separate gap, not something this check papers over), but a
        // *newly created and already-open* file has a real signal: it
        // shows up in `overrides` before the cached walk has ever heard
        // of it. That, or no cached walk existing yet, are the only
        // triggers for redoing it.
        let need_fresh_discovery = match &cache.discovery {
            None => true,
            Some(discovery) => {
                let known: FxHashSet<&Path> =
                    discovery.apex_files.iter().map(PathBuf::as_path).collect();
                overrides.keys().any(|p| !known.contains(p.as_path()))
            }
        };
        if need_fresh_discovery {
            hotpath::measure_block!("discover_and_build_schema", {
                let discovery = apex_discover::discover(root);
                let discovery_input =
                    db::sync_discovery_into_db(&mut cache.db, cache.discovery_input, &discovery);
                cache.discovery_input = Some(discovery_input);
                cache.discovery = Some(discovery);
            });
        }
        let discovery = cache.discovery.as_ref().unwrap();
        let discovery_input = cache.discovery_input.unwrap();
        // Every call (not just a fresh-walk one) reads through these
        // salsa-memoized queries rather than a locally cached `Arc` clone
        // -- ticket 26's own Stage 1 cutover (Wayfinder `apex-diagnostics`
        // map) deleted `BindCache`'s (formerly redundant) `schema`/
        // `labels`/`pages`/`vf_referenced_classes` fields entirely, per
        // ticket 25's "no permanent shadow copy" rule. Deliberately
        // sequential, not `rayon::join`-parallelized the way the pre-
        // salsa build overlapped `SchemaIndex::from_discovery`/
        // `LabelIndex::from_discovery` with `global_stdlib_index`'s own
        // one-time JSON parse: a salsa database's thread-local "attached"
        // state (`salsa::attach`) isn't safe to hand to `rayon`'s
        // *shared, process-global* worker pool while another, unrelated
        // `BindDatabase` (a different `BoundProgram::from_files` call
        // running concurrently -- confirmed via `cargo test`'s parallel
        // test threads) might already be attached on the same physical
        // worker thread. A call whose `discovery_input` didn't just
        // change is a cheap memoized-value lookup in each query below,
        // not a recompute, so the lost parallelism only costs anything on
        // the comparatively rare fresh-walk call.
        let schema = db::schema_index(&cache.db, discovery_input);
        let labels = db::label_index(&cache.db, discovery_input);
        let pages = db::page_index(&cache.db, discovery_input);
        let vf_referenced_classes = db::vf_referenced_classes(&cache.db, discovery_input);
        let stdlib = global_stdlib_index();

        // Stage 0: resolve every discovered path to a stable `FileId`.
        // This is only a *candidate* list -- Stage 1a below is what
        // actually determines which of these still exist and are
        // readable, since `discovery` can itself be stale (reused from
        // an earlier call, per the above) even when it wasn't worth a
        // fresh walk.
        let candidates: Vec<(PathBuf, FileId)> = discovery
            .apex_files
            .iter()
            .map(|path| (path.clone(), cache.files.id_for(path)))
            .collect();

        // Stage 1a (parallel): check each candidate's on-disk/`overrides`
        // freshness (a stat, or an override's content hash) against
        // `cache.freshness`'s last-seen value -- cheap enough to run for
        // every candidate every call, and what decides which files are
        // actually dirty this call without reading, let alone parsing,
        // anything unaffected (ticket 29, Wayfinder `apex-diagnostics`
        // map: parsing itself moved to `crate::db::parse_query`, a
        // per-file salsa-tracked query -- see Stage 1b below -- so this
        // stage's only job now is deciding *which* files need one). A
        // candidate that fails to read (deleted since the walk that
        // produced it, a permissions race, ...) is silently dropped
        // here, exactly as it always was -- and that silent drop is
        // *also* this function's only signal that a file was removed
        // (see `current_ids` below), so a deletion self-corrects without
        // ever needing a fresh walk, only a genuinely new file does.
        struct CandidateFile {
            path: PathBuf,
            file: FileId,
            trigger: bool,
            dirty: bool,
            freshness: Freshness,
            /// Only `Some` for a dirty file -- the freshly-read text that
            /// file's salsa input needs. An unaffected file's input is
            /// never touched: ticket 27's confirmed load-bearing gotcha
            /// (writing a salsa input cancels every other in-flight
            /// query on every other clone of the database) makes this a
            /// correctness requirement here, not just an optimization.
            /// `Arc<str>` (not `String`) built directly here, not
            /// converted later at the `sync_file_text_into_db` call site
            /// -- the override branch already only has a borrowed
            /// `&String` on hand, so building the `Arc<str>` here is one
            /// allocation (`Arc::from(&str)`), not two (a `String` clone
            /// followed by a separate `Arc::from(String)` conversion).
            text: Option<Arc<str>>,
        }
        let check_freshness = |path: PathBuf, file: FileId| -> Option<CandidateFile> {
            let trigger = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));

            // An `overrides` entry (an unsaved editor buffer) has no
            // filesystem metadata to stat -- content is already in
            // memory (the caller supplied it, no disk I/O either way),
            // so it's compared by content hash, same as before. In
            // practice this is at most a handful of files per call
            // (whatever's actually being edited).
            if let Some(content) = overrides.get(&path) {
                let freshness =
                    Freshness::ContentHash(incremental::content_fingerprint(content));
                if cache.freshness.get(&path) == Some(&freshness) {
                    return Some(CandidateFile {
                        path,
                        file,
                        trigger,
                        dirty: false,
                        freshness,
                        text: None,
                    });
                }
                return Some(CandidateFile {
                    path,
                    file,
                    trigger,
                    dirty: true,
                    freshness,
                    text: Some(Arc::from(content.as_str())),
                });
            }

            // No override: stat the file *before* reading it -- a
            // size+mtime match against the cached entry means the read
            // can be skipped entirely, which is the common case for
            // every file besides the one actually being edited. See
            // `Freshness::Stat`'s doc comment for the honest staleness
            // caveat this trades for that.
            let metadata = std::fs::metadata(&path).ok()?;
            let stat_freshness = metadata.modified().ok().map(|modified| Freshness::Stat {
                len: metadata.len(),
                modified,
            });
            if let Some(freshness) = &stat_freshness {
                if cache.freshness.get(&path) == Some(freshness) {
                    return Some(CandidateFile {
                        path,
                        file,
                        trigger,
                        dirty: false,
                        freshness: freshness.clone(),
                        text: None,
                    });
                }
            }
            let content = std::fs::read_to_string(&path).ok()?;
            // `metadata().modified()` failing at all is rare and
            // platform-dependent -- fall back to a content hash so this
            // file still gets *some* freshness check next call, just not
            // the read-skipping kind.
            let freshness = stat_freshness.unwrap_or_else(|| {
                Freshness::ContentHash(incremental::content_fingerprint(&content))
            });
            Some(CandidateFile {
                path,
                file,
                trigger,
                dirty: true,
                freshness,
                text: Some(Arc::from(content)),
            })
        };
        let mut checked: Vec<CandidateFile> = hotpath::measure_block!("stage_1a_check_freshness", {
            candidates
                .into_par_iter()
                .filter_map(|(path, file)| check_freshness(path, file))
                .collect()
        });

        // Refresh the freshness cache for dirty files only -- an
        // unchanged file's entry is already correct.
        for c in checked.iter().filter(|c| c.dirty) {
            cache.freshness.insert(c.path.clone(), c.freshness.clone());
            cache.paths.insert(c.file, c.path.clone());
            cache.path_ids.insert(c.path.clone(), c.file);
        }

        // The *actual* current file set is whatever was just
        // successfully checked above, not `discovery`'s candidate list --
        // this is what makes a deleted file self-correct even when
        // `discovery` itself is stale (reused from an earlier call): it
        // simply isn't in `checked`. A removal changes the project-wide
        // namespace just as much as an addition does, so it also forces
        // the conservative "declarations changed" path below.
        let current_ids: FxHashSet<FileId> = checked.iter().map(|c| c.file).collect();
        let current_paths: FxHashSet<&Path> = checked.iter().map(|c| c.path.as_path()).collect();

        let removed: Vec<FileId> = cache
            .table
            .known_files()
            .filter(|id| !current_ids.contains(id))
            .collect();
        let mut declarations_changed = !removed.is_empty();
        // Whether the *file set itself* changed (added/removed), not
        // just declarations within already-known files -- Stage 3's own
        // `db::FileSetInput` (Wayfinder `apex-diagnostics` map, ticket
        // 30/31) is only re-synced on this narrower trigger, not every
        // call the way `discovery_input` is, since setting a salsa input
        // always bumps its own revision regardless of content equality.
        let mut file_set_changed = !removed.is_empty();
        for file in removed {
            cache.table.remove_file(file);
            cache.raw_extends.remove(&file);
            cache.raw_super.remove(&file);
            cache.supertype_ptrs.remove(&file);
            cache.bodies.remove(&file);
            if let Some(path) = cache.paths.remove(&file) {
                cache.path_ids.remove(&path);
            }
            cache.file_parses.remove(&file);
            cache.parse_last_used.remove(&file);
            cache.file_text_inputs.remove(&file);
        }
        cache
            .freshness
            .retain(|path, _| current_paths.contains(path.as_path()));

        // Stage 1a.5 (sequential): push each dirty file's freshly-read
        // text into its own salsa `FileTextInput`, creating it the first
        // time a file's seen and reusing it (its `text` field
        // overwritten) every time after. Every dirty file's input gets
        // set here, sequentially, strictly before Stage 1b's parallel
        // salsa reads below ever begin -- never interleaved with them,
        // per ticket 27's confirmed load-bearing gotcha.
        let dirty_inputs: Vec<(FileId, db::FileTextInput)> = checked
            .iter_mut()
            .filter(|c| c.dirty)
            .map(|c| {
                let existing = cache.file_text_inputs.get(&c.file).copied();
                let input = db::sync_file_text_into_db(
                    &mut cache.db,
                    existing,
                    c.file,
                    c.trigger,
                    c.text.take().expect("dirty candidate always carries fresh text"),
                );
                cache.file_text_inputs.insert(c.file, input);
                (c.file, input)
            })
            .collect();

        // Stage 1b (parallel): Pass 1 parsing + declaration collection
        // for dirty files only, routed through `crate::db`'s salsa-
        // tracked `parse_query`/`collect_query` (ticket 29) -- an
        // unchanged file's `Parse`/declarations are still exactly what
        // `cache.file_parses`/`cache.table`/`cache.raw_extends`/
        // `cache.raw_super` already hold. `Storage<Db>` isn't `Sync`
        // (confirmed, ticket 28), so every task below gets its own
        // already-cloned `BindDatabase` handed in by value, never a
        // shared reference to one outer `db` cloned from inside the
        // closure -- the working pattern salsa's own parallel tests use.
        let fresh: Vec<(FileId, Parse, collect::FileCollection)> =
            hotpath::measure_block!("pass1_collect_dirty_files", {
                let owned: Vec<(FileId, db::FileTextInput, db::BindDatabase)> = dirty_inputs
                    .iter()
                    .map(|&(file, input)| (file, input, cache.db.clone()))
                    .collect();
                owned
                    .into_par_iter()
                    .map(|(file, input, db)| {
                        let parse = db::parse_query(&db, input);
                        let collection = db::collect_query(&db, input);
                        (file, parse, collection)
                    })
                    .collect()
            });

        // Sequential merge: patch each dirty file's slice of the
        // persistent `SymbolTable`/Pass-1.5 inputs (plus its cached
        // `Parse`), tracking whether any file's *declared shape* actually
        // changed. Cheap (no parsing/tree-walking left to do here, just
        // moving already-built `Symbol`s and a same-length field-by-field
        // comparison).
        for (file, parse, collection) in fresh {
            let is_new = !cache.table.has_file(file);
            if is_new {
                file_set_changed = true;
            }
            if is_new
                || !declarations_equivalent(
                    cache.table.declared_symbols_of_file(file),
                    &collection.symbols,
                )
            {
                declarations_changed = true;
            }
            cache.table.set_file_symbols(file, collection.symbols);
            cache.raw_extends.insert(file, collection.raw_extends);
            cache.raw_super.insert(file, collection.raw_super);
            cache.supertype_ptrs.insert(file, collection.supertype_ptrs);
            cache.file_parses.insert(file, parse);
        }

        // Stage 3 (Wayfinder `apex-diagnostics` map, ticket 30/31):
        // re-sync `db::FileSetInput` only when the file set itself
        // changed, sorted by `FileId` for determinism (ticket 26's own
        // documented hash-map-iteration-order lesson applies here too --
        // an unsorted `Vec` would make an unchanged project look
        // "changed" to salsa purely from iteration-order jitter). Every
        // current file already has a `file_text_inputs` entry by this
        // point: an unaffected file's persists from a previous call, and
        // Stage 1a.5 above just created/refreshed every dirty file's
        // (including any brand-new file's) entry.
        if file_set_changed {
            let mut entries: Vec<(FileId, db::FileTextInput)> = current_ids
                .iter()
                .filter_map(|&file| cache.file_text_inputs.get(&file).map(|&input| (file, input)))
                .collect();
            entries.sort_by_key(|&(file, _)| file);
            let existing = cache.file_set_input;
            cache.file_set_input =
                Some(db::sync_file_set_into_db(&mut cache.db, existing, entries));
        }

        // Pass 1.5 + derived-index rebuild only when something actually
        // declared changed project-wide -- otherwise every index and
        // `inherited_chain`/`direct_super` entry is still exactly
        // correct from the previous call (see `SymbolTable::rebuild_indices`'s
        // doc comment for why skipping this is sound, not just fast).
        // `resolve_inheritance` itself has its own, narrower trigger one
        // level down (ticket 30/31): `rebuild_indices` still runs on
        // every declaration change (member additions/removals need
        // fresh `members_of`/`members_by_name` too), but re-running
        // `resolve_inheritance` needs only `db::raw_inheritance_inputs`'s
        // bundled content to have actually changed -- a strict subset of
        // what trips `declarations_changed` (a method/field/property
        // addition with no bearing on any type name or `extends`/
        // `implements` clause anywhere no longer forces a project-wide
        // re-flatten). `inherit.rs`/`resolve_inheritance`/`SymbolTable`
        // are otherwise completely unchanged by this stage.
        if declarations_changed {
            hotpath::measure_block!("pass1_5_inherit_and_rebuild_indices", {
                cache.table.rebuild_indices();
                let file_set = cache
                    .file_set_input
                    .expect("declarations_changed implies at least one known file");
                let inputs = db::raw_inheritance_inputs(&cache.db, file_set);
                let inputs_changed = cache.raw_inheritance_inputs.as_deref() != Some(&*inputs);
                if inputs_changed {
                    inherit::resolve_inheritance(
                        &mut cache.table,
                        &inputs.raw_extends,
                        &inputs.raw_super,
                    );
                }
                cache.raw_inheritance_inputs = Some(inputs);
            });
        }

        // Pass 2 (parallel): rebind every current file if declarations
        // changed anywhere (conservative fallback, identical cost to a
        // full rebuild), or just the dirty files otherwise -- see this
        // method's doc comment. Ticket 04 (`.scratch/apex-memory/`) narrows
        // both branches to `working_set` (pinned open files plus LRU-capped
        // on-demand-promoted ones -- `cache.open_paths`/`cache.promoted_files`)
        // whenever `cache.scoping_enabled` is set: a dirty file outside it
        // only gets Pass 1 (already unconditional, above) -- no Pass 2 rerun
        // -- and a declaration change anywhere only forces a rebind of the
        // working set, not the whole project, since cross-file resolution
        // never depends on another file's own Pass 2 output (ticket 03).
        // `None` (scoping off) preserves the exact prior behavior for every
        // CLI/test/batch caller.
        let working_set: Option<FxHashSet<FileId>> = cache.scoping_enabled.then(|| {
            let mut set: FxHashSet<FileId> = checked
                .iter()
                .filter(|c| cache.open_paths.contains(&c.path))
                .map(|c| c.file)
                .collect();
            set.extend(
                cache
                    .promoted_files
                    .keys()
                    .copied()
                    .filter(|f| current_ids.contains(f)),
            );
            set
        });
        let files_to_rebind: Vec<FileId> = match &working_set {
            Some(working_set) => {
                if declarations_changed {
                    working_set.iter().copied().collect()
                } else {
                    checked
                        .iter()
                        .filter(|c| c.dirty && working_set.contains(&c.file))
                        .map(|c| c.file)
                        .collect()
                }
            }
            None => {
                if declarations_changed {
                    current_ids.iter().copied().collect()
                } else {
                    checked.iter().filter(|c| c.dirty).map(|c| c.file).collect()
                }
            }
        };
        // Prune `cache.bodies`/`promoted_files` down to exactly the current
        // working set -- a file that fell out of it since the last call
        // (closed, or LRU-evicted) has its Pass 2 data actually dropped
        // here, not just excluded from this call's own snapshot below, so
        // steady-state memory really does shrink back toward the
        // declaration-only floor rather than accumulating every file ever
        // promoted.
        if let Some(working_set) = &working_set {
            cache.bodies.retain(|file, _| working_set.contains(file));
            cache.promoted_files.retain(|file, _| working_set.contains(file));
        }
        // Built for exactly `files_to_rebind` (not blanket `current_ids`) --
        // a cache hit is a cheap `Parse::clone` (two `Arc` bumps), a miss
        // (ticket 07, `.scratch/apex-performance/`: `cache.file_parses` is
        // now eviction-capped, see its own doc comment) re-fetches via
        // `db::parse_query`'s own salsa memoization and re-populates
        // `file_parses` so a file touched again soon doesn't keep missing.
        // Every touched file's `parse_last_used` is bumped either way, so
        // `evict_stale_parses` (called once this whole call finishes) never
        // evicts something this very call just needed. Owned `Parse`
        // values, not `&Parse` -- `Parse` is cheap to clone and `Sync`
        // (`Arc`-based `GreenNode`, see its own doc comment), so every
        // parallel closure below can still safely share `&parse_by_file`
        // across threads and call `.syntax()` itself to build its own
        // thread-local node.
        cache.parse_generation += 1;
        let mut parse_by_file: FxHashMap<FileId, Parse> = FxHashMap::default();
        for &file in &files_to_rebind {
            let parse = match cache.file_parses.get(&file) {
                Some(parse) => parse.clone(),
                None => {
                    let input = cache.file_text_inputs[&file];
                    let parse = db::parse_query(&cache.db, input);
                    cache.file_parses.insert(file, parse.clone());
                    parse
                }
            };
            cache.parse_last_used.insert(file, cache.parse_generation);
            parse_by_file.insert(file, parse);
        }
        // Borrows straight from `cache.table` instead of cloning each
        // `Symbol` -- `bind_symbol_body` only ever needs `&Symbol`, and
        // `cache.table` isn't mutated again until after `bound` (below) is
        // fully collected, so there's nothing for an owned copy to buy
        // here except a String/Vec allocation per symbol, on every rebind.
        let to_bind: Vec<(FileId, SymbolId, &Symbol)> =
            files_to_rebind
                .iter()
                .flat_map(|&file| {
                    cache.table.symbols_of_file(file).iter().enumerate().map(
                        move |(local, symbol)| (file, SymbolId::new(file, local as u32), symbol),
                    )
                })
                .collect();
        let bound: Vec<(FileId, Option<SyntaxPtr>, resolve::BoundBody)> =
            hotpath::measure_block!("pass2_bind_symbol_bodies", {
                to_bind
                    .par_iter()
                    .flat_map(|(file, id, symbol)| {
                        let root_node = parse_by_file[file].syntax();
                        bind_symbol_body(
                            &cache.table,
                            &schema,
                            &stdlib,
                            &labels,
                            &pages,
                            &root_node,
                            *id,
                            symbol,
                        )
                            .into_iter()
                            .map(move |(key, body)| (*file, key, body))
                            .collect::<Vec<_>>()
                    })
                    .collect()
            });

        // Same idea as `bound`, but for `extends`/`implements` supertype
        // names -- these aren't attached to a `Symbol::type_ref` (a
        // class/interface can have more than one, see
        // `collect::FileCollection::supertype_ptrs`'s doc comment), so
        // they're resolved as their own small stage rather than through
        // `bind_symbol_body`.
        let supertype_bound: Vec<(FileId, Option<SyntaxPtr>, resolve::BoundBody)> =
            hotpath::measure_block!("pass2_bind_supertypes", {
                files_to_rebind
                    .par_iter()
                    .flat_map(|file| {
                        let root_node = parse_by_file[file].syntax();
                        cache
                            .supertype_ptrs
                            .get(file)
                            .into_iter()
                            .flatten()
                            .filter_map(|(owner, ptr)| {
                                let ty = ptr.to_node(&root_node)?;
                                Some((
                                    *file,
                                    None,
                                    resolve::bind_type_ref(
                                        &cache.table,
                                        &schema,
                                        &stdlib,
                                        *file,
                                        Some(*owner),
                                        &ty,
                                    ),
                                ))
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect()
            });

        // Sequential merge, mirroring Pass 1's: group each file's bound
        // bodies together, then -- one file at a time -- allocate its
        // bodies' pending locals onto the end of *that file's own*
        // declared symbols (not the whole project's), remapping each
        // body's sentinel ids to the resulting real ids as it goes (a
        // running per-file base, so sibling bodies bound concurrently in
        // the same file don't collide over the same local-id range).
        // Every file in `files_to_rebind` gets a fresh `FileBodies`
        // entry even if it produced zero bound bodies (e.g. every method
        // in it was just deleted) -- otherwise a stale fragment from
        // before that edit would silently survive in `cache.bodies`.
        hotpath::measure_block!("pass2_merge_bodies", {
            let mut by_file: FxHashMap<FileId, Vec<(Option<SyntaxPtr>, resolve::BoundBody)>> =
                files_to_rebind.iter().map(|&f| (f, Vec::new())).collect();
            for (file, key, body) in bound.into_iter().chain(supertype_bound) {
                by_file.entry(file).or_default().push((key, body));
            }
            for (file, bodies) in by_file {
                let file_bodies = merge_pass2_results(&mut cache.table, file, bodies);
                cache.bodies.insert(file, Arc::new(file_bodies));
            }
        });

        // Assemble this call's independent, owned snapshot. `symbols`
        // (`Arc`-backed, see `symbol_table`'s module doc comment) and
        // `bodies` (`Arc`-shared per file) are both cheap here regardless
        // of project size -- an *unaffected* file only costs a pointer
        // clone, not a deep copy of its `Symbol`s/references/scopes.
        // `files`/`file_ids`/`parses` were patched incrementally in
        // `cache` above (dirty/removed files only, not every file every
        // call), so assembling them here is one `.clone()` of each
        // already-correct persisted map, not `parsed.len()` fresh
        // inserts with a `PathBuf` clone apiece.
        let files = cache.paths.clone();
        let file_ids = cache.path_ids.clone();
        let parses = cache.file_parses.clone();
        // Every current file's source text, independent of whether its
        // `Parse` is still resident in `parses` above (ticket 07,
        // `.scratch/apex-performance/`: `file_parses` is now eviction-capped)
        // -- a cheap `Arc<str>` clone per file (ticket 11 made this a
        // pointer bump, not a byte copy), so `BoundProgram::syntax` always
        // has enough to re-derive a evicted file's tree on demand even when
        // its `Parse` itself isn't in `parses`.
        let texts: FxHashMap<FileId, Arc<str>> = current_ids
            .iter()
            .filter_map(|&file| {
                cache
                    .file_text_inputs
                    .get(&file)
                    .map(|&input| (file, input.text(&cache.db).clone()))
            })
            .collect();
        let bodies: FxHashMap<FileId, Arc<FileBodies>> = current_ids
            .iter()
            .filter_map(|&file| cache.bodies.get(&file).map(|fb| (file, Arc::clone(fb))))
            .collect();

        // Ticket 07's own eviction, applied after this call's snapshot is
        // fully assembled -- so it never evicts something *this* call
        // itself just needed, only what's gone stale since. The *next*
        // call's own `parses`/`texts` clone will reflect whatever this
        // sweep removed; this call's snapshot keeps what was resident at
        // assembly time regardless.
        cache.evict_stale_parses();

        BoundProgram {
            files,
            file_ids,
            parses,
            texts,
            symbols: cache.table.clone(),
            schema,
            stdlib,
            labels,
            pages,
            bodies,
            vf_referenced_classes,
        }
    }

    /// Every file this bind knows about. A `HashSet` dedup over
    /// `symbols`' own per-symbol `file` field rather than a stored list --
    /// cheap (one entry per symbol, not per file, but still a tiny
    /// fraction of a project's total symbol count) and avoids a
    /// dedicated file-list field nothing else needs; a batch caller
    /// wanting "every file" (`apexls check`'s whole-project scan) is the
    /// only consumer.
    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.symbols
            .iter()
            .map(|(_, s)| s.file)
            .collect::<rustc_hash::FxHashSet<_>>()
            .into_iter()
    }

    pub fn file_path(&self, file: FileId) -> &Path {
        &self.files[&file]
    }

    /// Reverse of [`Self::file_path`] -- the `FileId` a request-supplied
    /// path corresponds to, if it names a file that's actually part of
    /// this bound project. `path` must match exactly as discovered
    /// (canonicalization/case-folding, if a caller's path came from
    /// somewhere less exact than `apex_discover`, is the caller's job).
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.file_ids.get(path).copied()
    }

    /// Re-parses `file` on demand from its always-retained `texts` entry --
    /// the fallback [`Self::syntax`]/[`Self::syntax_errors`] share for a
    /// file whose `Parse` was evicted from `BindCache::file_parses` (ticket
    /// 07, `.scratch/apex-performance/`) since this snapshot's own
    /// assembly. Not cached anywhere: `BoundProgram` is an immutable,
    /// `Arc`-shared snapshot read concurrently by many request handlers, so
    /// caching a re-parse here would need its own interior-mutability
    /// story. Accepted v1 simplification -- a repeated read of the same
    /// rarely-edited-but-often-viewed file within one snapshot's lifetime
    /// re-parses every time, rather than a correctness gap; upgrade path is
    /// a `Mutex`-guarded per-snapshot cache if this ever measures as a real
    /// cost.
    fn reparse_evicted(&self, file: FileId) -> Parse {
        let text = self
            .texts
            .get(&file)
            .expect("every current file has retained text, even if its Parse was evicted")
            .clone();
        let trigger = self
            .files
            .get(&file)
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
        let mut cache = apex_parser::NodeCache::default();
        if trigger {
            apex_parser::parse_trigger_unit_with_cache_and_text(text, &mut cache)
        } else {
            apex_parser::parse_compilation_unit_with_cache_and_text(text, &mut cache)
        }
    }

    pub fn syntax(&self, file: FileId) -> SyntaxNode {
        match self.parses.get(&file) {
            Some(parse) => parse.syntax(),
            None => self.reparse_evicted(file).syntax(),
        }
    }

    /// `file`'s exact source text -- a reference fetch into `texts`
    /// (retained for every current file regardless of whether its `Parse`
    /// is still in `BindCache::file_parses`, ticket 07), not a
    /// `SyntaxNode::text().to_string()` tree walk. Callers building a
    /// `LineIndex` (hover, completion, rename, semantic-tokens, ...) should
    /// use this instead of `self.syntax(file).text().to_string()`.
    pub fn source_text(&self, file: FileId) -> &str {
        &self.texts[&file]
    }

    /// Every `apex_parser::ParseError` recorded while parsing `file` --
    /// the parser's own "never panics on malformed input, always records
    /// an error plus a best-effort tree" guarantee
    /// (`apex_parser::errors`'s own module doc comment) means this is
    /// just surfacing data that already existed, not computing anything
    /// new. Empty for a file that was never parsed (or parsed cleanly).
    /// `Cow` (not a plain reference, unlike before) since a `Parse` evicted
    /// from `BindCache::file_parses` (ticket 07) falls back to
    /// [`Self::reparse_evicted`], a local temporary with nothing in `self`
    /// left to borrow the errors from -- real syntax errors on an evicted
    /// file must still be reported, never silently treated as "no errors."
    /// `Cow::Borrowed` for the common (not-evicted) case avoids cloning the
    /// error list on every call just to satisfy the rare fallback branch's
    /// own need for an owned value.
    pub fn syntax_errors(&self, file: FileId) -> std::borrow::Cow<'_, [ParseError]> {
        match self.parses.get(&file) {
            Some(parse) => std::borrow::Cow::Borrowed(&parse.errors),
            None => std::borrow::Cow::Owned(self.reparse_evicted(file).errors),
        }
    }

    /// Every provable type-checking defect found inline while binding
    /// `file`'s bodies -- see [`resolve::TypeMismatch`]'s own doc comment
    /// (Wayfinder `apex-diagnostics` map, ticket 23). Empty for a file
    /// with no bound bodies at all, matching every other `self.bodies`-backed
    /// lookup's "nothing recorded" behavior.
    pub fn type_mismatches(&self, file: FileId) -> &[resolve::TypeMismatch] {
        self.bodies.get(&file).map_or(&[], |fb| &fb.type_mismatches)
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// What `ptr` (a reference `SyntaxPtr` -- a `NameExpr`, `FieldExpr`,
    /// SOQL field name, ...) resolved to during Pass 2, if anything was
    /// ever recorded for it.
    pub fn resolution(&self, ptr: SyntaxPtr) -> Option<&Resolution> {
        self.bodies.get(&ptr.file())?.refs.get(ptr)
    }

    /// The range `documentHighlight`/`references`/`rename` should report
    /// for `ptr`. Three tiers, cheapest/most-common first:
    /// 1. A narrow range eagerly recorded via `set_with_highlight`
    ///    (`ReferenceTable::stored_highlight_range`) -- a call/field-
    ///    access-shaped reference, where the node spans well past the
    ///    identifier (target through closing paren, say).
    /// 2. Otherwise, computed here on demand for a reference kind whose
    ///    node range is only ever off by trailing trivia (`NameExpr`/
    ///    `Type`/`QualifiedName` -- see `crate::resolve::bind_name_expr`'s
    ///    doc comment for why this is computed lazily, per query, rather
    ///    than eagerly stored the same way tier 1 is: these three kinds
    ///    are common enough in real code that eager storage measurably
    ///    regressed bind time).
    /// 3. Otherwise (or when `ptr`'s file has no bound body at all,
    ///    matching every other `bodies.get(...)`-backed lookup's
    ///    "nothing recorded" behavior), just `ptr.range()` itself --
    ///    already exactly the identifier for every remaining reference
    ///    kind (`ThisExpr`/`SuperExpr`/a `SoqlFieldName`'s own token-keyed
    ///    entries).
    pub fn highlight_range(&self, ptr: SyntaxPtr) -> rowan::TextRange {
        let Some(fb) = self.bodies.get(&ptr.file()) else {
            return ptr.range();
        };
        if let Some(stored) = fb.refs.stored_highlight_range(ptr) {
            return stored;
        }
        self.tight_range_for(ptr).unwrap_or_else(|| ptr.range())
    }

    /// Tier 2 of [`Self::highlight_range`]: for a reference `SyntaxPtr`
    /// whose node range can include trailing trivia, re-resolves it
    /// against the live tree and returns just the identifier's own
    /// token(s) -- `None` when `ptr`'s kind isn't one of these, or it
    /// fails to re-resolve (defensive; shouldn't happen against this
    /// file's own current tree).
    fn tight_range_for(&self, ptr: SyntaxPtr) -> Option<rowan::TextRange> {
        use apex_syntax::ast::expr::NameExpr;
        use apex_syntax::ast::{QualifiedName, Type};
        use apex_syntax::SyntaxKind;

        let node = ptr.to_node(&self.syntax(ptr.file()))?;
        match ptr.kind() {
            SyntaxKind::NameExpr => Some(NameExpr::cast(node)?.name_token()?.text_range()),
            SyntaxKind::Type => Type::cast(node)?
                .base_name_tokens()
                .last()
                .map(|t| t.text_range()),
            SyntaxKind::QualifiedName => {
                Some(QualifiedName::cast(node)?.last_token()?.text_range())
            }
            _ => None,
        }
    }

    /// Finds the reference (if any) covering `offset` in `file` -- first
    /// checks whether the token itself has its own recorded `Resolution`
    /// (only ever true for one segment of a qualified `Outer.Inner` type
    /// reference, `resolve::record_qualified_segments` -- see that
    /// function's doc comment for why a bare token, not a node, is the
    /// only way to disambiguate which segment the cursor is on), then
    /// falls back to walking up through ancestors until the first node
    /// whose kind is one Pass 2 actually registers a `Resolution` for.
    /// Confirmed exhaustive by reading every `refs.set(...)` call site in
    /// `resolve.rs`/`soql.rs`: exactly `NameExpr`, `FieldExpr`, `Type`,
    /// `QualifiedName` (a catch clause's exception type), `MethodCallExpr`,
    /// `CallExpr`, `NewExpr`, `SoqlFieldName`, `ThisExpr`, and `SuperExpr`
    /// ever get registered at the node level, each keyed by its *whole*
    /// node range. `FieldExpr` in particular is keyed by the entire `a.b`
    /// (receiver included, not just the member) -- this still resolves
    /// `a` and `b` independently without any special-casing, since `a`
    /// (when itself a simple name) has its own, smaller, closer `NameExpr`
    /// ancestor, reached by the walk-up before it ever gets to `FieldExpr`;
    /// `b` has no node of its own, so climbing from its token lands
    /// directly on the enclosing `FieldExpr`. `ThisExpr`/`SuperExpr` are
    /// the same story for `this.member`/`super.member`: `this`/`super`
    /// each have their own smaller, closer node (resolving to the
    /// enclosing type / its direct `extends` target respectively),
    /// reached before the walk-up ever gets to the enclosing
    /// `FieldExpr`/`MethodCallExpr` -- without these two, clicking `this`
    /// or `super` itself (not the member after the dot) fell through to
    /// that enclosing node instead, landing on the member being accessed.
    pub fn resolution_at(&self, file: FileId, offset: rowan::TextSize) -> Option<&Resolution> {
        const REFERENCE_KINDS: [apex_syntax::SyntaxKind; 10] = [
            apex_syntax::SyntaxKind::NameExpr,
            apex_syntax::SyntaxKind::FieldExpr,
            apex_syntax::SyntaxKind::Type,
            apex_syntax::SyntaxKind::QualifiedName,
            apex_syntax::SyntaxKind::MethodCallExpr,
            apex_syntax::SyntaxKind::CallExpr,
            apex_syntax::SyntaxKind::NewExpr,
            apex_syntax::SyntaxKind::SoqlFieldName,
            apex_syntax::SyntaxKind::ThisExpr,
            apex_syntax::SyntaxKind::SuperExpr,
        ];
        let root = self.syntax(file);
        let token = match root.token_at_offset(offset) {
            rowan::TokenAtOffset::None => return None,
            rowan::TokenAtOffset::Single(t) => t,
            // Cursor sits exactly between two tokens -- whichever side is
            // an actual `Identifier` token wins, regardless of which
            // side it's on: that's unambiguously the name the cursor is
            // "on," whether the cursor is at an identifier's start (right
            // after an open paren/comma/brace with no space, e.g.
            // `foo(bar)`'s `bar`) or its end (right before a dot/semi/
            // close-paren with no space, e.g. `other` in `other.field`).
            // A first cut here preferred left-when-non-trivia, which
            // happened to cover the "end of identifier" case but actively
            // broke the "start of identifier" one -- any identifier
            // immediately preceded by punctuation with no space (any
            // call argument, any brace-initializer element, ...) climbed
            // from the punctuation token instead, landing on whatever
            // enclosing expression *that* belonged to. Falls back to the
            // old trivia-based rule only for the (practically unreachable
            // for an identifier boundary) case where neither or both
            // sides are `Identifier` tokens.
            rowan::TokenAtOffset::Between(left, right) => {
                match (
                    left.kind() == apex_syntax::SyntaxKind::Identifier,
                    right.kind() == apex_syntax::SyntaxKind::Identifier,
                ) {
                    (true, false) => left,
                    (false, true) => right,
                    _ => {
                        if left.kind().is_trivia() {
                            right
                        } else {
                            left
                        }
                    }
                }
            }
        };
        // A dynamic-SOQL bind variable (`:nameVar`) embedded in a string
        // literal's own *text content* (`resolve::bind_dynamic_soql_binds`)
        // has no real syntax node/token of its own -- the lexer never
        // tokenizes anything inside a string literal's body -- so a click
        // landing inside one can never be found via the node-climbing walk
        // below (which only ever matches a whole token or node's exact
        // range). Checked first, cheaply gated on the clicked token
        // actually being a string literal (true for none of the many
        // other tokens a click can land on), against the small per-token
        // span index `ReferenceTable::dynamic_soql_bind_at` builds, rather
        // than re-scanning the token's text here.
        if matches!(
            token.kind(),
            apex_syntax::SyntaxKind::StringLiteral | apex_syntax::SyntaxKind::MultilineStringLiteral
        ) {
            if let Some(fb) = self.bodies.get(&file) {
                let container = SyntaxPtr::for_token(file, &token);
                if let Some(res) = fb.refs.dynamic_soql_bind_at(container, offset) {
                    return Some(res);
                }
            }
        }
        // A qualified `Outer.Inner` type reference (`resolve::record_qualified_segments`)
        // is the one case a bare *token* -- not just a node -- can have
        // its own recorded `Resolution`: the whole dotted path is a
        // single flat `Type` node, so `Outer` and `Inner` have no node
        // of their own to disambiguate through the climb below. Checked
        // first, before climbing to any ancestor node, so it takes
        // priority whenever present; every other reference kind never
        // populates a token-shaped key, so this is a no-op for them.
        if let Some(res) = self.resolution(SyntaxPtr::for_token(file, &token)) {
            return Some(res);
        }
        let mut node = token.parent()?;
        loop {
            if REFERENCE_KINDS.contains(&node.kind()) {
                let ptr = SyntaxPtr::new(file, &node);
                return self.resolution(ptr);
            }
            node = node.parent()?;
        }
    }

    /// Finds a `Symbol` declared in `file` whose own name occupies
    /// `offset` -- the declaration-site counterpart to
    /// [`Self::resolution_at`]: hovering a declaration's own name (the
    /// `Foo` in `class Foo`) is never itself a reference recorded in
    /// `ReferenceTable` (a declaration doesn't reference itself), so
    /// answering "what's the user looking at" there needs a direct scan
    /// of this file's own declared symbols instead. Includes locals
    /// (parameters, block-local variables, ...), same as
    /// `symbols_of_file` always has -- hovering a local's own declared
    /// name is exactly as valid a query as hovering a field's.
    pub fn symbol_at(&self, file: FileId, offset: rowan::TextSize) -> Option<SymbolId> {
        self.symbols
            .symbols_of_file(file)
            .iter()
            .enumerate()
            .find(|(_, symbol)| symbol.name_range.contains(offset))
            .map(|(local, _)| SymbolId::new(file, local as u32))
    }

    /// Every symbol declared in `file` -- an O(1) per-file accessor
    /// (`SymbolTable::symbols_of_file`) instead of flat-mapping
    /// `SymbolTable::iter()` over every file in the project and filtering
    /// by `s.file == file`, which costs O(project symbols) instead of
    /// O(file symbols). Matches [`Self::resolutions_in_file`]'s shape.
    pub fn symbols_in_file(&self, file: FileId) -> &[Symbol] {
        self.symbols.symbols_of_file(file)
    }

    /// Every reference's `SyntaxPtr` and its `Resolution`, across every
    /// file -- the whole-project view `Self::resolution` doesn't give
    /// one lookup at a time. Costs time proportional to the whole
    /// project to iterate in full, same as it always did; unlike
    /// `Self::resolution`, there's no way to avoid that for a query that
    /// is, by definition, asking for everything.
    pub fn all_resolutions(&self) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)> {
        self.bodies.values().flat_map(|fb| fb.refs.iter())
    }

    /// Like [`Self::all_resolutions`], but scoped to one file -- the
    /// per-open-file granularity `textDocument/publishDiagnostics`
    /// actually needs (matching `Self::syntax_errors`/`dead_symbols_in_file`'s
    /// existing per-file posture) instead of iterating the whole project
    /// for every file a client happens to have open. Empty for a file with
    /// no bound body, matching every other `self.bodies.get(...)`-backed
    /// lookup's "nothing recorded" behavior.
    pub fn resolutions_in_file(&self, file: FileId) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)> {
        self.bodies.get(&file).into_iter().flat_map(|fb| fb.refs.iter())
    }

    /// Every reference (project-wide) whose `Resolution` touches `id` --
    /// `textDocument/references`'s primitive. Backed by each file's own
    /// `ReferenceTable::references_to`, an O(1) hash lookup per file
    /// (`reference_table.rs`'s `by_symbol` reverse index), not a scan --
    /// still O(files) to check every file (a reference can live in any
    /// file, not just `id`'s declaring one), but each file's own
    /// contribution no longer costs O(references in that file).
    pub fn references_to(&self, id: SymbolId) -> impl Iterator<Item = SyntaxPtr> + '_ {
        self.bodies
            .values()
            .flat_map(move |fb| fb.refs.references_to(id).iter().copied())
    }

    /// Like [`Self::references_to`], scoped to one file -- the cheaper
    /// path `textDocument/documentHighlight` uses, since a highlight
    /// request only ever cares about the currently-open file.
    pub fn references_to_in_file(
        &self,
        file: FileId,
        id: SymbolId,
    ) -> impl Iterator<Item = SyntaxPtr> + '_ {
        self.bodies
            .get(&file)
            .into_iter()
            .flat_map(move |fb| fb.refs.references_to(id).iter().copied())
    }

    /// Every reference (project-wide) whose `Resolution` shares `key` --
    /// the `ExternalKey` counterpart of `Self::references_to`, for a
    /// `SchemaObject`/`UnknownSchema`/`StdlibMember` reference, none of
    /// which have a `SymbolId` to look up by instead.
    pub fn references_to_external<'a>(
        &'a self,
        key: &'a ExternalKey,
    ) -> impl Iterator<Item = SyntaxPtr> + 'a {
        self.bodies
            .values()
            .flat_map(move |fb| fb.refs.references_to_external(key).iter().copied())
    }

    /// Like [`Self::references_to_external`], scoped to one file -- the
    /// cheaper path `textDocument/documentHighlight` uses.
    pub fn references_to_external_in_file<'a>(
        &'a self,
        file: FileId,
        key: &'a ExternalKey,
    ) -> impl Iterator<Item = SyntaxPtr> + 'a {
        self.bodies
            .get(&file)
            .into_iter()
            .flat_map(move |fb| fb.refs.references_to_external(key).iter().copied())
    }

    pub fn scope_tree(&self, block: SyntaxPtr) -> Option<&ScopeTree> {
        self.bodies.get(&block.file())?.scopes.get(&block)
    }

    /// Every reference in `file` whose own node lies fully within
    /// `range`, restricted to the three reference kinds that are ever a
    /// *call* (`MethodCallExpr`/`CallExpr`/`NewExpr` -- confirmed
    /// exhaustive by the same read of every `refs.set(...)` call site
    /// `Self::resolution_at`'s doc comment already did; `this(...)`/
    /// `super(...)` are `CallExpr`, not a separate kind). The primitive
    /// `crate::call_hierarchy::outgoing_calls` needs "what does this
    /// method's body call," not every reference in it -- a field access,
    /// a local, a type name, none of which is a call.
    pub fn call_sites_in_range(
        &self,
        file: FileId,
        range: rowan::TextRange,
    ) -> Vec<(SyntaxPtr, Resolution)> {
        const CALL_KINDS: [apex_syntax::SyntaxKind; 3] = [
            apex_syntax::SyntaxKind::MethodCallExpr,
            apex_syntax::SyntaxKind::CallExpr,
            apex_syntax::SyntaxKind::NewExpr,
        ];
        let Some(fb) = self.bodies.get(&file) else {
            return Vec::new();
        };
        fb.refs
            .iter()
            .filter(|(ptr, _)| CALL_KINDS.contains(&ptr.kind()) && range.contains_range(ptr.range()))
            .map(|(ptr, res)| (*ptr, res.clone()))
            .collect()
    }

    /// The nearest enclosing `Method`/`Constructor` symbol whose
    /// declaration contains `offset` in `file` -- `crate::call_hierarchy::incoming_calls`'s
    /// "who called this, from where" primitive: each call site found via
    /// `Self::references_to` needs mapping back to whichever callable
    /// it's physically inside, to serve as the caller `CallHierarchyItem`.
    /// `None` when `offset` isn't inside a method/constructor body at
    /// all -- a field/property initializer (Apex allows a call there
    /// too, e.g. `private static Integer x = computeSomething();`, but
    /// there's no enclosing callable to honestly report it from).
    pub fn enclosing_callable(&self, file: FileId, offset: rowan::TextSize) -> Option<SymbolId> {
        let root = self.syntax(file);
        let token = match root.token_at_offset(offset) {
            rowan::TokenAtOffset::None => return None,
            rowan::TokenAtOffset::Single(t) => t,
            rowan::TokenAtOffset::Between(left, right) => {
                if left.kind().is_trivia() {
                    right
                } else {
                    left
                }
            }
        };
        let decl = token.parent()?.ancestors().find(|n| {
            matches!(
                n.kind(),
                apex_syntax::SyntaxKind::MethodDecl | apex_syntax::SyntaxKind::ConstructorDecl
            )
        })?;
        let range = decl.text_range();
        self.symbols
            .symbols_of_file(file)
            .iter()
            .enumerate()
            .find(|(_, s)| {
                matches!(s.kind, SymbolKind::Method | SymbolKind::Constructor) && s.ptr.range() == range
            })
            .map(|(local, _)| SymbolId::new(file, local as u32))
    }

    /// Whether `file` currently has Pass 2 (body/reference) data bound in
    /// this snapshot -- `false` for a declaration-only file outside the
    /// Pass 2 working set (ticket 04, `.scratch/apex-memory/`), or for a
    /// file this snapshot doesn't know about at all.
    pub fn is_bound(&self, file: FileId) -> bool {
        self.bodies.contains_key(&file)
    }

    /// Whether *every* currently-known file has Pass 2 data bound --
    /// unconditionally `true` for a caller that never turned on working-set
    /// scoping (`BindCache::scoping_enabled`, ticket 04), since Pass 2 then
    /// always covers the whole project; under scoping, `true` only when the
    /// working set happens to cover every file. Consulted by any analysis
    /// that needs to *prove an absence* project-wide (dead code, visibility
    /// narrowing) via `Self::references_to`, since a scoped snapshot's
    /// `references_to` can only prove presence, never absence, for a symbol
    /// whose visibility could reach outside its own file.
    pub fn is_fully_bound(&self) -> bool {
        self.bodies.len() >= self.file_count()
    }

    /// Ticket 04's on-demand promotion: if `file` isn't part of this
    /// snapshot's Pass 2 working set yet (no `bodies` entry), synchronously
    /// binds just that one file now, reusing `cache`'s already-resident
    /// Pass 1 state (declarations, inheritance, supertype pointers) --
    /// the ~80KB/file cost ticket 03 measured, not a project-wide rebuild.
    /// Also records the promotion in `cache` (LRU-capped,
    /// `BindCache::note_promoted`) so it survives into the *next*
    /// background rebuild's snapshot instead of silently reverting on the
    /// next edit -- and, when that promotion pushes the cache over its
    /// cap, evicts the same displaced file from *this* snapshot's own
    /// `bodies` too, so a single long-lived snapshot visited into more
    /// than the cap's worth of distinct non-open files (many cross-file
    /// jumps between rebuilds) can't grow unbounded just because the
    /// cache-side cap alone only bounds the *next* rebuild's snapshot. A
    /// no-op (`false`) if `file` is already bound, or isn't a file this
    /// snapshot knows about at all.
    pub fn ensure_bound(&mut self, file: FileId, cache: &mut BindCache) -> bool {
        if self.bodies.contains_key(&file) || !self.files.contains_key(&file) {
            return false;
        }
        let Some(bodies) =
            cache.bind_pass2_for_file(file, &self.schema, &self.stdlib, &self.labels, &self.pages)
        else {
            return false;
        };
        if let Some(evicted) = cache.note_promoted(file) {
            self.bodies.remove(&evicted);
        }
        self.bodies.insert(file, bodies);
        true
    }

    /// Ticket 04's temporary full-project Pass 2 spike: `find-all-references`,
    /// `rename`, and call-hierarchy's `incomingCalls` are the only
    /// capabilities needing every file's `bodies` at once
    /// (`Self::references_to` iterates `self.bodies.values()`
    /// project-wide) -- binds whatever this snapshot's working set left
    /// unbound, runs `f` against the now fully-bound snapshot, then evicts
    /// exactly what this call itself added (from both this snapshot and
    /// `cache`) so the steady-state working-set-only footprint returns
    /// right after. A file already legitimately in the working set (open,
    /// or LRU-promoted) is untouched by the eviction. A no-op spike when
    /// scoping was never turned on (`cache.scoping_enabled` false, e.g. the
    /// CLI/tests): every file is already bound, so nothing gets added or
    /// evicted.
    pub fn with_full_binding<R>(
        &mut self,
        cache: &mut BindCache,
        f: impl FnOnce(&BoundProgram) -> R,
    ) -> R {
        let files: Vec<FileId> = self.files().collect();
        let mut newly_bound = Vec::new();
        for file in files {
            if !self.bodies.contains_key(&file) {
                if let Some(bodies) = cache.bind_pass2_for_file(
                    file,
                    &self.schema,
                    &self.stdlib,
                    &self.labels,
                    &self.pages,
                ) {
                    self.bodies.insert(file, bodies);
                    newly_bound.push(file);
                }
            }
        }
        let result = f(self);
        for file in newly_bound {
            self.bodies.remove(&file);
            cache.bodies.remove(&file);
        }
        result
    }
}

impl BindCache {
    /// Synchronously Pass-2-binds `file` alone, reusing this cache's
    /// already-resident Pass 1 state (`self.table`'s declarations/
    /// inheritance, `self.supertype_ptrs`) instead of re-walking the
    /// project -- ticket 04's (`.scratch/apex-memory/`) single-file
    /// on-demand-promotion and full-project-spike primitive, shared by
    /// `BoundProgram::ensure_bound`/`with_full_binding`. `None` if `file`
    /// isn't a known file at all (nothing declared for it in `self.table`)
    /// or its text input can't be recovered.
    pub(crate) fn bind_pass2_for_file(
        &mut self,
        file: FileId,
        schema: &SchemaIndex,
        stdlib: &StdlibIndex,
        labels: &LabelIndex,
        pages: &PageIndex,
    ) -> Option<Arc<FileBodies>> {
        if !self.table.has_file(file) {
            return None;
        }
        let parse = match self.file_parses.get(&file) {
            Some(parse) => parse.clone(),
            None => {
                let input = *self.file_text_inputs.get(&file)?;
                let parse = db::parse_query(&self.db, input);
                self.file_parses.insert(file, parse.clone());
                parse
            }
        };
        self.parse_generation += 1;
        self.parse_last_used.insert(file, self.parse_generation);
        let root = parse.syntax();

        let to_bind: Vec<(SymbolId, &Symbol)> = self
            .table
            .symbols_of_file(file)
            .iter()
            .enumerate()
            .map(|(local, symbol)| (SymbolId::new(file, local as u32), symbol))
            .collect();
        let mut bound: Vec<(Option<SyntaxPtr>, resolve::BoundBody)> = to_bind
            .iter()
            .flat_map(|(id, symbol)| {
                bind_symbol_body(&self.table, schema, stdlib, labels, pages, &root, *id, symbol)
            })
            .collect();
        for (owner, ptr) in self.supertype_ptrs.get(&file).into_iter().flatten() {
            if let Some(ty) = ptr.to_node(&root) {
                bound.push((
                    None,
                    resolve::bind_type_ref(&self.table, schema, stdlib, file, Some(*owner), &ty),
                ));
            }
        }

        let file_bodies = Arc::new(merge_pass2_results(&mut self.table, file, bound));
        self.bodies.insert(file, Arc::clone(&file_bodies));
        Some(file_bodies)
    }
}

/// Folds one file's freshly-bound Pass 2 output (`bodies`, one entry per
/// declared symbol's body/initializer/supertype -- see `bind_symbol_body`/
/// `resolve::bind_type_ref`) into `table` and a fresh [`FileBodies`],
/// allocating each body's pending locals onto the end of `file`'s own
/// declared symbols and remapping each body's sentinel ids to the
/// resulting real ids as it goes (a running per-file base, so sibling
/// bodies bound concurrently in the same file don't collide over the same
/// local-id range). Shared by `BoundProgram::from_files_cached`'s main
/// per-file merge loop and `BindCache::bind_pass2_for_file`'s single-file
/// on-demand path (ticket 04, `.scratch/apex-memory/`) -- both need
/// exactly this same remap-and-append dance, just for a different-sized
/// `files_to_rebind`.
fn merge_pass2_results(
    table: &mut SymbolTable,
    file: FileId,
    bodies: Vec<(Option<SyntaxPtr>, resolve::BoundBody)>,
) -> FileBodies {
    let mut base = table.declared_len(file) as u32;
    // Pre-sized rather than growing via repeated `.extend()`/`.insert()`
    // calls as each body is folded in below -- `scopes`' bound
    // (`bodies.len()`) is an upper bound, not exact (not every body has a
    // `Some(key)`), but a small over-reservation beats the reallocations
    // this file's bodies would otherwise cause one at a time. See the
    // `hotpath`-measured finding in `BACKLOG.md` §2 this targets.
    let mut extra_symbols =
        Vec::with_capacity(bodies.iter().map(|(_, body)| body.pending_locals.len()).sum());
    let mut file_bodies = FileBodies {
        refs: ReferenceTable::default(),
        scopes: FxHashMap::with_capacity_and_hasher(bodies.len(), Default::default()),
        type_mismatches: Vec::new(),
    };
    for (key, body) in bodies {
        let remap = |id: SymbolId| resolve::remap_local_id(id, base);
        let mut scopes = body.scopes;
        scopes.remap_symbol_ids(&remap);
        body.refs.map_ids_into(&remap, &mut file_bodies.refs);
        if let Some(key) = key {
            file_bodies.scopes.insert(key, scopes);
        }
        file_bodies.type_mismatches.extend(body.type_mismatches);
        base += body.pending_locals.len() as u32;
        extra_symbols.extend(body.pending_locals);
    }
    table.append_file_symbols(file, extra_symbols);
    file_bodies
}

/// Compares two versions of one file's Pass 1 output by declared
/// *shape* only (`kind`/`name`/`container`/`type_name`/`modifiers`),
/// deliberately ignoring `ptr`/`name_range`/`type_ref` -- all three are
/// byte-range-based, and *any* edit earlier in a file shifts every later
/// declaration's range even when nothing about what's declared actually
/// changed. Comparing those too would make `BoundProgram::from_files_cached`'s
/// fast Pass 2 path fire only for edits at the very end of a file,
/// defeating the point. `container`'s `SymbolId` is safe to compare
/// directly despite embedding no range itself: it only depends on
/// declaration order/count within the file, which Pass 1 never looks at
/// body content to determine.
fn declarations_equivalent(old: &[Symbol], new: &[Symbol]) -> bool {
    old.len() == new.len()
        && old.iter().zip(new).all(|(a, b)| {
            a.kind == b.kind
                && a.name == b.name
                && a.container == b.container
                && a.type_name == b.type_name
                && a.modifiers == b.modifiers
        })
}

/// Binds one declared symbol's body/initializer, if it has one --
/// almost always zero or one result, except a `Property` (0-2, one per
/// accessor with a real body) and a `Trigger` (0-2: its `ON <object>`
/// reference and its executable top-level block are two independent
/// `BoundBody`s). The `Option<SyntaxPtr>` is `Some` only for a result
/// that's a real body worth keeping a `ScopeTree` for (`None` for the
/// trigger's bare object-name resolution, which has no scope of its
/// own).
/// Walks `symbol.container` up until it lands on a type-kind symbol
/// (`Class`/`Interface`/`Enum`) -- for a `Field`/`Property`/`Method`
/// that's the immediate container already; for a `Parameter`, whose
/// immediate container is the `Method`/`Constructor` it belongs to,
/// this climbs one level further. The scope an unqualified type
/// reference's fallback nested-type lookup (`resolve::resolve_type_ref`)
/// should search from.
pub(crate) fn enclosing_type_of(table: &SymbolTable, symbol: &Symbol) -> Option<SymbolId> {
    let mut current = symbol.container?;
    loop {
        match table.get(current).kind {
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => return Some(current),
            _ => current = table.get(current).container?,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn bind_symbol_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    labels: &LabelIndex,
    pages: &PageIndex,
    root: &SyntaxNode,
    id: SymbolId,
    symbol: &Symbol,
) -> Vec<(Option<SyntaxPtr>, resolve::BoundBody)> {
    // A field/property/parameter's own type, or a method's return type
    // -- shared by several arms below, so factored out once. `None` for
    // a symbol kind with no type of its own (`Symbol::type_ref`'s own
    // doc comment), or when the file's tree has moved on since this
    // pointer was captured (defensive; shouldn't happen mid-call).
    let enclosing_type = enclosing_type_of(table, symbol);
    let declared_type = || {
        symbol
            .type_ref
            .and_then(|type_ref| type_ref.to_node(root))
            .map(|ty| {
                (
                    None,
                    resolve::bind_type_ref(table, schema, stdlib, symbol.file, enclosing_type, &ty),
                )
            })
    };
    match symbol.kind {
        SymbolKind::Method => {
            let mut out: Vec<(Option<SyntaxPtr>, resolve::BoundBody)> =
                declared_type().into_iter().collect();
            if let Some(body) = symbol
                .ptr
                .to_node(root)
                .and_then(MethodDecl::cast)
                .and_then(|m| m.body())
            {
                let params = table.params(id);
                let key = SyntaxPtr::new(symbol.file, body.syntax());
                let bound = resolve::bind_body(
                    table,
                    schema,
                    stdlib,
                    labels,
                    pages,
                    symbol.file,
                    symbol.container,
                    Some(id),
                    &params,
                    &body,
                );
                out.push((Some(key), bound));
            }
            out
        }
        SymbolKind::Constructor => {
            let Some(c) = symbol.ptr.to_node(root).and_then(ConstructorDecl::cast) else {
                return Vec::new();
            };
            let Some(body) = c.body() else {
                return Vec::new();
            };
            let params = table.params(id);
            let key = SyntaxPtr::new(symbol.file, body.syntax());
            let bound = resolve::bind_body(
                table,
                schema,
                stdlib,
                labels,
                pages,
                symbol.file,
                symbol.container,
                Some(id),
                &params,
                &body,
            );
            vec![(Some(key), bound)]
        }
        SymbolKind::Property => {
            let mut out: Vec<(Option<SyntaxPtr>, resolve::BoundBody)> =
                declared_type().into_iter().collect();
            // A `set` accessor's own implicit `value` parameter is
            // collected under the property's own id as its container
            // (`crate::collect::collect_property`) specifically so
            // `table.params` -- which already filters `members_of` down
            // to `Parameter` kind -- finds it here the same way an
            // ordinary method body finds its real parameters. A `get`
            // accessor has none, so this is empty for it.
            let value_param = table.params(id);
            if let Some(p) = symbol.ptr.to_node(root).and_then(PropertyDecl::cast) {
                out.extend(p.accessors().filter_map(|accessor| {
                    let body = accessor.body()?;
                    let key = SyntaxPtr::new(symbol.file, body.syntax());
                    let params: &[SymbolId] = if accessor.is_setter() { &value_param } else { &[] };
                    let bound = resolve::bind_body(
                        table,
                        schema,
                        stdlib,
                        labels,
                        pages,
                        symbol.file,
                        symbol.container,
                        None,
                        params,
                        &body,
                    );
                    Some((Some(key), bound))
                }));
            }
            out
        }
        SymbolKind::Field => {
            let mut out: Vec<(Option<SyntaxPtr>, resolve::BoundBody)> =
                declared_type().into_iter().collect();
            if let Some(init) = symbol
                .ptr
                .to_node(root)
                .and_then(VarDeclarator::cast)
                .and_then(|decl| decl.init())
            {
                let bound = resolve::bind_initializer(
                    table,
                    schema,
                    stdlib,
                    labels,
                    pages,
                    symbol.file,
                    symbol.container,
                    &init,
                );
                out.push((None, bound));
            }
            out
        }
        SymbolKind::Parameter => declared_type().into_iter().collect(),
        SymbolKind::Trigger => {
            let Some(tu) = symbol.ptr.to_node(root).and_then(TriggerUnit::cast) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if let Some(obj_tok) = tu.object_ref() {
                if let Some(parent) = obj_tok.parent() {
                    let bound = resolve::bind_object_ref(
                        schema,
                        SyntaxPtr::new(symbol.file, &parent),
                        obj_tok.text(),
                    );
                    out.push((None, bound));
                }
            }
            if let Some(block) = tu.block() {
                let key = SyntaxPtr::new(symbol.file, block.syntax());
                let bound = resolve::bind_trigger_body(
                    table,
                    schema,
                    stdlib,
                    labels,
                    pages,
                    symbol.file,
                    Some(id),
                    &block,
                );
                out.push((Some(key), bound));
            }
            out
        }
        SymbolKind::Interface
        | SymbolKind::Class
        | SymbolKind::Enum
        | SymbolKind::EnumConstant
        | SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => Vec::new(),
    }
}

/// Ticket 04 (`.scratch/apex-memory/`): working-set scoping mechanics --
/// `BindCache::scoping_enabled`/`open_paths`/`note_promoted` and
/// `BoundProgram::is_bound`/`is_fully_bound`/`ensure_bound`/`with_full_binding`.
#[cfg(test)]
mod scoping_tests {
    use super::*;
    use std::collections::HashMap;

    fn write_fixture(test_name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apex-binder-scoping-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    /// Nothing in this map's design touches a caller that never sets
    /// `scoping_enabled` -- confirms `from_files_cached`'s default
    /// (`BindCache::default()`) still fully binds every file, matching the
    /// CLI/tests' existing expectations untouched by ticket 04.
    #[test]
    fn scoping_disabled_binds_every_file_regardless_of_open_paths() {
        let dir = write_fixture(
            "disabled",
            &[
                ("A.cls", "public class A { public void a() { } }"),
                ("B.cls", "public class B { public void b() { } }"),
            ],
        );
        let mut cache = BindCache::default();
        let program = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
        std::fs::remove_dir_all(&dir).ok();
        assert!(program.is_fully_bound());
    }

    /// The map's decisive behavior: with scoping on and only `A.cls`
    /// open, Pass 2 covers `A` alone -- `B` stays declaration-only -- while
    /// Pass 1 (declarations) stays project-wide for both, unaffected.
    #[test]
    fn scoping_enabled_restricts_pass2_to_open_files_only() {
        let dir = write_fixture(
            "enabled",
            &[
                ("A.cls", "public class A { public void a() { } }"),
                ("B.cls", "public class B { public void b() { } }"),
            ],
        );
        let mut cache = BindCache::default();
        cache.scoping_enabled = true;
        cache.open_paths = std::iter::once(dir.join("A.cls")).collect();
        let program = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
        let a = program.file_id(&dir.join("A.cls")).unwrap();
        let b = program.file_id(&dir.join("B.cls")).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(program.is_bound(a), "the open file must be Pass 2-bound");
        assert!(!program.is_bound(b), "the unopened file must stay declaration-only");
        assert!(!program.is_fully_bound());
        assert!(
            program.symbols.iter().any(|(_, s)| s.name == "B"),
            "Pass 1 declarations must stay project-wide even for an unopened file"
        );
    }

    /// A cross-file jump into `B` (goto-definition landing there, say)
    /// promotes it synchronously -- idempotent on a repeat call.
    #[test]
    fn ensure_bound_promotes_a_non_open_file_on_demand() {
        let dir = write_fixture(
            "promote",
            &[
                ("A.cls", "public class A { public void a() { } }"),
                ("B.cls", "public class B { public void b() { } }"),
            ],
        );
        let mut cache = BindCache::default();
        cache.scoping_enabled = true;
        cache.open_paths = std::iter::once(dir.join("A.cls")).collect();
        let mut program = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
        let b = program.file_id(&dir.join("B.cls")).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(!program.is_bound(b));
        assert!(program.ensure_bound(b, &mut cache), "promotion should bind B");
        assert!(program.is_bound(b));
        assert!(!program.ensure_bound(b, &mut cache), "already bound -- a no-op");
    }

    /// `note_promoted`'s LRU cap in isolation, via synthetic `FileId`s --
    /// no real project needed to prove the eviction arithmetic itself.
    #[test]
    fn note_promoted_evicts_the_least_recently_promoted_file_past_the_cap() {
        let mut cache = BindCache::default();
        for i in 0..=(incremental::PROMOTED_FILE_CAP as u32) {
            cache.note_promoted(FileId(i));
        }
        assert_eq!(cache.promoted_files.len(), incremental::PROMOTED_FILE_CAP);
        assert!(
            !cache.promoted_files.contains_key(&FileId(0)),
            "the least-recently-promoted file should have been evicted"
        );
        assert!(cache
            .promoted_files
            .contains_key(&FileId(incremental::PROMOTED_FILE_CAP as u32)));
    }

    /// `find-all-references`'s own primitive (`references_to`) is exactly
    /// what `with_full_binding` exists to make safe: a reference written in
    /// an unopened file (`B` calling `A::helper`) is invisible until the
    /// spike runs, present during it, and the spike leaves the
    /// working-set-only footprint restored afterward.
    #[test]
    fn with_full_binding_sees_every_reference_then_evicts_back_to_the_working_set() {
        let dir = write_fixture(
            "spike",
            &[
                ("A.cls", "public class A { public void helper() { } }"),
                ("B.cls", "public class B { public void go() { new A().helper(); } }"),
            ],
        );
        let mut cache = BindCache::default();
        cache.scoping_enabled = true;
        cache.open_paths = std::iter::once(dir.join("A.cls")).collect();
        let mut program = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
        let a = program.file_id(&dir.join("A.cls")).unwrap();
        let b = program.file_id(&dir.join("B.cls")).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let helper = program
            .symbols
            .iter()
            .find(|(_, s)| s.name == "helper")
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(
            program.references_to(helper).count(),
            0,
            "B isn't bound yet, so its call site is invisible before the spike"
        );

        let count_during_spike =
            program.with_full_binding(&mut cache, |program| program.references_to(helper).count());
        assert_eq!(count_during_spike, 1, "the spike must see B's own call site");

        assert!(program.is_bound(a), "A was already in the working set, untouched by eviction");
        assert!(!program.is_bound(b), "B must be evicted back out once the spike finishes");
    }
}
