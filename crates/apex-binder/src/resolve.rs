//! Pass 2: scope building + reference resolution. Walks one method/
//! constructor/property-accessor body (or a bare field/property
//! initializer expression) building a [`ScopeTree`] alongside a
//! [`Stmt`]/[`Expr`] walk, resolving references as they're encountered.
//!
//! Runs only after Pass 1.5 (`crate::inherit`) has finished, so member/
//! inherited-chain lookups against the whole project are always safe.
//! Like Pass 1, each body is bound independently against a read-only
//! `&SymbolTable` and is safe to run in parallel (`crate::BoundProgram::from_files`
//! does, via `rayon`) -- a body's newly-declared locals (`LocalVar`/
//! `CatchVar`/`ForEachVar`/`SwitchBindingVar`) are allocated into a
//! body-local arena (`BodyBinder::pending_locals`) tagged with a
//! sentinel `SymbolId` (see [`LOCAL_SENTINEL_BASE`]) rather than the
//! shared table directly, and merged in afterward by a fast sequential
//! pass -- the same local-collect-then-merge trick Pass 1 uses for
//! symbols, applied here to keep concurrent bodies from needing to
//! coordinate a single shared mutable arena.
//!
//! Scope of what gets resolved, and how -- see the project's design
//! plan for the full rationale, summarized here:
//! - Unqualified names (locals/params/fields/enclosing-type members)
//!   resolve via lexical scope, then member lookup through the
//!   enclosing type's `extends`/`implements` chain -- both steps need
//!   no type inference, since Apex guarantees an unqualified name
//!   resolves unambiguously (or not at all) by simple name.
//! - `this`/`super` are a cheap one-hop special case: `this` is always
//!   the enclosing type, `super` is always its direct `extends` target
//!   (`SymbolTable::direct_super`).
//! - A qualified reference (`target.member`) only resolves past
//!   `Unresolved` when `target`'s type is *already* known without any
//!   inference -- a `NameExpr`/`this`/`super`/`new` expression whose
//!   declared type is itself a project-local class. Anything requiring
//!   real type inference (the result of an arbitrary method call, a
//!   binary expression, ...) stays `Unresolved` rather than guessing.
//! - Method/constructor calls (qualified or not, including `new`/
//!   `this(...)`/`super(...)`) go through [`narrow_by_overload`]: an
//!   exact arity filter (real Apex semantics -- there's no varargs or
//!   default parameter values for user-defined methods, so arity alone
//!   is authoritative, not a heuristic), then a best-effort narrowing by
//!   known project-local argument types among same-arity candidates. A
//!   call resolves to a single `Resolved` symbol when exactly one
//!   candidate survives; otherwise it's `Candidates` (genuinely
//!   ambiguous, or argument types v1 can't check -- system/library
//!   types have no model to compare against) or `Unresolved` (no
//!   same-name member at all).

use crate::file_id::FileId;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::reference_table::{ReferenceTable, Resolution};
use crate::schema_index::SchemaIndex;
use crate::scope::{ScopeId, ScopeKind, ScopeTree};
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind};
use crate::symbol_table::SymbolTable;
use crate::ty::Ty;
use apex_syntax::ast::decl::TriggerBlock;
use apex_syntax::ast::expr::{CallExpr, FieldExpr, Initializer, MethodCallExpr, NameExpr, NewExpr};
use apex_syntax::ast::stmt::Block;
use apex_syntax::ast::{Expr, Name, Stmt, Type};
use apex_syntax::SyntaxKind;
use rowan::ast::AstNode;

