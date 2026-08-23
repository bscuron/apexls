//! Typed AST layer over the raw [`crate::SyntaxNode`] tree: thin
//! `rowan::ast::AstNode` wrappers, one dispatch enum per grammar
//! category. `decl` (declarations: classes/interfaces/enums/triggers/
//! members), `expr` (expressions), and `stmt` (statements) are built out
//! with real per-node accessors (`BinExpr::lhs()`, `IfStmt::condition()`,
//! ...) rather than just `AstNode` casting, since a symbol-table binder
//! and an LSP built on top of it both need to walk into real operands
//! and sub-statements, not just identify a node's kind.

use crate::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::AstNode;

pub mod decl;
pub mod expr;
pub mod stmt;

pub use expr::Expr;
pub use stmt::{Block, Stmt};

/// Each variant holds the specific typed wrapper its name suggests
/// (`Member::NestedClass` holds a `ClassDecl`, not a raw `SyntaxNode`),
/// matching rust-analyzer's equivalent macro -- callers get that
/// variant's own accessors immediately after a `match`, no re-cast
/// needed. Every `$ty` must already have its own `ast_node!`/
/// `dispatch_enum!`-generated `AstNode` impl; Rust's item-order
/// independence within a module means the two macro invocations for a
/// type and the dispatch enum that names it can appear in either order.
macro_rules! dispatch_enum {
    ($enum_name:ident { $($variant:ident($ty:ty) => $kind:ident),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $enum_name {
            $($variant($ty),)*
        }

        impl AstNode for $enum_name {
            type Language = ApexLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                matches!(kind, $(SyntaxKind::$kind)|*)
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                match node.kind() {
                    $(SyntaxKind::$kind => Some($enum_name::$variant(<$ty>::cast(node)?)),)*
                    _ => None,
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $($enum_name::$variant(inner) => inner.syntax(),)*
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

ast_node!(Name, DeclName);
ast_node!(Type, Type);
ast_node!(QualifiedName, QualifiedName);

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

/// All direct non-trivia token children of `node`, in source order --
/// the general-purpose accessor behind [`first_non_trivia_token`]/
/// [`last_non_trivia_token`], and used directly wherever a node can hold
/// more than one significant token with no children of its own between
/// them (e.g. `BinExpr`'s operator, which is 1-3 tokens: `=`, `<=`
/// merged from `Lt`+`Assign`, `>>>` merged from three `Gt`s, ...). Child
/// *nodes* (an expression's operands, a statement's sub-block, ...) are
/// never tokens, so they're transparently skipped without needing to
/// know where they are positionally.
pub(crate) fn direct_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> + '_ {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| !t.kind().is_trivia())
}

/// The first non-trivia token directly under `node` -- for nodes (like
/// `Name`, or a prefix `UnaryExpr`'s operator) known to lead with exactly
/// one significant token.
pub(crate) fn first_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    direct_tokens(node).next()
}

/// The last non-trivia token directly under `node` -- for nodes (like a
/// `FieldExpr`/`MethodCallExpr`'s member name, always the last direct
/// token after the target expression and the `.`/`?.`) known to trail
/// with exactly one significant token.
pub(crate) fn last_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    direct_tokens(node).last()
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
