//! Typed AST wrappers for expression grammar (`grammar::expressions`),
//! with real per-node accessors (`BinExpr::lhs()`/`::rhs()`,
//! `CallExpr::callee_token()`/`::args()`, ...) rather than just `AstNode`
//! casting -- hover/goto-definition/etc. all need to walk into actual
//! operands, not just identify a node's kind.
//!
//! Most binary-shaped nodes (`BinExpr`, `FieldExpr`, `MethodCallExpr`,
//! `PostfixExpr`, `UnaryExpr`) have no dedicated node for their operator
//! -- it's just the raw token(s) sitting between (or before/after) their
//! operand node(s) in the tree, exactly as the lexer produced them (see
//! `SyntaxKind`'s doc comment: no synthetic kinds for merged operators
//! like `<=`/shift). Accessors below reflect that directly rather than
//! inventing an operator node the parser never builds.

use super::soql::{SoqlExpr, SoslExpr};
use super::{
    ast_node, direct_tokens, dispatch_enum, first_non_trivia_token, last_non_trivia_token, Type,
};
use crate::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::{support, AstChildren, AstNode};

dispatch_enum! {
    Expr {
        Literal(LiteralExpr) => LiteralExpr,
        Name(NameExpr) => NameExpr,
        This(ThisExpr) => ThisExpr,
        Super(SuperExpr) => SuperExpr,
        Paren(ParenExpr) => ParenExpr,
        Cast(CastExpr) => CastExpr,
        Bin(BinExpr) => BinExpr,
        Unary(UnaryExpr) => UnaryExpr,
        Postfix(PostfixExpr) => PostfixExpr,
        Ternary(TernaryExpr) => TernaryExpr,
        Instanceof(InstanceofExpr) => InstanceofExpr,
        Field(FieldExpr) => FieldExpr,
        Index(IndexExpr) => IndexExpr,
        Call(CallExpr) => CallExpr,
        MethodCall(MethodCallExpr) => MethodCallExpr,
        New(NewExpr) => NewExpr,
        Soql(SoqlExpr) => SoqlExpr,
        Sosl(SoslExpr) => SoslExpr,
    }
}

// A `NewExpr`'s trailing initializer, when it has one shaped like `{...}`
// (as opposed to constructor args `(...)` or an array size `[n]`) --
// which of the three kinds depends on what's inside the braces (see
// `grammar::expressions::brace_init`'s doc comment).
dispatch_enum! {
    Initializer {
        Array(ArrayInitializer) => ArrayInitializer,
        Map(MapInitializer) => MapInitializer,
        Set(SetInitializer) => SetInitializer,
    }
}

ast_node!(LiteralExpr, LiteralExpr);
ast_node!(NameExpr, NameExpr);
ast_node!(ThisExpr, ThisExpr);
ast_node!(SuperExpr, SuperExpr);
ast_node!(ParenExpr, ParenExpr);
ast_node!(CastExpr, CastExpr);
ast_node!(BinExpr, BinExpr);
ast_node!(UnaryExpr, UnaryExpr);
ast_node!(PostfixExpr, PostfixExpr);
ast_node!(TernaryExpr, TernaryExpr);
ast_node!(InstanceofExpr, InstanceofExpr);
ast_node!(FieldExpr, FieldExpr);
ast_node!(IndexExpr, IndexExpr);
ast_node!(CallExpr, CallExpr);
ast_node!(MethodCallExpr, MethodCallExpr);
ast_node!(ArgList, ArgList);
ast_node!(NewExpr, NewExpr);
ast_node!(ArrayInitializer, ArrayInitializer);
ast_node!(MapInitializer, MapInitializer);
ast_node!(MapEntry, MapEntry);
ast_node!(SetInitializer, SetInitializer);

impl LiteralExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }
}

impl NameExpr {
    /// The plain-identifier form (`foo`, `Rollup`, any `id`-shaped
    /// token). `None` for the other form this node can hold -- see
    /// [`Self::type_ref`].
    pub fn name_token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    /// The `List<Foo>`/`Map<K, V>`/`Set<Foo>` form, used only as the base
    /// of the `List<Foo>.class` reflection idiom (`List`/`Map`/`Set`
    /// aren't `id`-shaped, so they can't take the plain-token form
    /// above). `None` for the ordinary identifier form.
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }
}

impl ThisExpr {
    pub fn keyword(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }
}