/// Narrows a same-name candidate set (methods, or constructors for a
/// `new`/`this(...)`/`super(...)` call) using real Apex overload-
/// resolution rules where they're checkable without a full type system.
///
/// First, an exact arity filter: Apex has no varargs or default
/// parameter values for user-defined methods/constructors, so a
/// candidate whose parameter count doesn't equal `arg_types.len()`
/// genuinely cannot be the one called -- this is exact, not a
/// heuristic. If nothing survives arity filtering at all (a real
/// compile error, or a gap in this binder's own counting), the original
/// full candidate set is reported rather than claiming nothing matched.
///
/// Second, among same-arity survivors, elimination by known project-
/// local argument types: a candidate is ruled out only when some
/// argument's inferred type is *positively* incompatible with the
/// corresponding parameter's declared type (neither an exact match nor
/// a subtype via `extends`/`implements`). An argument whose type isn't
/// known at all, or is only known as a *system* type (`Ty::System` --
/// a literal, a `List<T>`, an uninferred call result, ...), or a
/// parameter whose declared type isn't itself project-local (`String`,
/// `List<T>`, ...) can never rule a candidate out: "can't prove wrong"
/// always wins over "assume wrong." System-vs-system comparisons are
/// deliberately never attempted either, even when both names are known
/// (an `Integer` argument against a `Long` parameter, say) -- Apex's
/// real implicit-numeric-widening rules aren't modeled, and a wrong
/// elimination is a real correctness bug, not just a missed narrowing;
/// only project-local-vs-project-local comparisons are exact enough to
/// eliminate on.
fn narrow_by_overload(
    table: &SymbolTable,
    candidates: Vec<SymbolId>,
    arg_types: &[Option<Ty>],
) -> Resolution {
    if candidates.is_empty() {
        return Resolution::Unresolved;
    }

    let by_arity: Vec<SymbolId> = candidates
        .iter()
        .copied()
        .filter(|&id| table.params(id).len() == arg_types.len())
        .collect();
    let pool = if by_arity.is_empty() {
        candidates
    } else {
        by_arity
    };
    if pool.len() == 1 {
        return Resolution::Resolved(pool[0]);
    }

    let by_type: Vec<SymbolId> = pool
        .iter()
        .copied()
        .filter(|&id| is_argument_type_compatible(table, id, arg_types))
        .collect();
    match by_type.len() {
        1 => Resolution::Resolved(by_type[0]),
        // Over-eliminated (every candidate ruled itself out, which given
        // the conservative rule above should only happen if `pool`
        // itself was already empty -- defensive, not expected) or still
        // ambiguous: report the honest pre-type-filter pool either way.
        0 => Resolution::Candidates(pool),
        _ => Resolution::Candidates(by_type),
    }
}

fn is_argument_type_compatible(
    table: &SymbolTable,
    candidate: SymbolId,
    arg_types: &[Option<Ty>],
) -> bool {
    for (param, arg_type) in table.params(candidate).iter().zip(arg_types.iter()) {
        // Only a project-local argument type is exact enough to compare
        // -- see this function's caller's doc comment for why a `Ty::System`
        // argument (or an entirely unknown one) never eliminates.
        let Some(Ty::Project(arg_type_id)) = arg_type else {
            continue;
        };
        let Some(param_type_name) = table.get(*param).type_name.as_deref() else {
            continue;
        };
        let Some(param_type_id) = table.top_level(param_type_name) else {
            continue;
        };
        if *arg_type_id == param_type_id {
            continue;
        }
        if table.inherited_chain(*arg_type_id).contains(&param_type_id) {
            continue; // the argument's type extends/implements the parameter's type -- a valid upcast
        }
        return false;
    }
    true
}

/// The reserved `SymbolId` range `declare_local` allocates from during
/// one body's binding pass, disjoint from any real project `SymbolId`
/// (no real Apex org has ~2 billion top-level declarations). A
/// placeholder, remapped to a real global id once every body's local
/// batch has been merged into the shared `SymbolTable`
/// (`crate::BoundProgram::from_files`). Using a disjoint numbering range
/// -- rather than an `enum` wrapper around every `SymbolId`-typed field
/// -- keeps `Resolution`/`Scope` unchanged: the remap step is a single
/// `SymbolId -> SymbolId` function, a no-op below this threshold,
/// applied uniformly without needing to track which ids came from where.
pub(crate) const LOCAL_SENTINEL_BASE: u32 = u32::MAX / 2;

/// `base` is the *file's own* declared-symbol count (`FileCollection::symbols.len()`
/// for the file this sentinel id's `id.file` names) -- not a project-wide
/// count. Since a body's sentinel ids already carry the correct `file`
/// (set once at `declare_local` time), remapping only ever needs to
/// translate `local`, never `file`.
pub(crate) fn remap_local_id(id: SymbolId, base: u32) -> SymbolId {
    if id.local >= LOCAL_SENTINEL_BASE {
        SymbolId::new(id.file, base + (id.local - LOCAL_SENTINEL_BASE))
    } else {
        id
    }
}

/// One body's complete Pass 2 output, still containing sentinel ids
/// (see [`LOCAL_SENTINEL_BASE`]) for any symbol it locally declared.
/// `crate::BoundProgram::from_files` merges `pending_locals` into the
/// shared `SymbolTable` and remaps `scopes`/`refs` accordingly, once per
/// body -- safe to do for many bodies' outputs in any order, including
/// interleaved from multiple threads, since each body's own remap only
/// touches its own data.
pub(crate) struct BoundBody {
    pub(crate) scopes: ScopeTree,
    pub(crate) pending_locals: Vec<Symbol>,
    pub(crate) refs: ReferenceTable,
}

