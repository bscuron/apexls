//! Typed AST layer over the raw [`crate::SyntaxNode`] tree: thin
//! `rowan::ast::AstNode` wrappers, one dispatch enum per grammar
//! category. Built out incrementally alongside whatever actually needs
//! typed access (currently: the parenthesization metamorphic test, which
//! needs to find and wrap `Expr` sub-nodes) rather than exhaustively
//! ahead of it -- per-variant field accessors (e.g. `BinExpr::lhs()`) can
//! be added the same way as they become needed.

use crate::{ApexLanguage, SyntaxKind, SyntaxNode};
use rowan::ast::AstNode;

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
    }
}
