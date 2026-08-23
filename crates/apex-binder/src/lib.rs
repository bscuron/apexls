//! The symbol-table binder: turns "we can parse and walk Apex" into "we
//! can answer real LSP questions." [`BoundProgram::from_files`] walks a
//! whole project (via `apex_discover::discover`) through three passes:
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
//! and honestly imprecise everywhere real type inference or overload
//! resolution would be required (`Resolution::Candidates`/`Unresolved`
//! rather than a guessed single answer).

mod collect;
mod file_id;
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

impl BoundProgram {
    /// Discovers, parses, and binds every `.cls`/`.trigger` file under
    /// `root`, plus its SFDX object/field metadata.
    pub fn from_files(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let discovery = apex_discover::discover(root);
        let schema = SchemaIndex::build(root);

        let mut files = Vec::new();
        let mut parses = Vec::new();
        let mut is_trigger = Vec::new();
        for path in &discovery.apex_files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let trigger = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
            let parse = if trigger {
                apex_parser::parse_trigger_unit(&src)
            } else {
                apex_parser::parse_compilation_unit(&src)
            };
            files.push(path.clone());
            is_trigger.push(trigger);
            parses.push(parse);
        }

        let mut table = SymbolTable::default();
        let mut raw_extends: Vec<(SymbolId, Vec<String>)> = Vec::new();
        let mut raw_super: Vec<(SymbolId, String)> = Vec::new();

        for (i, parse) in parses.iter().enumerate() {
            let file = FileId(i as u32);
            let root_node = parse.syntax();
            let collection = if is_trigger[i] {
                TriggerUnit::cast(root_node.clone())
                    .map(|tu| collect::collect_trigger_unit(file, &tu))
            } else {
                CompilationUnit::cast(root_node.clone())
                    .map(|cu| collect::collect_compilation_unit(file, &cu))
            };
            let Some(collection) = collection else {
                continue;
            };

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

        inherit::resolve_inheritance(&mut table, &raw_extends, &raw_super);

        let mut refs = ReferenceTable::default();
        let mut scope_trees = HashMap::new();

        // Snapshotted before Pass 2, which allocates new local symbols
        // into `table` as it walks -- iterating `table.iter()` live
        // while also mutating it isn't possible in safe Rust, and isn't
        // what's wanted anyway (Pass 2 shouldn't try to bind bodies for
        // the locals it itself just declared).
        let declared: Vec<(SymbolId, Symbol)> =
            table.iter().map(|(id, s)| (id, s.clone())).collect();

        for (id, symbol) in &declared {
            let root_node = parses[symbol.file.index()].syntax();
            bind_symbol_body(
                &mut table,
                &schema,
                &mut refs,
                &mut scope_trees,
                &root_node,
                *id,
                symbol,
            );
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

fn bind_symbol_body(
    table: &mut SymbolTable,
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    scope_trees: &mut HashMap<SyntaxPtr, ScopeTree>,
    root: &SyntaxNode,
    id: SymbolId,
    symbol: &Symbol,
) {
    match symbol.kind {
        SymbolKind::Method => {
            let Some(m) = symbol.ptr.to_node(root).and_then(MethodDecl::cast) else {
                return;
            };
            let Some(body) = m.body() else { return };
            let params = params_of(table, id);
            let key = SyntaxPtr::new(body.syntax());
            let tree = resolve::bind_body(
                table,
                schema,
                refs,
                symbol.file,
                symbol.container,
                Some(id),
                &params,
                &body,
            );
            scope_trees.insert(key, tree);
        }
        SymbolKind::Constructor => {
            let Some(c) = symbol.ptr.to_node(root).and_then(ConstructorDecl::cast) else {
                return;
            };
            let Some(body) = c.body() else { return };
            let params = params_of(table, id);
            let key = SyntaxPtr::new(body.syntax());
            let tree = resolve::bind_body(
                table,
                schema,
                refs,
                symbol.file,
                symbol.container,
                Some(id),
                &params,
                &body,
            );
            scope_trees.insert(key, tree);
        }
        SymbolKind::Property => {
            let Some(p) = symbol.ptr.to_node(root).and_then(PropertyDecl::cast) else {
                return;
            };
            for accessor in p.accessors() {
                let Some(body) = accessor.body() else {
                    continue;
                };
                let key = SyntaxPtr::new(body.syntax());
                let tree = resolve::bind_body(
                    table,
                    schema,
                    refs,
                    symbol.file,
                    symbol.container,
                    None,
                    &[],
                    &body,
                );
                scope_trees.insert(key, tree);
            }
        }
        SymbolKind::Field => {
            let Some(decl) = symbol.ptr.to_node(root).and_then(VarDeclarator::cast) else {
                return;
            };
            if let Some(init) = decl.init() {
                resolve::bind_initializer(
                    table,
                    schema,
                    refs,
                    symbol.file,
                    symbol.container,
                    &init,
                );
            }
        }
        SymbolKind::Trigger => {
            let Some(tu) = symbol.ptr.to_node(root).and_then(TriggerUnit::cast) else {
                return;
            };
            if let Some(obj_tok) = tu.object_ref() {
                if let Some(parent) = obj_tok.parent() {
                    schema_index::resolve_object(
                        schema,
                        refs,
                        SyntaxPtr::new(&parent),
                        obj_tok.text(),
                    );
                }
            }
            if let Some(block) = tu.block() {
                let key = SyntaxPtr::new(block.syntax());
                let tree =
                    resolve::bind_trigger_body(table, schema, refs, symbol.file, Some(id), &block);
                scope_trees.insert(key, tree);
            }
        }
        SymbolKind::Interface
        | SymbolKind::Class
        | SymbolKind::Enum
        | SymbolKind::EnumConstant
        | SymbolKind::Parameter
        | SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => {}
    }
}

fn params_of(table: &SymbolTable, container: SymbolId) -> Vec<SymbolId> {
    table
        .members_of(container)
        .iter()
        .copied()
        .filter(|&id| table.get(id).kind == SymbolKind::Parameter)
        .collect()
}
