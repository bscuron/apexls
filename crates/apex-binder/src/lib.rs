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

mod collect;
mod file_id;
mod file_table;
mod incremental;
mod inherit;
mod ptr;
mod reference_table;
mod resolve;
mod schema_index;
mod scope;
mod soql;
mod symbol;
mod symbol_table;

pub use file_id::FileId;
pub use incremental::BindCache;
pub use ptr::{AstPtr, SyntaxPtr};
pub use reference_table::{ReferenceTable, Resolution};
pub use schema_index::SchemaIndex;
pub use scope::{Scope, ScopeId, ScopeKind, ScopeTree};
pub use symbol::{ModifierSet, Sharing, Symbol, SymbolId, SymbolKind, Visibility};
pub use symbol_table::SymbolTable;

use apex_parser::Parse;
use apex_syntax::ast::decl::{
    CompilationUnit, ConstructorDecl, MethodDecl, PropertyDecl, TriggerUnit, VarDeclarator,
};
use apex_syntax::SyntaxNode;
use incremental::FileBodies;
use rayon::prelude::*;
use rowan::ast::AstNode;
use std::collections::{HashMap, HashSet};
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
    files: HashMap<FileId, PathBuf>,
    parses: HashMap<FileId, Parse>,
    pub symbols: SymbolTable,
    pub schema: SchemaIndex,
    bodies: HashMap<FileId, Arc<FileBodies>>,
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
        let discovery = apex_discover::discover(root);
        let schema = SchemaIndex::build(root);

        // Stage 0: resolve every discovered path to a stable `FileId`,
        // and prune anything the cache still remembers that no longer
        // exists (a file deleted or renamed since the last call) -- a
        // removal changes the project-wide namespace just as much as an
        // addition does, so it also forces the conservative "declarations
        // changed" path below.
        let current_files: Vec<(PathBuf, FileId)> = discovery
            .apex_files
            .iter()
            .map(|path| (path.clone(), cache.files.id_for(path)))
            .collect();
        let current_ids: HashSet<FileId> = current_files.iter().map(|(_, id)| *id).collect();
        let current_paths: HashSet<&Path> =
            current_files.iter().map(|(p, _)| p.as_path()).collect();

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
            cache.bodies.remove(&file);
        }
        cache
            .parses
            .retain(|path, _| current_paths.contains(path.as_path()));

        // Stage 1a (parallel): read (or reuse an override's in-memory
        // content) and parse (or reuse a byte-identical cache hit's
        // already-built tree) every file independently.
        struct ParsedFile {
            path: PathBuf,
            file: FileId,
            trigger: bool,
            dirty: bool,
            content: String,
            parse: Parse,
        }
        let parsed: Vec<ParsedFile> = current_files
            .par_iter()
            .filter_map(|(path, file)| {
                let content = match overrides.get(path) {
                    Some(c) => c.clone(),
                    None => std::fs::read_to_string(path).ok()?,
                };
                let trigger = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
                if let Some((cached_content, cached_parse)) = cache.parses.get(path) {
                    if cached_content == &content {
                        return Some(ParsedFile {
                            path: path.clone(),
                            file: *file,
                            trigger,
                            dirty: false,
                            content,
                            parse: cached_parse.clone(),
                        });
                    }
                }
                let parse = if trigger {
                    apex_parser::parse_trigger_unit(&content)
                } else {
                    apex_parser::parse_compilation_unit(&content)
                };
                Some(ParsedFile {
                    path: path.clone(),
                    file: *file,
                    trigger,
                    dirty: true,
                    content,
                    parse,
                })
            })
            .collect();

        // Refresh the parse cache for dirty files only -- an unchanged
        // file's entry is already correct.
        for p in parsed.iter().filter(|p| p.dirty) {
            cache
                .parses
                .insert(p.path.clone(), (p.content.clone(), p.parse.clone()));
        }

        // Stage 1b (parallel): Pass 1 declaration collection, but only
        // for dirty files -- an unchanged file's declarations are still
        // exactly what `cache.table`/`cache.raw_extends`/`cache.raw_super`
        // already hold.
        let fresh: Vec<(FileId, collect::FileCollection)> = parsed
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
            .collect();

        // Sequential merge: patch each dirty file's slice of the
        // persistent `SymbolTable`/Pass-1.5 inputs, tracking whether any
        // file's *declared shape* actually changed. Cheap (no parsing/
        // tree-walking left to do here, just moving already-built
        // `Symbol`s and a same-length field-by-field comparison).
        for (file, collection) in fresh {
            let is_new = !cache.table.has_file(file);
            if is_new
                || !declarations_equivalent(cache.table.symbols_of_file(file), &collection.symbols)
            {
                declarations_changed = true;
            }
            cache.table.set_file_symbols(file, collection.symbols);
            cache.raw_extends.insert(file, collection.raw_extends);
            cache.raw_super.insert(file, collection.raw_super);
        }

        // Pass 1.5 + derived-index rebuild only when something actually
        // declared changed project-wide -- otherwise every index and
        // `inherited_chain`/`direct_super` entry is still exactly
        // correct from the previous call (see `SymbolTable::rebuild_indices`'s
        // doc comment for why skipping this is sound, not just fast).
        if declarations_changed {
            cache.table.rebuild_indices();
            let all_raw_extends: Vec<(SymbolId, Vec<String>)> =
                cache.raw_extends.values().flatten().cloned().collect();
            let all_raw_super: Vec<(SymbolId, String)> =
                cache.raw_super.values().flatten().cloned().collect();
            inherit::resolve_inheritance(&mut cache.table, &all_raw_extends, &all_raw_super);
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
        let parse_by_file: HashMap<FileId, &Parse> =
            parsed.iter().map(|p| (p.file, &p.parse)).collect();
        let files_to_rebind: Vec<FileId> = if declarations_changed {
            current_ids.iter().copied().collect()
        } else {
            parsed.iter().filter(|p| p.dirty).map(|p| p.file).collect()
        };
        let to_bind: Vec<(FileId, SymbolId, Symbol)> =
            files_to_rebind
                .iter()
                .flat_map(|&file| {
                    cache.table.symbols_of_file(file).iter().enumerate().map(
                        move |(local, symbol)| {
                            (file, SymbolId::new(file, local as u32), symbol.clone())
                        },
                    )
                })
                .collect();
        let bound: Vec<(FileId, Option<SyntaxPtr>, resolve::BoundBody)> = to_bind
            .par_iter()
            .flat_map(|(file, id, symbol)| {
                let root_node = parse_by_file[file].syntax();
                bind_symbol_body(&cache.table, &schema, &root_node, *id, symbol)
                    .into_iter()
                    .map(move |(key, body)| (*file, key, body))
                    .collect::<Vec<_>>()
            })
            .collect();

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
        let mut by_file: HashMap<FileId, Vec<(Option<SyntaxPtr>, resolve::BoundBody)>> =
            files_to_rebind.iter().map(|&f| (f, Vec::new())).collect();
        for (file, key, body) in bound {
            by_file.entry(file).or_default().push((key, body));
        }
        for (file, bodies) in by_file {
            let mut base = cache.table.symbols_of_file(file).len() as u32;
            let mut extra_symbols = Vec::new();
            let mut file_bodies = incremental::FileBodies::default();
            for (key, body) in bodies {
                let remap = |id: SymbolId| resolve::remap_local_id(id, base);
                let mut scopes = body.scopes;
                scopes.remap_symbol_ids(&remap);
                body.refs.map_ids(&remap).merge_into(&mut file_bodies.refs);
                if let Some(key) = key {
                    file_bodies.scopes.insert(key, scopes);
                }
                base += body.pending_locals.len() as u32;
                extra_symbols.extend(body.pending_locals);
            }
            cache.table.append_file_symbols(file, extra_symbols);
            cache.bodies.insert(file, Arc::new(file_bodies));
        }

        // Assemble this call's independent, owned snapshot. `symbols`
        // (`Arc`-backed, see `symbol_table`'s module doc comment) and
        // `bodies` (`Arc`-shared per file) are both cheap here regardless
        // of project size -- an *unaffected* file only costs a pointer
        // clone, not a deep copy of its `Symbol`s/references/scopes.
        let files: HashMap<FileId, PathBuf> =
            current_files.into_iter().map(|(p, f)| (f, p)).collect();
        let parses: HashMap<FileId, Parse> =
            parsed.into_iter().map(|p| (p.file, p.parse)).collect();
        let bodies: HashMap<FileId, Arc<FileBodies>> = current_ids
            .iter()
            .filter_map(|&file| cache.bodies.get(&file).map(|fb| (file, Arc::clone(fb))))
            .collect();

        BoundProgram {
            files,
            parses,
            symbols: cache.table.clone(),
            schema,
            bodies,
        }
    }

    pub fn file_path(&self, file: FileId) -> &Path {
        &self.files[&file]
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

    /// Every reference's `SyntaxPtr` and its `Resolution`, across every
    /// file -- the whole-project view `Self::resolution` doesn't give
    /// one lookup at a time. Costs time proportional to the whole
    /// project to iterate in full, same as it always did; unlike
    /// `Self::resolution`, there's no way to avoid that for a query that
    /// is, by definition, asking for everything.
    pub fn all_resolutions(&self) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)> {
        self.bodies.values().flat_map(|fb| fb.refs.iter())
    }

    pub fn scope_tree(&self, block: SyntaxPtr) -> Option<&ScopeTree> {
        self.bodies.get(&block.file())?.scopes.get(&block)
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
fn bind_symbol_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    root: &SyntaxNode,
    id: SymbolId,
    symbol: &Symbol,
) -> Vec<(Option<SyntaxPtr>, resolve::BoundBody)> {
    match symbol.kind {
        SymbolKind::Method => {
            let Some(m) = symbol.ptr.to_node(root).and_then(MethodDecl::cast) else {
                return Vec::new();
            };
            let Some(body) = m.body() else {
                return Vec::new();
            };
            let params = table.params(id);
            let key = SyntaxPtr::new(symbol.file, body.syntax());
            let bound = resolve::bind_body(
                table,
                schema,
                symbol.file,
                symbol.container,
                Some(id),
                &params,
                &body,
            );
            vec![(Some(key), bound)]
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
                symbol.file,
                symbol.container,
                Some(id),
                &params,
                &body,
            );
            vec![(Some(key), bound)]
        }
        SymbolKind::Property => {
            let Some(p) = symbol.ptr.to_node(root).and_then(PropertyDecl::cast) else {
                return Vec::new();
            };
            p.accessors()
                .filter_map(|accessor| {
                    let body = accessor.body()?;
                    let key = SyntaxPtr::new(symbol.file, body.syntax());
                    let bound = resolve::bind_body(
                        table,
                        schema,
                        symbol.file,
                        symbol.container,
                        None,
                        &[],
                        &body,
                    );
                    Some((Some(key), bound))
                })
                .collect()
        }
        SymbolKind::Field => {
            let Some(decl) = symbol.ptr.to_node(root).and_then(VarDeclarator::cast) else {
                return Vec::new();
            };
            let Some(init) = decl.init() else {
                return Vec::new();
            };
            let bound =
                resolve::bind_initializer(table, schema, symbol.file, symbol.container, &init);
            vec![(None, bound)]
        }
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
                    resolve::bind_trigger_body(table, schema, symbol.file, Some(id), &block);
                out.push((Some(key), bound));
            }
            out
        }
        SymbolKind::Interface
        | SymbolKind::Class
        | SymbolKind::Enum
        | SymbolKind::EnumConstant
        | SymbolKind::Parameter
        | SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => Vec::new(),
    }
}
