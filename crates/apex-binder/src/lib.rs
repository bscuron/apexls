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
mod conversions;
mod dead_code;
mod file_id;
mod file_table;
mod generics;
mod incremental;
mod inherit;
mod ptr;
mod reference_table;
mod resolve;
mod schema_index;
mod scope;
mod soql;
mod stdlib_index;
mod symbol;
mod symbol_table;
mod ty;

pub use call_hierarchy::{incoming_calls, is_callable, outgoing_calls, IncomingCall, OutgoingCall};
pub use dead_code::{kind_label, dead_symbols_in_file, DeadSymbol};
pub use file_id::FileId;
pub use incremental::BindCache;
pub use ptr::{AstPtr, SyntaxPtr};
pub use reference_table::{
    ExternalKey, ReferenceTable, Resolution, SchemaObjectRef, StdlibMemberRef, UnknownSchemaRef,
};
pub use schema_index::SchemaIndex;
pub use stdlib_index::StdlibIndex;
pub use scope::{Scope, ScopeId, ScopeKind, ScopeTree};
pub use symbol::{ModifierSet, Sharing, Symbol, SymbolId, SymbolKind, Visibility};
pub use symbol_table::SymbolTable;

use apex_parser::Parse;
use apex_syntax::ast::decl::{
    CompilationUnit, ConstructorDecl, MethodDecl, PropertyDecl, TriggerUnit, VarDeclarator,
};
use apex_syntax::SyntaxNode;
use incremental::{FileBodies, Freshness};
use rayon::prelude::*;
use rowan::ast::AstNode;
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;
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
    pub symbols: SymbolTable,
    /// `Arc`-wrapped for the same reason `symbols`/`bodies` are cheap to
    /// clone into each call's snapshot: `cache.schema` is only ever
    /// rebuilt when the directory walk itself is redone (see
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
/// no per-project staleness to track the way `BindCache.schema` has.
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
                let schema = Arc::new(SchemaIndex::from_discovery(&discovery));
                let vf_referenced_classes = Arc::new(
                    apex_metadata::visualforce::referenced_controller_classes(&discovery.page_files),
                );
                cache.discovery = Some(discovery);
                cache.schema = Some(schema);
                cache.vf_referenced_classes = Some(vf_referenced_classes);
            });
        }
        let discovery = cache.discovery.as_ref().unwrap();
        let schema = Arc::clone(cache.schema.as_ref().unwrap());
        let stdlib = global_stdlib_index();
        let vf_referenced_classes = Arc::clone(cache.vf_referenced_classes.as_ref().unwrap());

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

        // Stage 1a (parallel): read (or reuse an override's in-memory
        // content) and parse (or reuse a byte-identical cache hit's
        // already-built tree) every candidate file independently. A
        // candidate that fails to read (deleted since the walk that
        // produced it, a permissions race, ...) is silently dropped
        // here, exactly as it always was -- and that silent drop is
        // *also* this function's only signal that a file was removed
        // (see `current_ids` below), so a deletion self-corrects without
        // ever needing a fresh walk, only a genuinely new file does.
        struct ParsedFile {
            path: PathBuf,
            file: FileId,
            trigger: bool,
            dirty: bool,
            freshness: Freshness,
            parse: Parse,
        }
        // Every file `rayon` groups into the same fold segment (its own
        // adaptive work-stealing split, not a fixed size this crate
        // picks) shares one `NodeCache` instead of each file getting its
        // own, empty one: `apex_parser::parse_compilation_unit_with_cache`'s
        // doc comment explains why that matters (identical keyword/
        // punctuation/small-node text across files interns into one
        // `Arc`-shared allocation instead of a fresh one per file --
        // measured as a real, if modest, share of steady-state memory,
        // see `crates/apex-binder/examples/mem_profile.rs` and
        // `BACKLOG.md`). `fold` (not a hand-chosen chunk size fed through
        // `.chunks()`) is what makes this scale to any machine rather
        // than one tuned to a specific core count: a first attempt
        // pre-partitioned `candidates` into fixed 64-item chunks via
        // `.chunks(64).flat_map(...)`, which pays for materializing a
        // `Vec` per chunk *and* a `Vec` of per-chunk results before
        // flattening -- fine for the memory-dominated cold-project case,
        // but that fixed per-call overhead regressed the far more latency-
        // sensitive warm single-file-edit rebind by ~47% (measured via
        // `cargo bench -p apex-binder`), since nearly all of its ~1044
        // candidates never parse at all (a cache/stat hit) and pay pure
        // grouping overhead for no benefit. `fold`'s accumulator
        // (`(NodeCache, Vec<ParsedFile>)`, built up in place per segment)
        // has no such fixed cost -- confirmed via the same benchmark to
        // leave warm-rebind's timing within its usual run-to-run noise.
        // The `NodeCache` in a fold segment is discarded when that
        // segment's accumulator is consumed by `flat_map` below, so
        // (same as the rejected chunking approach) nothing about it can
        // grow unbounded across an editing session.
        let parse_one = |path: PathBuf, file: FileId, node_cache: &mut apex_parser::NodeCache| -> Option<ParsedFile> {
                    let trigger = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));

                    // An `overrides` entry (an unsaved editor buffer) has no
                    // filesystem metadata to stat -- content is already in
                    // memory (the caller supplied it, no disk I/O either
                    // way), so it's compared by content hash, same as
                    // before. In practice this is at most a handful of
                    // files per call (whatever's actually being edited).
                    // `path` moves into whichever `ParsedFile` this
                    // invocation actually returns (one `PathBuf` per
                    // candidate, not the extra clone an owned-vs-borrowed
                    // mismatch used to force here) -- see the `hotpath`-
                    // measured finding in `BACKLOG.md` §2 this targets.
                    if let Some(content) = overrides.get(&path) {
                        let freshness =
                            Freshness::ContentHash(incremental::content_fingerprint(content));
                        if let Some((cached, cached_parse)) = cache.parses.get(&path) {
                            if *cached == freshness {
                                return Some(ParsedFile {
                                    path,
                                    file,
                                    trigger,
                                    dirty: false,
                                    freshness,
                                    parse: cached_parse.clone(),
                                });
                            }
                        }
                        let parse = if trigger {
                            apex_parser::parse_trigger_unit_with_cache(content, node_cache)
                        } else {
                            apex_parser::parse_compilation_unit_with_cache(content, node_cache)
                        };
                        return Some(ParsedFile {
                            path,
                            file,
                            trigger,
                            dirty: true,
                            freshness,
                            parse,
                        });
                    }

                    // No override: stat the file *before* reading it -- a
                    // size+mtime match against the cached entry means the
                    // read (not just the reparse) can be skipped entirely,
                    // which is the common case for every file besides the
                    // one actually being edited. See `Freshness::Stat`'s
                    // doc comment for the honest staleness caveat this
                    // trades for that.
                    let metadata = std::fs::metadata(&path).ok()?;
                    let stat_freshness = metadata.modified().ok().map(|modified| Freshness::Stat {
                        len: metadata.len(),
                        modified,
                    });
                    if let Some(freshness) = &stat_freshness {
                        if let Some((cached, cached_parse)) = cache.parses.get(&path) {
                            if cached == freshness {
                                return Some(ParsedFile {
                                    path,
                                    file,
                                    trigger,
                                    dirty: false,
                                    freshness: freshness.clone(),
                                    parse: cached_parse.clone(),
                                });
                            }
                        }
                    }
                    let content = std::fs::read_to_string(&path).ok()?;
                    let parse = if trigger {
                        apex_parser::parse_trigger_unit_with_cache(&content, node_cache)
                    } else {
                        apex_parser::parse_compilation_unit_with_cache(&content, node_cache)
                    };
                    // `metadata().modified()` failing at all is rare and
                    // platform-dependent -- fall back to a content hash so
                    // this file still gets *some* freshness check next call,
                    // just not the read-skipping kind.
                    let freshness = stat_freshness.unwrap_or_else(|| {
                        Freshness::ContentHash(incremental::content_fingerprint(&content))
                    });
                    Some(ParsedFile {
                        path,
                        file,
                        trigger,
                        dirty: true,
                        freshness,
                        parse,
                    })
        };
        let parsed: Vec<ParsedFile> = hotpath::measure_block!("stage_1a_read_and_parse", {
            candidates
                .into_par_iter()
                // `rayon`'s default adaptive splitting favors near-perfect
                // load balance over grouping -- left alone, it split this
                // source finely enough that `fold`'s segments barely
                // grouped any files together at all (measured: almost no
                // memory win over no sharing whatsoever). `with_min_len`
                // is a workload property, not a machine-specific tuning
                // knob: it just says "don't bother splitting a group of
                // candidates smaller than this," and `rayon` still freely
                // decides *how many* such groups to make (as many as it
                // wants, capped by however many threads/cores the running
                // machine actually has) -- unlike a fixed chunk count or
                // count derived from `rayon::current_num_threads()`, nothing
                // here is tuned to any particular machine.
                .with_min_len(64)
                .fold(
                    || (apex_parser::NodeCache::default(), Vec::new()),
                    |(mut node_cache, mut out), (path, file)| {
                        if let Some(parsed_file) = parse_one(path, file, &mut node_cache) {
                            out.push(parsed_file);
                        }
                        (node_cache, out)
                    },
                )
                .flat_map(|(_, files)| files)
                .collect()
        });

        // Refresh the parse cache for dirty files only -- an unchanged
        // file's entry is already correct.
        for p in parsed.iter().filter(|p| p.dirty) {
            cache
                .parses
                .insert(p.path.clone(), (p.freshness.clone(), p.parse.clone()));
            cache.paths.insert(p.file, p.path.clone());
            cache.path_ids.insert(p.path.clone(), p.file);
            cache.file_parses.insert(p.file, p.parse.clone());
        }

        // The *actual* current file set is whatever was just
        // successfully read above, not `discovery`'s candidate list --
        // this is what makes a deleted file self-correct even when
        // `discovery` itself is stale (reused from an earlier call): it
        // simply isn't in `parsed`. A removal changes the project-wide
        // namespace just as much as an addition does, so it also forces
        // the conservative "declarations changed" path below.
        let current_ids: FxHashSet<FileId> = parsed.iter().map(|p| p.file).collect();
        let current_paths: FxHashSet<&Path> = parsed.iter().map(|p| p.path.as_path()).collect();

        let removed: Vec<FileId> = cache
            .table
            .known_files()
            .filter(|id| !current_ids.contains(id))
            .collect();
        let mut declarations_changed = !removed.is_empty();
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
        }
        cache
            .parses
            .retain(|path, _| current_paths.contains(path.as_path()));

        // Stage 1b (parallel): Pass 1 declaration collection, but only
        // for dirty files -- an unchanged file's declarations are still
        // exactly what `cache.table`/`cache.raw_extends`/`cache.raw_super`
        // already hold.
        let fresh: Vec<(FileId, collect::FileCollection)> =
            hotpath::measure_block!("pass1_collect_dirty_files", {
                parsed
                    .par_iter()
                    .filter(|p| p.dirty)
                    .map(|p| {
                        let root_node = p.parse.syntax();
                        let collection = if p.trigger {
                            TriggerUnit::cast(root_node.clone())
                                .map(|tu| collect::collect_trigger_unit(p.file, &tu))
                        } else {
                            CompilationUnit::cast(root_node.clone())
                                .map(|cu| collect::collect_compilation_unit(p.file, &cu))
                        }
                        .unwrap_or_default();
                        (p.file, collection)
                    })
                    .collect()
            });

        // Sequential merge: patch each dirty file's slice of the
        // persistent `SymbolTable`/Pass-1.5 inputs, tracking whether any
        // file's *declared shape* actually changed. Cheap (no parsing/
        // tree-walking left to do here, just moving already-built
        // `Symbol`s and a same-length field-by-field comparison).
        for (file, collection) in fresh {
            let is_new = !cache.table.has_file(file);
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
        }

        // Pass 1.5 + derived-index rebuild only when something actually
        // declared changed project-wide -- otherwise every index and
        // `inherited_chain`/`direct_super` entry is still exactly
        // correct from the previous call (see `SymbolTable::rebuild_indices`'s
        // doc comment for why skipping this is sound, not just fast).
        if declarations_changed {
            hotpath::measure_block!("pass1_5_inherit_and_rebuild_indices", {
                cache.table.rebuild_indices();
                let all_raw_extends: Vec<(SymbolId, Vec<SmolStr>)> =
                    cache.raw_extends.values().flatten().cloned().collect();
                let all_raw_super: Vec<(SymbolId, SmolStr)> =
                    cache.raw_super.values().flatten().cloned().collect();
                inherit::resolve_inheritance(&mut cache.table, &all_raw_extends, &all_raw_super);
            });
        }

        // Pass 2 (parallel): rebind every current file if declarations
        // changed anywhere (conservative fallback, identical cost to a
        // full rebuild), or just the dirty files otherwise -- see this
        // method's doc comment.
        // Keyed by `&Parse` (`Sync`, safe to share across threads), not
        // by an already-built `SyntaxNode` (rowan's tree is `Rc`-based,
        // so `SyntaxNode` itself is neither `Send` nor `Sync`) -- each
        // parallel closure below calls `.syntax()` itself to build its
        // own thread-local node from the shared `Parse`.
        let parse_by_file: FxHashMap<FileId, &Parse> =
            parsed.iter().map(|p| (p.file, &p.parse)).collect();
        let files_to_rebind: Vec<FileId> = if declarations_changed {
            current_ids.iter().copied().collect()
        } else {
            parsed.iter().filter(|p| p.dirty).map(|p| p.file).collect()
        };
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
                        bind_symbol_body(&cache.table, &schema, &stdlib, &root_node, *id, symbol)
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
                let mut base = cache.table.declared_len(file) as u32;
                // Pre-sized rather than growing via repeated `.extend()`/
                // `.insert()` calls as each body is folded in below --
                // `scopes`' bound (`bodies.len()`) is an upper bound, not
                // exact (not every body has a `Some(key)`), but a small
                // over-reservation beats the reallocations this file's
                // bodies would otherwise cause one at a time. See the
                // `hotpath`-measured finding in `BACKLOG.md` §2 this
                // targets.
                let mut extra_symbols = Vec::with_capacity(
                    bodies
                        .iter()
                        .map(|(_, body)| body.pending_locals.len())
                        .sum(),
                );
                let mut file_bodies = incremental::FileBodies {
                    refs: ReferenceTable::default(),
                    scopes: FxHashMap::with_capacity_and_hasher(bodies.len(), Default::default()),
                };
                for (key, body) in bodies {
                    let remap = |id: SymbolId| resolve::remap_local_id(id, base);
                    let mut scopes = body.scopes;
                    scopes.remap_symbol_ids(&remap);
                    body.refs.map_ids_into(&remap, &mut file_bodies.refs);
                    if let Some(key) = key {
                        file_bodies.scopes.insert(key, scopes);
                    }
                    base += body.pending_locals.len() as u32;
                    extra_symbols.extend(body.pending_locals);
                }
                cache.table.append_file_symbols(file, extra_symbols);
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
        let bodies: FxHashMap<FileId, Arc<FileBodies>> = current_ids
            .iter()
            .filter_map(|&file| cache.bodies.get(&file).map(|fb| (file, Arc::clone(fb))))
            .collect();

        BoundProgram {
            files,
            file_ids,
            parses,
            symbols: cache.table.clone(),
            schema,
            stdlib,
            bodies,
            vf_referenced_classes,
        }
    }

    /// Every file this bind knows about. A `HashSet` dedup over
    /// `symbols`' own per-symbol `file` field rather than a stored list --
    /// cheap (one entry per symbol, not per file, but still a tiny
    /// fraction of a project's total symbol count) and avoids a
    /// dedicated file-list field nothing else needs; a batch caller
    /// wanting "every file" (`apexls dead`'s whole-project scan) is the
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

    pub fn syntax(&self, file: FileId) -> SyntaxNode {
        self.parses[&file].syntax()
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

    /// Every reference's `SyntaxPtr` and its `Resolution`, across every
    /// file -- the whole-project view `Self::resolution` doesn't give
    /// one lookup at a time. Costs time proportional to the whole
    /// project to iterate in full, same as it always did; unlike
    /// `Self::resolution`, there's no way to avoid that for a query that
    /// is, by definition, asking for everything.
    pub fn all_resolutions(&self) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)> {
        self.bodies.values().flat_map(|fb| fb.refs.iter())
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

fn bind_symbol_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
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
                    resolve::bind_type_ref(table, schema, symbol.file, enclosing_type, &ty),
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
            if let Some(p) = symbol.ptr.to_node(root).and_then(PropertyDecl::cast) {
                out.extend(p.accessors().filter_map(|accessor| {
                    let body = accessor.body()?;
                    let key = SyntaxPtr::new(symbol.file, body.syntax());
                    let bound = resolve::bind_body(
                        table,
                        schema,
                        stdlib,
                        symbol.file,
                        symbol.container,
                        None,
                        &[],
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
                let bound =
                    resolve::bind_trigger_body(table, schema, stdlib, symbol.file, Some(id), &block);
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
