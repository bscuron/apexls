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
mod inherit;
mod parse_cache;
mod ptr;
mod reference_table;
mod resolve;
mod schema_index;
mod scope;
mod soql;
mod symbol;
mod symbol_table;

pub use file_id::FileId;
pub use parse_cache::ParseCache;
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
use rayon::prelude::*;
use rowan::ast::AstNode;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A whole bound project: every file's parse tree, the project-wide
/// symbol table, every reference's resolution, the Salesforce object/
/// field schema index consulted along the way, and every body's scope
/// tree (keyed by that body's own `Block`/`TriggerBlock`
/// [`SyntaxPtr`] -- not by symbol, since a property's two accessors
/// share one symbol but have two independent bodies).
pub struct BoundProgram {
    files: Vec<PathBuf>,
    parses: Vec<Parse>,
    pub symbols: SymbolTable,
    pub refs: ReferenceTable,
    pub schema: SchemaIndex,
    scopes: HashMap<SyntaxPtr, ScopeTree>,
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
    /// `root`, plus its SFDX object/field metadata. Always reads every
    /// file fresh from disk and re-parses it -- see
    /// [`Self::from_files_cached`] for a version that skips re-parsing
    /// files whose content hasn't changed since a previous call.
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
        let mut cache = ParseCache::default();
        Self::from_files_cached(root, overrides, &mut cache)
    }

    /// Like [`Self::from_files_with_overrides`], but reuses `cache`'s
    /// previous `(content, Parse)` for any file whose content is
    /// byte-for-byte identical to last time, skipping that file's
    /// lex/parse entirely -- and updates `cache` to reflect this call's
    /// results before returning, so the next call benefits too. See
    /// `BACKLOG.md` §2 and [`ParseCache`]'s module doc comment. Binding
    /// itself (Pass 1/1.5/2 below) still runs project-wide on every
    /// call regardless of what was cached -- this cache only ever saves
    /// parse work, not bind work; see `BACKLOG.md` §2's "incremental
    /// rebind" item for the (deliberately not-yet-built) next step that
    /// would also skip unaffected binding.
    pub fn from_files_cached(
        root: impl AsRef<Path>,
        overrides: &HashMap<PathBuf, String>,
        cache: &mut ParseCache,
    ) -> Self {
        ensure_large_worker_stacks();
        let root = root.as_ref();
        let discovery = apex_discover::discover(root);
        let schema = SchemaIndex::build(root);

        // Stage 1a (parallel): read (or reuse an override's in-memory
        // content) and parse (or reuse a cache hit's already-built tree)
        // every file independently. `par_iter().filter_map(...).collect::<Vec<_>>()`
        // preserves `discovery.apex_files`'s original order (rayon's
        // indexed collection always does), so a file's final position in
        // this `Vec` -- and thus its `FileId` below -- stays
        // deterministic across runs regardless of which thread actually
        // processed it.
        let parsed: Vec<(PathBuf, bool, String, Parse)> = discovery
            .apex_files
            .par_iter()
            .filter_map(|path| {
                let src = match overrides.get(path) {
                    Some(src) => src.clone(),
                    None => std::fs::read_to_string(path).ok()?,
                };
                let trigger = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
                if let Some((cached_src, cached_parse)) = cache.by_path.get(path) {
                    if cached_src == &src {
                        return Some((path.clone(), trigger, src, cached_parse.clone()));
                    }
                }
                let parse = if trigger {
                    apex_parser::parse_trigger_unit(&src)
                } else {
                    apex_parser::parse_compilation_unit(&src)
                };
                Some((path.clone(), trigger, src, parse))
            })
            .collect();

        // Stage 1b (parallel): Pass 1 declaration collection, one file
        // at a time -- a pure function of that file's already-parsed
        // tree with no cross-file lookups (see `collect`'s module doc
        // comment), safe to run fully independently now that every
        // file has a final `FileId` (its index here).
        let collections: Vec<collect::FileCollection> = parsed
            .par_iter()
            .enumerate()
            .map(|(i, (_, trigger, _, parse))| {
                let file = FileId(i as u32);
                let root_node = parse.syntax();
                if *trigger {
                    TriggerUnit::cast(root_node.clone())
                        .map(|tu| collect::collect_trigger_unit(file, &tu))
                } else {
                    CompilationUnit::cast(root_node.clone())
                        .map(|cu| collect::collect_compilation_unit(file, &cu))
                }
                .unwrap_or_default()
            })
            .collect();

        let files: Vec<PathBuf> = parsed.iter().map(|(p, _, _, _)| p.clone()).collect();

        // Refresh the cache with this call's own results before moving
        // `parsed`'s `Parse`s out below -- a file no longer present in
        // `parsed` (deleted, or failed to read) is naturally dropped
        // from the cache here too, since this replaces the whole map
        // rather than merging into it.
        cache.by_path = parsed
            .iter()
            .map(|(path, _, src, parse)| (path.clone(), (src.clone(), parse.clone())))
            .collect();

        let parses: Vec<Parse> = parsed.into_iter().map(|(_, _, _, parse)| parse).collect();

        // Sequential merge: fold each file's locally-numbered symbols
        // into the shared arena, remapping `container` ids by that
        // file's base offset. Cheap (no parsing/tree-walking left to
        // do here, just `Vec` extends and integer arithmetic), so this
        // doesn't need to be parallel itself.
        let mut table = SymbolTable::default();
        let mut raw_extends: Vec<(SymbolId, Vec<String>)> = Vec::new();
        let mut raw_super: Vec<(SymbolId, String)> = Vec::new();
        for collection in collections {
            let base = table.len() as u32;
            for mut symbol in collection.symbols {
                symbol.container = symbol
                    .container
                    .map(|SymbolId(local)| SymbolId(base + local));
                table.alloc(symbol);
            }
            for (local, names) in collection.raw_extends {
                raw_extends.push((SymbolId(base + local), names));
            }
            for (local, name) in collection.raw_super {
                raw_super.push((SymbolId(base + local), name));
            }
        }

        // Pass 1.5 (parallel where it counts): resolves every type's
        // `extends`/`implements` chain -- see `inherit`'s module doc
        // comment for the parallel/sequential split within it.
        inherit::resolve_inheritance(&mut table, &raw_extends, &raw_super);

        // Pass 2 (parallel): bind every declared symbol's body/
        // initializer independently against the now-final, read-only
        // `table` -- see `resolve`'s module doc comment for how a
        // body's own newly-declared locals stay out of `table` until
        // the sequential merge below (`rayon`-safe concurrent binding
        // needs a single shared mutable arena avoided, not locked).
        let declared: Vec<(SymbolId, Symbol)> =
            table.iter().map(|(id, s)| (id, s.clone())).collect();
        let bound: Vec<(Option<SyntaxPtr>, resolve::BoundBody)> = declared
            .par_iter()
            .flat_map(|(id, symbol)| {
                let root_node = parses[symbol.file.index()].syntax();
                bind_symbol_body(&table, &schema, &root_node, *id, symbol)
            })
            .collect();

        // Sequential merge, mirroring Pass 1's: allocate each body's
        // pending locals into the shared table, remap that body's
        // sentinel ids to the resulting real global ids, and fold its
        // reference/scope-tree output into the project-wide result.
        let mut refs = ReferenceTable::default();
        let mut scope_trees = HashMap::new();
        for (key, body) in bound {
            let base = table.len() as u32;
            for local in body.pending_locals {
                table.alloc(local);
            }
            let remap = |id: SymbolId| resolve::remap_local_id(id, base);
            let mut scopes = body.scopes;
            scopes.remap_symbol_ids(&remap);
            body.refs.map_ids(&remap).merge_into(&mut refs);
            if let Some(key) = key {
                scope_trees.insert(key, scopes);
            }
        }

        BoundProgram {
            files,
            parses,
            symbols: table,
            refs,
            schema,
            scopes: scope_trees,
        }
    }

    pub fn file_path(&self, file: FileId) -> &Path {
        &self.files[file.index()]
    }

    pub fn syntax(&self, file: FileId) -> SyntaxNode {
        self.parses[file.index()].syntax()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn scope_tree(&self, block: SyntaxPtr) -> Option<&ScopeTree> {
        self.scopes.get(&block)
    }
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
            let key = SyntaxPtr::new(body.syntax());
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
            let key = SyntaxPtr::new(body.syntax());
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
                    let key = SyntaxPtr::new(body.syntax());
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
                    let bound =
                        resolve::bind_object_ref(schema, SyntaxPtr::new(&parent), obj_tok.text());
                    out.push((None, bound));
                }
            }
            if let Some(block) = tu.block() {
                let key = SyntaxPtr::new(block.syntax());
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
