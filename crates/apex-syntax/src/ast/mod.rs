//! Typed AST layer over the raw [`crate::SyntaxNode`] tree: thin
//! `rowan::ast::AstNode` wrappers, one dispatch enum per grammar
//! category. Built out incrementally alongside whatever actually needs
//! typed access -- per-variant field accessors (e.g. `BinExpr::lhs()`)
//! can be added the same way as they become needed. `decl` (declarations:
//! classes/interfaces/enums/triggers/members) is the first grammar area
//! built out beyond `Expr`, since a symbol-table binder needs typed
//! access to walk declarations before it can do anything else.

use crate::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::AstNode;

pub mod decl;

/// Each variant holds the raw `SyntaxNode`, not the more specific typed
/// wrapper its name suggests (`Member::NestedClass` holds a `SyntaxNode`,
/// not a `ClassDecl`) -- callers that need that variant's own accessors
/// re-cast it (`ClassDecl::cast(node)`), same as rust-analyzer's
/// equivalent macro. Kept this way rather than holding typed payloads so
/// this macro doesn't require every dispatch target to already have its
/// own `ast_node!`/`dispatch_enum!` wrapper defined first.
macro_rules! dispatch_enum {
    ($enum_name:ident { $($variant:ident => $kind:ident),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $enum_name {
            $($variant(SyntaxNode),)*
        }

        impl AstNode for $enum_name {
            type Language = ApexLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                matches!(kind, $(SyntaxKind::$kind)|*)
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                match node.kind() {
                    $(SyntaxKind::$kind => Some($enum_name::$variant(node)),)*
                    _ => None,
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $($enum_name::$variant(node) => node,)*
                }
            }
        }
    };
}

/// Generates a single-`SyntaxKind` `AstNode` wrapper -- the non-dispatch
/// counterpart of `dispatch_enum!` above, for node kinds that only ever
/// mean one thing (`ClassBody`, `FormalParam`, ...) rather than one of a
/// family of alternatives.
macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            type Language = ApexLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                if Self::can_cast(node.kind()) {
                    Some(Self(node))
                } else {
                    None
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

pub(crate) use ast_node;
pub(crate) use dispatch_enum;

dispatch_enum! {
    Expr {
        Literal => LiteralExpr,
        Name => NameExpr,
        This => ThisExpr,
        Super => SuperExpr,
        Paren => ParenExpr,
        Cast => CastExpr,
        Bin => BinExpr,
        Unary => UnaryExpr,
        Postfix => PostfixExpr,
        Ternary => TernaryExpr,
        Instanceof => InstanceofExpr,
        Field => FieldExpr,
        Index => IndexExpr,
        Call => CallExpr,
        MethodCall => MethodCallExpr,
        New => NewExpr,
        Soql => SoqlExpr,
        Sosl => SoslExpr,
    }
}

ast_node!(Name, DeclName);
ast_node!(Type, Type);
ast_node!(QualifiedName, QualifiedName);
ast_node!(Block, Block);

impl Name {
    /// The single identifier-shaped token a `Name` node wraps -- may be
    /// `Identifier` or any of the many keyword tokens that double as
    /// valid declared names (see `grammar::ids`), so this can't look for
    /// one specific `SyntaxKind`.
    pub fn token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    pub fn text(&self) -> Option<String> {
        self.token().map(|t| t.text().to_string())
    }
}

/// The first non-trivia token directly under `node` -- the general-
/// purpose accessor for nodes (like `Name`) that are known to wrap
/// exactly one significant token, whichever kind it happens to be.
pub(crate) fn first_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| !t.kind().is_trivia())
}

/// The first non-trivia token strictly after the first occurrence of a
/// `kind` token among `node`'s direct children -- used for reference
/// positions that immediately follow a fixed keyword but aren't declared
/// names themselves (e.g. a `TriggerUnit`'s `ON <object>` SObject
/// reference), so don't get their own dedicated node kind.
pub(crate) fn token_after(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    let mut seen = false;
    for elem in node.children_with_tokens() {
        if seen {
            if let Some(tok) = elem.as_token() {
                if !tok.kind().is_trivia() {
                    return Some(tok.clone());
                }
            }
        } else if elem.as_token().is_some_and(|t| t.kind() == kind) {
            seen = true;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GreenNodeBuilder;

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

        let bin = Expr::cast(root.clone()).expect("BinExpr should cast to Expr");
        assert!(matches!(bin, Expr::Bin(_)));

        let lit_node = root.first_child().unwrap();
        let lit = Expr::cast(lit_node).expect("LiteralExpr should cast to Expr");
        assert!(matches!(lit, Expr::Literal(_)));

        // A token (not a node) can't cast at all -- can_cast only ever
        // sees node kinds coming from `SyntaxNode::kind()`.
        assert!(!Expr::can_cast(SyntaxKind::Add));
    }
}
