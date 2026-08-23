//! Pass 2: scope building + reference resolution. Walks one method/
//! constructor/property-accessor body (or a bare field/property
//! initializer expression) building a [`ScopeTree`] alongside a
//! [`Stmt`]/[`Expr`] walk, resolving references as they're encountered.
//!
//! Runs only after Pass 1.5 (`crate::inherit`) has finished, so member/
//! inherited-chain lookups against the whole project are always safe --
//! unlike Pass 1, this isn't embarrassingly parallel across bodies in
//! this version, since [`BodyBinder`] allocates new [`SymbolId`]s for
//! locals directly into the shared [`SymbolTable`] as it walks (the same
//! arena Pass 1 populated), rather than a separate local-then-remap
//! step. Parallelizing this later would need that same local-collect-
//! then-merge trick Pass 1 already uses for symbols.
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
//! - Method calls (qualified or not) never resolve to a single
//!   `SymbolId`, even when only one same-name method exists -- v1 does
//!   no argument-type/arity overload resolution, so every method-call
//!   reference is `Candidates` or `Unresolved`, never `Resolved`.

use crate::file_id::FileId;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::reference_table::{ReferenceTable, Resolution};
use crate::schema_index::SchemaIndex;
use crate::scope::{ScopeId, ScopeKind, ScopeTree};
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind};
use crate::symbol_table::SymbolTable;
use apex_syntax::ast::decl::TriggerBlock;
use apex_syntax::ast::expr::{CallExpr, FieldExpr, Initializer, MethodCallExpr, NameExpr, NewExpr};
use apex_syntax::ast::stmt::Block;
use apex_syntax::ast::{Expr, Name, Stmt, Type};
use apex_syntax::SyntaxKind;
use rowan::ast::AstNode;

/// One method/constructor/property-accessor body's binding context:
/// shared read-only project state (`table`* is `&mut` only because
/// locals get allocated into it as they're discovered; every *lookup*
/// against already-collected symbols is logically read-only),
/// write-only output (`refs`), and the scope chain being built as the
/// walk descends.
pub(crate) struct BodyBinder<'a> {
    pub(crate) table: &'a mut SymbolTable,
    pub(crate) schema: &'a SchemaIndex,
    pub(crate) refs: &'a mut ReferenceTable,
    pub(crate) scopes: ScopeTree,
    file: FileId,
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
/// statement/expression, and returns the finished tree.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bind_body(
    table: &mut SymbolTable,
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    enclosing_member: Option<SymbolId>,
    params: &[SymbolId],
    block: &Block,
) -> ScopeTree {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs,
        scopes,
        file,
        enclosing_type,
        enclosing_member,
    };
    for &p in params {
        let name = binder.table.get(p).name.clone();
        binder.scopes.bind(root_scope, name, p);
    }
    binder.bind_block_stmts(root_scope, block);
    binder.scopes
}

/// Binds a `TriggerBlock`'s bare top-level statements -- the trigger's
/// actual executable body, which `TriggerBlock::members()` (declaration-
/// shaped children only) never reaches. Walks direct `Stmt` children
/// alongside (interleaved with, in source order) the `Member`
/// declarations Pass 1 already collected separately.
pub(crate) fn bind_trigger_body(
    table: &mut SymbolTable,
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    block: &TriggerBlock,
) -> ScopeTree {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs,
        scopes,
        file,
        enclosing_type,
        enclosing_member: None,
    };
    for child in block.syntax().children() {
        if let Some(stmt) = Stmt::cast(child) {
            binder.bind_stmt(root_scope, &stmt);
        }
    }
    binder.scopes
}

/// Binds a bare expression with no enclosing statement context (a
/// field/property initializer) -- member lookup only, no locals, no
/// `ScopeTree` worth keeping around afterward.
pub(crate) fn bind_initializer(
    table: &mut SymbolTable,
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    expr: &Expr,
) {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, expr.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs,
        scopes,
        file,
        enclosing_type,
        enclosing_member: None,
    };
    binder.bind_expr(root_scope, expr);
}