impl SuperExpr {
    pub fn keyword(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl CastExpr {
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn operand(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl BinExpr {
    pub fn lhs(&self) -> Option<Expr> {
        support::children(self.syntax()).next()
    }

    pub fn rhs(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(1)
    }

    /// The operator, as the lexer produced it -- 1 token for everything
    /// except a merged relational-with-equals (`Lt`/`Gt` + `Assign`, 2
    /// tokens) or shift (`Lt`+`Lt`, `Gt`+`Gt`, or `Gt`+`Gt`+`Gt`, 2-3
    /// tokens). Concatenate `.text()` for the conventional operator
    /// spelling.
    pub fn operator_tokens(&self) -> Vec<SyntaxToken> {
        direct_tokens(self.syntax()).collect()
    }
}

impl UnaryExpr {
    /// The prefix operator (`!`, `~`, `+`, `-`, `++`, `--`).
    pub fn operator(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    /// May itself be another `UnaryExpr` for a chain like `--++x`.
    pub fn operand(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl PostfixExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    /// `++` or `--`.
    pub fn operator(&self) -> Option<SyntaxToken> {
        last_non_trivia_token(self.syntax())
    }
}

impl TernaryExpr {
    pub fn condition(&self) -> Option<Expr> {
        support::children(self.syntax()).next()
    }

    pub fn then_branch(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(1)
    }

    pub fn else_branch(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(2)
    }
}

impl InstanceofExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }
}

impl FieldExpr {
    pub fn target(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    /// The member name -- `anyId`-shaped (see `grammar::ids`), so a bare
    /// token rather than a `Name` node (accessing `x.new` is fine even
    /// though *declaring* something named `new` wouldn't be).
    pub fn member_token(&self) -> Option<SyntaxToken> {
        last_non_trivia_token(self.syntax())
    }

    pub fn is_null_safe(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::QuestionDot).is_some()
    }
}

impl IndexExpr {
    pub fn target(&self) -> Option<Expr> {
        support::children(self.syntax()).next()
    }

    /// `None` for the empty-`[]` array-type-suffix form of the
    /// `Foo[].class` reflection idiom (see `grammar::expressions`'
    /// `expr_primary_chain` doc comment) -- an empty index expression is
    /// never a real index.
    pub fn index(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(1)
    }
}

impl CallExpr {
    /// The callee -- always a bare token (`this`, `super`, or an
    /// `id`-shaped identifier/keyword), never a nested expression, since
    /// this node only covers *unqualified* calls (`foo(...)`,
    /// `this(...)`, `super(...)`); a qualified call (`x.foo(...)`) is a
    /// `MethodCallExpr` instead.
    pub fn callee_token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    pub fn args(&self) -> Option<ArgList> {
        support::child(self.syntax())
    }
}

impl MethodCallExpr {
    pub fn target(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    pub fn method_name_token(&self) -> Option<SyntaxToken> {
        last_non_trivia_token(self.syntax())
    }

    pub fn is_null_safe(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::QuestionDot).is_some()
    }

    pub fn args(&self) -> Option<ArgList> {
        support::child(self.syntax())
    }
}

impl ArgList {
    pub fn args(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }
}

impl NewExpr {
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    /// `Some` only for the constructor-call form (`new Foo(...)`).
    pub fn args(&self) -> Option<ArgList> {
        support::child(self.syntax())
    }

    /// `Some` only for the array-size form (`new Foo[n]`).
    pub fn array_size(&self) -> Option<Expr> {
        support::child(self.syntax())
    }

    /// `Some` for either brace-initializer form (`new Foo[]{...}` or
    /// `new Foo{...}`/`new Map<K,V>{k => v, ...}`).
    pub fn initializer(&self) -> Option<Initializer> {
        support::child(self.syntax())
    }
}

impl ArrayInitializer {
    pub fn elements(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }
}

impl SetInitializer {
    pub fn elements(&self) -> AstChildren<Expr> {
        support::children(self.syntax())
    }
}

impl MapInitializer {
    pub fn entries(&self) -> AstChildren<MapEntry> {
        support::children(self.syntax())
    }
}

impl MapEntry {
    pub fn key(&self) -> Option<Expr> {
        support::children(self.syntax()).next()
    }

    pub fn value(&self) -> Option<Expr> {
        support::children::<Expr>(self.syntax()).nth(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GreenNodeBuilder, SyntaxKind};

    /// Builds `1 + 2` as a `BinExpr(LiteralExpr, Add, LiteralExpr)` by
    /// hand (no parser involved) and checks the `Expr` dispatch enum
    /// casts/rejects correctly, and that `BinExpr::lhs`/`rhs`/
    /// `operator_tokens` resolve the right children -- `apex-parser`'s
    /// own tests exercise this against real parsed trees; this just
    /// checks the macro-generated `AstNode`/`support::child` wiring.
    #[test]
    fn bin_expr_operands_and_operator_resolve() {
        let mut b = GreenNodeBuilder::new();
        b.start_node(SyntaxKind::BinExpr.into());
        b.start_node(SyntaxKind::LiteralExpr.into());
        b.token(SyntaxKind::IntegerLiteral.into(), "1");
        b.finish_node();
        b.token(SyntaxKind::Add.into(), "+");
        b.start_node(SyntaxKind::LiteralExpr.into());
        b.token(SyntaxKind::IntegerLiteral.into(), "2");
        b.finish_node();
        b.finish_node();

        let root = SyntaxNode::new_root(b.finish());
        assert_eq!(root.text().to_string(), "1+2");

        let expr = Expr::cast(root.clone()).expect("BinExpr should cast to Expr");
        let Expr::Bin(bin) = expr else {
            panic!("expected Expr::Bin");
        };

        let lhs = bin.lhs().expect("lhs should resolve");
        assert!(matches!(lhs, Expr::Literal(_)));
        let rhs = bin.rhs().expect("rhs should resolve");
        assert!(matches!(rhs, Expr::Literal(_)));
        let ops = bin.operator_tokens();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].text(), "+");

        // A token (not a node) can't cast at all -- can_cast only ever
        // sees node kinds coming from `SyntaxNode::kind()`.
        assert!(!Expr::can_cast(SyntaxKind::Add));
    }
}
