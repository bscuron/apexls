//! Typed AST wrappers for statement grammar (`grammar::statements`),
//! with real per-node accessors, matching `ast::expr`'s approach.

use super::decl::{HasModifiers, VarDeclarator};
use super::{ast_node, dispatch_enum, Expr, Name, Type};
use crate::{ApexLanguage, SyntaxKind, SyntaxNode};
use rowan::ast::{support, AstChildren, AstNode};

dispatch_enum! {
    Stmt {
        Block(Block) => Block,
        If(IfStmt) => IfStmt,
        Switch(SwitchStmt) => SwitchStmt,
        For(ForStmt) => ForStmt,
        ForEach(ForEachStmt) => ForEachStmt,
        While(WhileStmt) => WhileStmt,
        DoWhile(DoWhileStmt) => DoWhileStmt,
        Try(TryStmt) => TryStmt,
        Return(ReturnStmt) => ReturnStmt,
        Throw(ThrowStmt) => ThrowStmt,
        Break(BreakStmt) => BreakStmt,
        Continue(ContinueStmt) => ContinueStmt,
        Insert(InsertStmt) => InsertStmt,
        Update(UpdateStmt) => UpdateStmt,
        Delete(DeleteStmt) => DeleteStmt,
        Undelete(UndeleteStmt) => UndeleteStmt,
        Upsert(UpsertStmt) => UpsertStmt,
        Merge(MergeStmt) => MergeStmt,
        RunAs(RunAsStmt) => RunAsStmt,
        LocalVarDecl(LocalVarDeclStmt) => LocalVarDeclStmt,
        Expr(ExprStmt) => ExprStmt,
    }
}

ast_node!(Block, Block);
ast_node!(IfStmt, IfStmt);
ast_node!(SwitchStmt, SwitchStmt);
ast_node!(WhenClause, WhenClause);
ast_node!(WhenValue, WhenValue);
ast_node!(WhenLiteral, WhenLiteral);
ast_node!(ForStmt, ForStmt);
ast_node!(ForEachStmt, ForEachStmt);
ast_node!(ForInit, ForInit);
ast_node!(ForUpdate, ForUpdate);
ast_node!(WhileStmt, WhileStmt);
ast_node!(DoWhileStmt, DoWhileStmt);
ast_node!(TryStmt, TryStmt);
ast_node!(CatchClause, CatchClause);
ast_node!(FinallyClause, FinallyClause);
ast_node!(ReturnStmt, ReturnStmt);
ast_node!(ThrowStmt, ThrowStmt);
ast_node!(BreakStmt, BreakStmt);
ast_node!(ContinueStmt, ContinueStmt);
ast_node!(AccessLevelClause, AccessLevelClause);
ast_node!(InsertStmt, InsertStmt);
ast_node!(UpdateStmt, UpdateStmt);
ast_node!(DeleteStmt, DeleteStmt);
ast_node!(UndeleteStmt, UndeleteStmt);
ast_node!(UpsertStmt, UpsertStmt);
ast_node!(MergeStmt, MergeStmt);
ast_node!(RunAsStmt, RunAsStmt);
ast_node!(LocalVarDeclStmt, LocalVarDeclStmt);
ast_node!(ExprStmt, ExprStmt);

impl HasModifiers for CatchClause {}

impl Block {
    pub fn statements(&self) -> AstChildren<Stmt> {
        support::children(self.syntax())
    }
}

impl IfStmt {
    /// Not wrapped in its own node -- `if (cond)`'s parens are bare
    /// tokens, direct children of `IfStmt` alongside everything else
    /// (see `grammar::statements::par_expr`).
    pub fn condition(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn then_branch(&self) -> Option<Stmt> {
        support::children(self.syntax()).next()
    }

    /// `None` for an `if` with no `else`, *not* confusable with a
    /// missing `then_branch` -- `then_branch` is never actually absent
    /// in a well-formed tree (a syntax error there still produces a best-
    /// effort `Stmt`), while `else_branch` legitimately often is.
    pub fn else_branch(&self) -> Option<Stmt> {
        support::children::<Stmt>(self.syntax()).nth(1)
    }
}

impl SwitchStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn when_clauses(&self) -> AstChildren<WhenClause> {
        support::children(self.syntax())
    }
}

impl WhenClause {
    pub fn value(&self) -> Option<WhenValue> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl WhenValue {
    pub fn is_else(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Else).is_some()
    }

    /// `Some` only for the type-pattern form (`when Account a`).
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    /// The bound variable name, for the type-pattern form.
    pub fn binding(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// The literal(s), for the literal-list form (`when 1, 2, 3`) --
    /// empty for the other two forms.
    pub fn literals(&self) -> AstChildren<WhenLiteral> {
        support::children(self.syntax())
    }
}

impl ForStmt {
    pub fn init(&self) -> Option<ForInit> {
        support::child(self.syntax())
    }