/// One method/constructor/property-accessor body's binding context:
/// shared read-only project state (`table`, `schema`), this body's own
/// local write targets (`refs`, `scopes`, `pending_locals`), all owned
/// so binding never needs to coordinate with any other body running
/// concurrently.
pub(crate) struct BodyBinder<'a> {
    pub(crate) table: &'a SymbolTable,
    pub(crate) schema: &'a SchemaIndex,
    pub(crate) refs: ReferenceTable,
    pub(crate) scopes: ScopeTree,
    pending_locals: Vec<Symbol>,
    pub(crate) file: FileId,
    /// The enclosing type, for `this`/`super`/unqualified member lookup
    /// fallthrough once local-scope lookup misses. `None` when binding a
    /// trigger's top-level body (a `TriggerUnit` isn't itself a type
    /// symbol with members to fall through to).
    enclosing_type: Option<SymbolId>,
    /// The enclosing method/constructor, if any -- becomes the
    /// `container` of any local symbol declared inside. `None` when
    /// binding a field/property initializer expression directly (no
    /// enclosing method; `declare_local` is never reachable from a bare
    /// expression walk anyway, since only statements declare locals).
    enclosing_member: Option<SymbolId>,
}

/// Binds one method/constructor/property-accessor `Block` body: builds
/// its `ScopeTree` seeded with `params` in the root scope, walks every
/// statement/expression, and returns the (still sentinel-tagged) result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bind_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    enclosing_member: Option<SymbolId>,
    params: &[SymbolId],
    block: &Block,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member,
    };
    for &p in params {
        let name = binder.table.get(p).name.clone();
        binder.scopes.bind(root_scope, name, p);
    }
    binder.bind_block_stmts(root_scope, block);
    binder.into_bound_body()
}

/// Binds a `TriggerBlock`'s bare top-level statements -- the trigger's
/// actual executable body, which `TriggerBlock::members()` (declaration-
/// shaped children only) never reaches. Walks direct `Stmt` children
/// alongside (interleaved with, in source order) the `Member`
/// declarations Pass 1 already collected separately.
pub(crate) fn bind_trigger_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    block: &TriggerBlock,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member: None,
    };
    for child in block.syntax().children() {
        if let Some(stmt) = Stmt::cast(child) {
            binder.bind_stmt(root_scope, &stmt);
        }
    }
    binder.into_bound_body()
}

/// Binds a bare expression with no enclosing statement context (a
/// field/property initializer) -- member lookup only, no locals, no
/// `ScopeTree` worth keeping around afterward.
pub(crate) fn bind_initializer(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    expr: &Expr,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, expr.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member: None,
    };
    binder.bind_expr(root_scope, expr);
    binder.into_bound_body()
}

/// Resolves a single bare object-name reference (a trigger's `ON
/// <object>`) against `schema`, wrapped as a `BoundBody` purely so it
/// merges through the same sequential path as every other Pass 2
/// result -- there's no body/scope involved, so `scopes`/`pending_locals`
/// are empty and this entry is never inserted into
/// `BoundProgram`'s scope-tree map (its caller passes `key: None`).
pub(crate) fn bind_object_ref(schema: &SchemaIndex, ptr: SyntaxPtr, name: &str) -> BoundBody {
    let mut refs = ReferenceTable::default();
    crate::schema_index::resolve_object(schema, &mut refs, ptr, name);
    let (scopes, _) = ScopeTree::new_root(ScopeKind::Body, rowan::TextRange::empty(0.into()));
    BoundBody {
        scopes,
        pending_locals: Vec::new(),
        refs,
    }
}

impl<'a> BodyBinder<'a> {
    fn into_bound_body(self) -> BoundBody {
        BoundBody {
            scopes: self.scopes,
            pending_locals: self.pending_locals,
            refs: self.refs,
        }
    }

    fn declare_local(
        &mut self,
        kind: SymbolKind,
        name: &Name,
        type_ref: Option<&Type>,
    ) -> SymbolId {
        let (type_ptr, type_name, type_args) = match type_ref {
            Some(ty) => {
                let args = ty
                    .type_args()
                    .map(|list| list.args().map(|a| a.text()).collect())
                    .unwrap_or_default();
                (Some(AstPtr::new(self.file, ty)), Some(ty.text()), args)
            }
            None => (None, None, Vec::new()),
        };
        let symbol = Symbol {
            kind,
            name: name.text().unwrap_or_default(),
            file: self.file,
            ptr: SyntaxPtr::new(self.file, name.syntax()),
            name_range: name.syntax().text_range(),
            container: self.enclosing_member,
            type_ref: type_ptr,
            type_name,
            type_args,
            modifiers: ModifierSet::default(),
        };
        let id = SymbolId::new(
            self.file,
            LOCAL_SENTINEL_BASE + self.pending_locals.len() as u32,
        );
        self.pending_locals.push(symbol);
        id
    }

