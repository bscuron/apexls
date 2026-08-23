//! Lossless (trivia-preserving) concrete syntax tree for Apex, plus a typed
//! AST layer built as a cheap traversal over it (Roslyn/rust-analyzer style
//! red-green tree). This is what makes exact source round-trip possible.

mod syntax_kind;

pub mod ast;

pub use syntax_kind::{ApexLanguage, SyntaxKind};

pub type SyntaxNode = rowan::SyntaxNode<ApexLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<ApexLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<ApexLanguage>;
pub type SyntaxNodeChildren = rowan::SyntaxNodeChildren<ApexLanguage>;
pub type SyntaxElementChildren = rowan::SyntaxElementChildren<ApexLanguage>;
pub type GreenNode = rowan::GreenNode;
pub type GreenNodeBuilder<'a> = rowan::GreenNodeBuilder<'a>;

pub use rowan::ast::AstNode;
pub use rowan::NodeOrToken;

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `1 + 2` as a `BinExpr(LiteralExpr, Add, LiteralExpr)` by
    /// hand (no parser involved) and checks the `Expr` dispatch enum
    /// casts/rejects correctly -- `apex-parser`'s tests exercise this
    /// against real parsed trees; this just checks the macro-generated
    /// `AstNode` impl itself is wired up right.
    #[test]
    fn expr_cast_dispatches_by_kind() {
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

        let bin = ast::Expr::cast(root.clone()).expect("BinExpr should cast to Expr");
        assert!(matches!(bin, ast::Expr::Bin(_)));

        let lit_node = root.first_child().unwrap();
        let lit = ast::Expr::cast(lit_node).expect("LiteralExpr should cast to Expr");
        assert!(matches!(lit, ast::Expr::Literal(_)));

        // A token (not a node) can't cast at all -- can_cast only ever
        // sees node kinds coming from `SyntaxNode::kind()`.
        assert!(!ast::Expr::can_cast(SyntaxKind::Add));
    }
}