    /// Not the `ForInit`/`ForUpdate` expressions -- those live inside
    /// their own nodes, so the only bare `Expr` direct child of `ForStmt`
    /// itself is the loop condition.
    pub fn condition(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn update(&self) -> Option<ForUpdate> {
        support::child(self.syntax())
    }

    /// `None` for a bare-`;` body (only legal here and on `while`, per
    /// the reference grammar -- see `grammar::statements`' module doc
    /// comment).
    pub fn body(&self) -> Option<Stmt> {
        support::child(self.syntax())
    }
}

impl ForEachStmt {
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    pub fn iterable(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<Stmt> {
        support::child(self.syntax())
    }
}

impl ForInit {
    /// `Some` only for the local-variable-declaration form.
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    /// Non-empty only for the local-variable-declaration form.
    pub fn declarators(&self) -> AstChildren<VarDeclarator> {
        support::children(self.syntax())
    }

    /// Non-empty only for the expression-list form (`for (i = 0, j = 10;
    /// ...)`) -- never collides with `declarators`' own initializer
    /// expressions, which are children of each `VarDeclarator`, not of
    /// `ForInit` directly.
    pub fn exprs(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }
}

impl ForUpdate {
    pub fn exprs(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }
}

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    /// `None` for a bare-`;` body (see [`ForStmt::body`]'s doc comment).
    pub fn body(&self) -> Option<Stmt> {
        support::child(self.syntax())
    }
}

impl DoWhileStmt {
    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }

    pub fn condition(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl TryStmt {
    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }

    pub fn catch_clauses(&self) -> AstChildren<CatchClause> {
        support::children(self.syntax())
    }

    pub fn finally_clause(&self) -> Option<FinallyClause> {
        support::child(self.syntax())
    }
}

impl CatchClause {
    /// Not a `Type` -- exception types are parsed as a `QualifiedName`
    /// (see `grammar::types::qualified_name`), since a cast-style generic
    /// type argument or array suffix never makes sense there.
    pub fn exception_type(&self) -> Option<super::QualifiedName> {
        support::child(self.syntax())
    }

    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl FinallyClause {
    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl ReturnStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl ThrowStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl AccessLevelClause {
    pub fn is_system(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::System).is_some()
    }

    pub fn is_user(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::User).is_some()
    }
}

/// Shared by the four DML statement kinds whose shape is exactly
/// `KEYWORD accessLevel? expression ';'` (`insert`/`update`/`delete`/
/// `undelete`) -- `upsert` and `merge` each have one extra field, so
/// they get their own `impl` blocks below instead.
macro_rules! simple_dml_stmt {
    ($ty:ident) => {
        impl $ty {
            pub fn access_level(&self) -> Option<AccessLevelClause> {
                support::child(self.syntax())
            }

            pub fn expr(&self) -> Option<Expr> {
                support::child(self.syntax())
            }
        }
    };
}

simple_dml_stmt!(InsertStmt);
simple_dml_stmt!(UpdateStmt);
simple_dml_stmt!(DeleteStmt);
simple_dml_stmt!(UndeleteStmt);

impl UpsertStmt {
    pub fn access_level(&self) -> Option<AccessLevelClause> {
        support::child(self.syntax())
    }

    pub fn expr(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    /// The optional external-ID field reference (`upsert records
    /// MyField__c;`).
    pub fn external_id_field(&self) -> Option<super::QualifiedName> {
        support::child(self.syntax())
    }
}

impl MergeStmt {
    pub fn access_level(&self) -> Option<AccessLevelClause> {
        support::child(self.syntax())
    }

    pub fn master(&self) -> Option<Expr> {
        support::children(self.syntax()).next()
    }

    pub fn duplicate(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(1)
    }
}

impl RunAsStmt {
    pub fn args(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl LocalVarDeclStmt {
    /// `final`/`transient` prefix keywords are bare tokens here (see
    /// `grammar::statements::try_local_var_decl_core`), not wrapped in
    /// `Modifier` nodes the way `grammar::declarations`' field/method
    /// modifiers are -- a local variable can't carry annotations or the
    /// full modifier set, only these two, so there's no `HasModifiers`
    /// impl for this type.
    pub fn is_final(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Final).is_some()
    }

    pub fn is_transient(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Transient).is_some()
    }

    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn declarators(&self) -> AstChildren<VarDeclarator> {
        support::children(self.syntax())
    }
}

impl ExprStmt {
    /// `None` if this statement is entirely the product of error
    /// recovery (see `grammar::statements::expr_stmt`'s doc comment) --
    /// its tokens still exist in the tree, wrapped in an `ErrorNode`
    /// child instead of a parsed `Expr`.
    pub fn expr(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}