    /// `id` resolved against this body's own pending local arena if it's
    /// a sentinel id (see [`LOCAL_SENTINEL_BASE`]), otherwise against the
    /// shared, already-final `SymbolTable` -- the two id spaces a body
    /// can encounter (a symbol it just locally declared, vs. any
    /// pre-existing project symbol) need this indirection since the
    /// shared table doesn't contain this body's locals yet.
    fn get_symbol(&self, id: SymbolId) -> &Symbol {
        if id.local >= LOCAL_SENTINEL_BASE {
            &self.pending_locals[(id.local - LOCAL_SENTINEL_BASE) as usize]
        } else {
            self.table.get(id)
        }
    }

    /// `symbol`'s declared type, as a [`Ty`] -- the "type of this
    /// expression" chaining `FieldExpr`/`MethodCallExpr` target
    /// resolution needs. `Ty::Project` when the declared type is a
    /// project-local class/interface/enum; otherwise `Ty::System` naming
    /// whatever was declared (a schema object, an unmodeled system type,
    /// a generic collection with its type argument(s) substituted in --
    /// see `Symbol::type_args`), never lost to `None` just because it
    /// isn't project-local. `None` only when the symbol has no type of
    /// its own at all (a type itself, an enum constant, a `void` method,
    /// a constructor).
    fn type_of_symbol(&self, id: SymbolId) -> Option<Ty> {
        let symbol = self.get_symbol(id);
        let type_name = symbol.type_name.as_deref()?;
        if let Some(project_id) = self.table.top_level(type_name) {
            return Some(Ty::Project(project_id));
        }
        let args = symbol
            .type_args
            .iter()
            .map(|name| match self.table.top_level(name) {
                Some(id) => Ty::Project(id),
                None => Ty::system_owned(name.clone(), Vec::new()),
            })
            .collect();
        Some(Ty::system_owned(type_name.to_string(), args))
    }

    /// Resolves a plain Apex `Type` reference (a field/param/local/
    /// return type, a `new`/`instanceof`/cast target, ...) against
    /// project-local types first, then `apex-metadata`'s schema (an
    /// SObject-typed declaration, e.g. `Account a;`) -- recursively
    /// resolving (and registering `self.refs` resolutions for) any type
    /// arguments along the way regardless of which case the base name
    /// itself falls into, since `List<Account>`'s `Account` is a real,
    /// independently-resolvable reference in its own right. Returns the
    /// resulting [`Ty`] either way -- the "one hop" other resolvers chain
    /// through, now never losing the type entirely just because it isn't
    /// project-local (see [`Ty`]'s own doc comment).
    pub(crate) fn resolve_type_ref(&mut self, ty: &Type) -> Option<Ty> {
        let name = ty.text();
        let ptr = SyntaxPtr::new(self.file, ty.syntax());
        if let Some(id) = self.table.top_level(&name) {
            self.refs.set(ptr, Resolution::Resolved(id));
            return Some(Ty::Project(id));
        }
        let args: Vec<Ty> = ty
            .type_args()
            .map(|list| {
                list.args()
                    .filter_map(|arg| self.resolve_type_ref(&arg))
                    .collect()
            })
            .unwrap_or_default();
        if self.schema.object(&name).is_some() {
            self.refs.set(
                ptr,
                Resolution::SchemaObject {
                    object: name.clone(),
                    field: None,
                },
            );
            return Some(Ty::system_owned(name, args));
        }
        // Could be a standard object this repo never locally extended
        // (indistinguishable, using `apex-metadata` alone, from a
        // genuinely nonexistent name), or an unmodeled system/library
        // type (`String`, `List`, an `Exception` subtype, ...) -- v1
        // can't tell these apart, so both land here as `Unresolved`
        // rather than one of them being misreported as `UnknownSchema`.
        // The name is still real, though, so the returned `Ty` keeps it.
        self.refs.set(ptr, Resolution::Unresolved);
        Some(Ty::system_owned(name, args))
    }