impl<'a> BodyBinder<'a> {
    fn declare_local(
        &mut self,
        kind: SymbolKind,
        name: &Name,
        type_ref: Option<&Type>,
    ) -> SymbolId {
        let (type_ptr, type_name) = match type_ref {
            Some(ty) => (Some(AstPtr::new(ty)), Some(ty.text())),
            None => (None, None),
        };
        let symbol = Symbol {
            kind,
            name: name.text().unwrap_or_default(),
            file: self.file,
            ptr: SyntaxPtr::new(name.syntax()),
            name_range: name.syntax().text_range(),
            container: self.enclosing_member,
            type_ref: type_ptr,
            type_name,
            modifiers: ModifierSet::default(),
        };
        self.table.alloc(symbol)
    }

    /// The project-local type symbol `symbol`'s declared type resolves
    /// to, if any -- the one-hop "type of this expression" chaining
    /// `FieldExpr`/`MethodCallExpr` target resolution needs. `None` for
    /// a symbol with no type, or whose type isn't a project-local class/
    /// interface/enum (a schema-object-typed or unmodeled system type).
    fn type_of_symbol(&self, id: SymbolId) -> Option<SymbolId> {
        let type_name = self.table.get(id).type_name.as_deref()?;
        self.table.top_level(type_name)
    }