    fn bind_block_stmts(&mut self, scope: ScopeId, block: &Block) {
        for stmt in block.statements() {
            self.bind_stmt(scope, &stmt);
        }
    }

    fn bind_child_block(&mut self, parent: ScopeId, block: &Block) {
        let child = self
            .scopes
            .push(Some(parent), ScopeKind::Block, block.syntax().text_range());
        self.bind_block_stmts(child, block);
    }

    fn bind_stmt(&mut self, scope: ScopeId, stmt: &Stmt) {
        match stmt {
            Stmt::Block(b) => self.bind_child_block(scope, b),
            Stmt::If(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                if let Some(t) = s.then_branch() {
                    self.bind_stmt(scope, &t);
                }
                if let Some(e) = s.else_branch() {
                    self.bind_stmt(scope, &e);
                }
            }
            Stmt::Switch(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                for when in s.when_clauses() {
                    let range = when.syntax().text_range();
                    let child = self.scopes.push(Some(scope), ScopeKind::Switch, range);
                    if let Some(value) = when.value() {
                        if let Some(ty) = value.type_ref() {
                            self.resolve_type_ref(&ty);
                        }
                        if let Some(binding) = value.binding() {
                            let sym = self.declare_local(
                                SymbolKind::SwitchBindingVar,
                                &binding,
                                value.type_ref().as_ref(),
                            );
                            if let Some(text) = binding.text() {
                                self.scopes.bind(child, text, sym);
                            }
                        }
                    }
                    if let Some(body) = when.body() {
                        self.bind_block_stmts(child, &body);
                    }
                }
            }
            Stmt::For(s) => {
                let child = self
                    .scopes
                    .push(Some(scope), ScopeKind::For, s.syntax().text_range());
                if let Some(init) = s.init() {
                    if let Some(ty) = init.type_ref() {
                        self.resolve_type_ref(&ty);
                    }
                    for d in init.declarators() {
                        if let Some(e) = d.init() {
                            self.bind_expr(child, &e);
                        }
                        if let Some(name) = d.name() {
                            let sym = self.declare_local(
                                SymbolKind::LocalVar,
                                &name,
                                init.type_ref().as_ref(),
                            );
                            if let Some(text) = name.text() {
                                self.scopes.bind(child, text, sym);
                            }
                        }
                    }
                    for e in init.exprs() {
                        self.bind_expr(child, &e);
                    }
                }
                if let Some(c) = s.condition() {
                    self.bind_expr(child, &c);
                }
                if let Some(update) = s.update() {
                    for e in update.exprs() {
                        self.bind_expr(child, &e);
                    }
                }
                if let Some(body) = s.body() {
                    self.bind_stmt(child, &body);
                }
            }
            Stmt::ForEach(s) => {
                let child = self
                    .scopes
                    .push(Some(scope), ScopeKind::For, s.syntax().text_range());
                if let Some(ty) = s.type_ref() {
                    self.resolve_type_ref(&ty);
                }
                if let Some(iterable) = s.iterable() {
                    self.bind_expr(child, &iterable);
                }
                if let Some(name) = s.name() {
                    let sym =
                        self.declare_local(SymbolKind::ForEachVar, &name, s.type_ref().as_ref());
                    if let Some(text) = name.text() {
                        self.scopes.bind(child, text, sym);
                    }
                }
                if let Some(body) = s.body() {
                    self.bind_stmt(child, &body);
                }
            }
            Stmt::While(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                if let Some(b) = s.body() {
                    self.bind_stmt(scope, &b);
                }
            }
            Stmt::DoWhile(s) => {
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
            }
            Stmt::Try(s) => {
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
                for catch in s.catch_clauses() {
                    if let Some(ty) = catch.exception_type() {
                        let ptr = SyntaxPtr::new(self.file, ty.syntax());
                        // `QualifiedName` (not `Type`) -- resolve by its
                        // whole-text base name against project types
                        // only; unlike `resolve_type_ref`, exception
                        // types are never SObject-shaped, so there's no
                        // schema fallback to attempt here.
                        let name = ty.syntax().text().to_string();
                        match self.table.top_level(&name) {
                            Some(id) => self.refs.set(ptr, Resolution::Resolved(id)),
                            None => self.refs.set(ptr, Resolution::Unresolved),
                        }
                    }
                    let child = self.scopes.push(
                        Some(scope),
                        ScopeKind::Catch,
                        catch.syntax().text_range(),
                    );
                    if let Some(name) = catch.name() {
                        let sym = self.declare_local(SymbolKind::CatchVar, &name, None);
                        if let Some(text) = name.text() {
                            self.scopes.bind(child, text, sym);
                        }
                    }
                    if let Some(body) = catch.body() {
                        self.bind_block_stmts(child, &body);
                    }
                }
                if let Some(f) = s.finally_clause() {
                    if let Some(body) = f.body() {
                        self.bind_child_block(scope, &body);
                    }
                }
            }
            Stmt::Return(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Throw(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::Insert(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Update(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Delete(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Undelete(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Upsert(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
                // The external-ID field reference isn't resolved against
                // schema in v1 -- it needs the DML target expression's
                // *type* (the SObject being upserted) known first, which
                // is exactly the inference v1 doesn't attempt.
            }
            Stmt::Merge(s) => {
                if let Some(e) = s.master() {
                    self.bind_expr(scope, &e);
                }
                if let Some(e) = s.duplicate() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::RunAs(s) => {
                for e in s.args() {
                    self.bind_expr(scope, &e);
                }
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
            }
            Stmt::LocalVarDecl(s) => {
                if let Some(ty) = s.type_ref() {
                    self.resolve_type_ref(&ty);
                }
                for d in s.declarators() {
                    if let Some(e) = d.init() {
                        self.bind_expr(scope, &e);
                    }
                    if let Some(name) = d.name() {
                        let sym =
                            self.declare_local(SymbolKind::LocalVar, &name, s.type_ref().as_ref());
                        if let Some(text) = name.text() {
                            self.scopes.bind(scope, text, sym);
                        }
                    }
                }
            }
            Stmt::Expr(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
        }
    }

    /// Walks `expr` and every subexpression, resolving references along
    /// the way. Returns `expr`'s inferred [`Ty`], when known -- `None`
    /// otherwise (an expression this binder genuinely can't type at all,
    /// e.g. the result of an unresolved call). A call expression's type
    /// is its resolved method/constructor's declared return type (only
    /// available when overload resolution narrowed to exactly one
    /// candidate).
    pub(crate) fn bind_expr(&mut self, scope: ScopeId, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Literal(lit) => lit.token().and_then(|t| Ty::for_literal(t.kind())),
            Expr::Name(n) => self.bind_name_expr(scope, n),
            Expr::This(_) => self.enclosing_type.map(Ty::Project),
            Expr::Super(_) => self
                .enclosing_type
                .and_then(|t| self.table.direct_super(t))
                .map(Ty::Project),
            Expr::Paren(p) => p.inner().and_then(|inner| self.bind_expr(scope, &inner)),
            Expr::Cast(c) => {
                if let Some(op) = c.operand() {
                    self.bind_expr(scope, &op);
                }
                c.type_ref().and_then(|t| self.resolve_type_ref(&t))
            }
            Expr::Bin(b) => {
                let lhs_ty = b.lhs().and_then(|l| self.bind_expr(scope, &l));
                let rhs_ty = b.rhs().and_then(|r| self.bind_expr(scope, &r));
                let op_text: String = b
                    .operator_tokens()
                    .iter()
                    .map(|t| t.text())
                    .collect::<Vec<_>>()
                    .join("");
                Ty::for_bin_op(&op_text, lhs_ty, rhs_ty)
            }
            Expr::Unary(u) => {
                let operand_ty = u.operand().and_then(|o| self.bind_expr(scope, &o));
                match u.operator().map(|t| t.text().to_string()).as_deref() {
                    Some("!") => Some(Ty::boolean()),
                    // `~`/unary `+`/`-`/prefix `++`/`--` all preserve the
                    // operand's own type.
                    _ => operand_ty,
                }
            }
            Expr::Postfix(p) => {
                // `++`/`--` only -- always preserves the operand's type.
                p.operand().and_then(|o| self.bind_expr(scope, &o))
            }
            Expr::Ternary(t) => {
                if let Some(c) = t.condition() {
                    self.bind_expr(scope, &c);
                }
                let then_ty = t.then_branch().and_then(|e| self.bind_expr(scope, &e));
                let else_ty = t.else_branch().and_then(|e| self.bind_expr(scope, &e));
                // No common-supertype inference when both branches are
                // known but disagree (that needs the same generics/
                // stdlib depth this step deliberately isn't building) --
                // just prefer `then`, falling back to `else` only when
                // `then`'s own type isn't known at all.
                then_ty.or(else_ty)
            }
            Expr::Instanceof(i) => {
                if let Some(o) = i.operand() {
                    self.bind_expr(scope, &o);
                }
                if let Some(t) = i.type_ref() {
                    self.resolve_type_ref(&t);
                }
                Some(Ty::boolean())
            }
            Expr::Field(f) => self.bind_field_expr(scope, f),
            Expr::Index(idx) => {
                if let Some(t) = idx.target() {
                    self.bind_expr(scope, &t);
                }
                if let Some(i) = idx.index() {
                    self.bind_expr(scope, &i);
                }
                // v1 doesn't model List<T>/Map<K,V> element-type
                // inference for indexing syntax (only for the
                // `crate::generics` method-call table `[...]` doesn't go
                // through), so an indexing expression's own type is
                // always unknown.
                None
            }
            Expr::Call(c) => self.bind_call_expr(scope, c),
            Expr::MethodCall(mc) => self.bind_method_call_expr(scope, mc),
            Expr::New(ne) => self.bind_new_expr(scope, ne),
            Expr::Soql(sq) => {
                crate::soql::bind_soql(self, scope, sq);
                None
            }
            Expr::Sosl(ss) => {
                crate::soql::bind_sosl(self, scope, ss);
                None
            }
        }
    }

    fn bind_name_expr(&mut self, scope: ScopeId, n: &NameExpr) -> Option<Ty> {
        if let Some(ty) = n.type_ref() {
            // The `List<Foo>.class` reflection form -- a type reference,
            // not a name lookup.
            return self.resolve_type_ref(&ty);
        }
        let tok = n.name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, n.syntax());

        if let Some(local) = self.scopes.resolve_local(scope, name) {
            self.refs.set(ptr, Resolution::Resolved(local));
            return self.type_of_symbol(local);
        }

        let Some(container) = self.enclosing_type else {
            self.refs.set(ptr, Resolution::Unresolved);
            return None;
        };
        // Methods/constructors are excluded from plain-name resolution:
        // a bare `NameExpr` naming a method with no call wouldn't
        // compile in real Apex, and including them here would let a
        // field and a same-named method collide in the candidate set
        // for no reason -- `CallExpr`/`MethodCallExpr` handle the
        // call-position case separately.
        let members: Vec<SymbolId> = self
            .table
            .lookup_member(container, name)
            .into_iter()
            .filter(|&id| {
                !matches!(
                    self.table.get(id).kind,
                    SymbolKind::Method | SymbolKind::Constructor
                ) && self.table.is_visible_from(id, self.enclosing_type)
            })
            .collect();
        match members.as_slice() {
            [] => {
                self.refs.set(ptr, Resolution::Unresolved);
                None
            }
            [one] => {
                self.refs.set(ptr, Resolution::Resolved(*one));
                self.type_of_symbol(*one)
            }
            many => {
                self.refs.set(ptr, Resolution::Candidates(many.to_vec()));
                None
            }
        }
    }

    fn bind_field_expr(&mut self, scope: ScopeId, f: &FieldExpr) -> Option<Ty> {
        let target_type = f.target().and_then(|t| self.bind_expr(scope, &t));
        let tok = f.member_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, f.syntax());

        let Some(Ty::Project(container)) = target_type else {
            // `target_type` is either entirely unknown or a `Ty::System`
            // -- a system/schema type this binder has no member model
            // for (see `Ty`'s doc comment) -- either way, honestly
            // `Unresolved` rather than a guess.
            self.refs.set(ptr, Resolution::Unresolved);
            return None;
        };
        let members: Vec<SymbolId> = self
            .table
            .lookup_member(container, name)
            .into_iter()
            .filter(|&id| {
                !matches!(
                    self.table.get(id).kind,
                    SymbolKind::Method | SymbolKind::Constructor
                ) && self.table.is_visible_from(id, self.enclosing_type)
            })
            .collect();
        match members.as_slice() {
            [] => {
                self.refs.set(ptr, Resolution::Unresolved);
                None
            }
            [one] => {
                self.refs.set(ptr, Resolution::Resolved(*one));
                self.type_of_symbol(*one)
            }
            many => {
                self.refs.set(ptr, Resolution::Candidates(many.to_vec()));
                None
            }
        }
    }

    fn bind_method_call_expr(&mut self, scope: ScopeId, mc: &MethodCallExpr) -> Option<Ty> {
        let target_type = mc.target().and_then(|t| self.bind_expr(scope, &t));
        let mut arg_types = Vec::new();
        if let Some(args) = mc.args() {
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
        }
        let tok = mc.method_name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, mc.syntax());

        match target_type {
            Some(Ty::Project(container)) => {
                let methods: Vec<SymbolId> = self
                    .table
                    .lookup_member(container, name)
                    .into_iter()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Method
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                let resolution = narrow_by_overload(self.table, methods, &arg_types);
                let result_type = match &resolution {
                    Resolution::Resolved(id) => self.type_of_symbol(*id),
                    _ => None,
                };
                self.refs.set(ptr, resolution);
                result_type
            }
            Some(Ty::System { name: base, args }) => {
                // No `SymbolId` backs a built-in generic method -- there's
                // no real declaration for goto-definition to point at --
                // so the *reference* stays honestly `Unresolved` even
                // when the *type* it returns is known (see
                // `crate::generics`'s module doc comment). The returned
                // `Ty` still flows upward for further chaining, e.g.
                // `myList.get(0).Name` resolving `Name` when the list's
                // element type is project-local.
                self.refs.set(ptr, Resolution::Unresolved);
                crate::generics::builtin_generic_member_type(&base, &args, name)
            }
            None => {
                self.refs.set(ptr, Resolution::Unresolved);
                None
            }
        }
    }

    fn bind_call_expr(&mut self, scope: ScopeId, c: &CallExpr) -> Option<Ty> {
        let mut arg_types = Vec::new();
        if let Some(args) = c.args() {
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
        }
        let tok = c.callee_token()?;
        let ptr = SyntaxPtr::new(self.file, c.syntax());

        let (target_type, want_ctor) = match tok.kind() {
            SyntaxKind::This => (self.enclosing_type, true),
            SyntaxKind::Super => (
                self.enclosing_type.and_then(|t| self.table.direct_super(t)),
                true,
            ),
            _ => (self.enclosing_type, false),
        };
        let Some(container) = target_type else {
            self.refs.set(ptr, Resolution::Unresolved);
            return None;
        };

        let candidates: Vec<SymbolId> = if want_ctor {
            // `this(...)`/`super(...)` chain to a constructor, whose
            // declared name equals the class name -- never "this"/
            // "super" -- so this takes every constructor directly rather
            // than a name-based `lookup_member`.
            self.table
                .members_of(container)
                .iter()
                .copied()
                .filter(|&id| {
                    self.table.get(id).kind == SymbolKind::Constructor
                        && self.table.is_visible_from(id, self.enclosing_type)
                })
                .collect()
        } else {
            let name = tok.text();
            self.table
                .lookup_member(container, name)
                .into_iter()
                .filter(|&id| {
                    self.table.get(id).kind == SymbolKind::Method
                        && self.table.is_visible_from(id, self.enclosing_type)
                })
                .collect()
        };

        let resolution = narrow_by_overload(self.table, candidates, &arg_types);
        // Constructor symbols never carry a `type_name` (see
        // `collect::collect_constructor`), so `type_of_symbol` is `None`
        // for the `this(...)`/`super(...)` case without needing a
        // separate `want_ctor` guard here.
        let result_type = match &resolution {
            Resolution::Resolved(id) => self.type_of_symbol(*id),
            _ => None,
        };
        self.refs.set(ptr, resolution);
        result_type
    }

    fn bind_new_expr(&mut self, scope: ScopeId, ne: &NewExpr) -> Option<Ty> {
        let resolved_type = ne.type_ref().and_then(|t| self.resolve_type_ref(&t));
        if let Some(args) = ne.args() {
            let mut arg_types = Vec::new();
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
            if let Some(&Ty::Project(container)) = resolved_type.as_ref() {
                let ctors: Vec<SymbolId> = self
                    .table
                    .members_of(container)
                    .iter()
                    .copied()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Constructor
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                if !ctors.is_empty() {
                    let resolution = narrow_by_overload(self.table, ctors, &arg_types);
                    self.refs
                        .set(SyntaxPtr::new(self.file, ne.syntax()), resolution);
                }
            }
        }
        if let Some(size) = ne.array_size() {
            self.bind_expr(scope, &size);
        }
        if let Some(init) = ne.initializer() {
            self.bind_initializer_expr(scope, &init);
        }
        resolved_type
    }

    fn bind_initializer_expr(&mut self, scope: ScopeId, init: &Initializer) {
        match init {
            Initializer::Array(a) => {
                for e in a.elements() {
                    self.bind_expr(scope, &e);
                }
            }
            Initializer::Set(s) => {
                for e in s.elements() {
                    self.bind_expr(scope, &e);
                }
            }
            Initializer::Map(m) => {
                for entry in m.entries() {
                    if let Some(k) = entry.key() {
                        self.bind_expr(scope, &k);
                    }
                    if let Some(v) = entry.value() {
                        self.bind_expr(scope, &v);
                    }
                }
            }
        }
    }
}