    /// Resolves a plain Apex `Type` reference (a field/param/local/
    /// return type, a `new`/`instanceof`/cast target, ...) against
    /// project-local types first, then `apex-metadata`'s schema (an
    /// SObject-typed declaration, e.g. `Account a;`). Returns the
    /// project-local type symbol, if that's what it resolved to -- the
    /// "one hop" other resolvers chain through.
    pub(crate) fn resolve_type_ref(&mut self, ty: &Type) -> Option<SymbolId> {
        let name = ty.text();
        let ptr = SyntaxPtr::new(ty.syntax());
        if let Some(id) = self.table.top_level(&name) {
            self.refs.set(ptr, Resolution::Resolved(id));
            return Some(id);
        }
        if self.schema.object(&name).is_some() {
            self.refs.set(
                ptr,
                Resolution::SchemaObject {
                    object: name,
                    field: None,
                },
            );
            return None;
        }
        // Could be a standard object this repo never locally extended
        // (indistinguishable, using `apex-metadata` alone, from a
        // genuinely nonexistent name), or an unmodeled system/library
        // type (`String`, `List`, an `Exception` subtype, ...) -- v1
        // can't tell these apart, so both land here as `Unresolved`
        // rather than one of them being misreported as `UnknownSchema`.
        self.refs.set(ptr, Resolution::Unresolved);
        None
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
                        let ptr = SyntaxPtr::new(ty.syntax());
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
    /// the way. Returns the project-local type symbol `expr`'s static
    /// type resolves to, when that's known without inference (see the
    /// module doc comment) -- `None` otherwise, including for every
    /// method-call expression (v1 never infers a call's return type).
    pub(crate) fn bind_expr(&mut self, scope: ScopeId, expr: &Expr) -> Option<SymbolId> {
        match expr {
            Expr::Literal(_) => None,
            Expr::Name(n) => self.bind_name_expr(scope, n),
            Expr::This(_) => self.enclosing_type,
            Expr::Super(_) => self.enclosing_type.and_then(|t| self.table.direct_super(t)),
            Expr::Paren(p) => p.inner().and_then(|inner| self.bind_expr(scope, &inner)),
            Expr::Cast(c) => {
                if let Some(op) = c.operand() {
                    self.bind_expr(scope, &op);
                }
                c.type_ref().and_then(|t| self.resolve_type_ref(&t))
            }
            Expr::Bin(b) => {
                if let Some(l) = b.lhs() {
                    self.bind_expr(scope, &l);
                }
                if let Some(r) = b.rhs() {
                    self.bind_expr(scope, &r);
                }
                None
            }
            Expr::Unary(u) => {
                if let Some(o) = u.operand() {
                    self.bind_expr(scope, &o);
                }
                None
            }
            Expr::Postfix(p) => {
                if let Some(o) = p.operand() {
                    self.bind_expr(scope, &o);
                }
                None
            }
            Expr::Ternary(t) => {
                if let Some(c) = t.condition() {
                    self.bind_expr(scope, &c);
                }
                let then_ty = t.then_branch().and_then(|e| self.bind_expr(scope, &e));
                if let Some(e) = t.else_branch() {
                    self.bind_expr(scope, &e);
                }
                then_ty
            }
            Expr::Instanceof(i) => {
                if let Some(o) = i.operand() {
                    self.bind_expr(scope, &o);
                }
                if let Some(t) = i.type_ref() {
                    self.resolve_type_ref(&t);
                }
                None
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
                // inference, so an indexing expression's own type is
                // always unknown.
                None
            }
            Expr::Call(c) => {
                self.bind_call_expr(scope, c);
                None
            }
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

    fn bind_name_expr(&mut self, scope: ScopeId, n: &NameExpr) -> Option<SymbolId> {
        if let Some(ty) = n.type_ref() {
            // The `List<Foo>.class` reflection form -- a type reference,
            // not a name lookup.
            return self.resolve_type_ref(&ty);
        }
        let tok = n.name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(n.syntax());

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
                )
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

    fn bind_field_expr(&mut self, scope: ScopeId, f: &FieldExpr) -> Option<SymbolId> {
        let target_type = f.target().and_then(|t| self.bind_expr(scope, &t));
        let tok = f.member_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(f.syntax());

        let Some(container) = target_type else {
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
                )
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

    fn bind_method_call_expr(&mut self, scope: ScopeId, mc: &MethodCallExpr) -> Option<SymbolId> {
        let target_type = mc.target().and_then(|t| self.bind_expr(scope, &t));
        if let Some(args) = mc.args() {
            for a in args.args() {
                self.bind_expr(scope, &a);
            }
        }
        let tok = mc.method_name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(mc.syntax());

        let Some(container) = target_type else {
            self.refs.set(ptr, Resolution::Unresolved);
            return None;
        };
        let methods: Vec<SymbolId> = self
            .table
            .lookup_member(container, name)
            .into_iter()
            .filter(|&id| self.table.get(id).kind == SymbolKind::Method)
            .collect();
        if methods.is_empty() {
            self.refs.set(ptr, Resolution::Unresolved);
        } else {
            // Never `Resolved`, even for a single same-name match -- v1
            // doesn't attempt argument-type/arity overload resolution
            // (see the module doc comment).
            self.refs.set(ptr, Resolution::Candidates(methods));
        }
        None
    }

    fn bind_call_expr(&mut self, scope: ScopeId, c: &CallExpr) {
        if let Some(args) = c.args() {
            for a in args.args() {
                self.bind_expr(scope, &a);
            }
        }
        let Some(tok) = c.callee_token() else {
            return;
        };
        let ptr = SyntaxPtr::new(c.syntax());

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
            return;
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
                .filter(|&id| self.table.get(id).kind == SymbolKind::Constructor)
                .collect()
        } else {
            let name = tok.text();
            self.table
                .lookup_member(container, name)
                .into_iter()
                .filter(|&id| self.table.get(id).kind == SymbolKind::Method)
                .collect()
        };

        if candidates.is_empty() {
            self.refs.set(ptr, Resolution::Unresolved);
        } else {
            self.refs.set(ptr, Resolution::Candidates(candidates));
        }
    }

    fn bind_new_expr(&mut self, scope: ScopeId, ne: &NewExpr) -> Option<SymbolId> {
        let resolved_type = ne.type_ref().and_then(|t| self.resolve_type_ref(&t));
        if let Some(args) = ne.args() {
            for a in args.args() {
                self.bind_expr(scope, &a);
            }
            if let Some(container) = resolved_type {
                let ctors: Vec<SymbolId> = self
                    .table
                    .members_of(container)
                    .iter()
                    .copied()
                    .filter(|&id| self.table.get(id).kind == SymbolKind::Constructor)
                    .collect();
                if !ctors.is_empty() {
                    self.refs
                        .set(SyntaxPtr::new(ne.syntax()), Resolution::Candidates(ctors));
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
