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
pub mod soql;
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

/// The `DocComment` token immediately preceding `node`'s real content, if
/// any. Trivia attaches to whatever token it's leading trivia *for*, and
/// that token can end up nested a level or two inside `node` (a
/// declaration's doc comment ends up inside its first `Modifier`/
/// `Annotation` child, not as a direct child of the declaration itself,
/// since that's whatever node is open when the sink processes the
/// trivia run right before the first real token) -- so this walks the
/// whole subtree in document order via `descendants_with_tokens()`
/// rather than just direct children, stopping at the first non-trivia
/// token found anywhere. Only `/** ... */`-style comments count (Apex/
/// Java convention); an unrelated `//` comment or blank line further
/// back is walked past, and the *last* `DocComment` immediately before
/// the real content wins if more than one appears in the run.
pub(crate) fn doc_comment_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let mut found = None;
    for elem in node.descendants_with_tokens() {
        let Some(t) = elem.as_token() else {
            continue; // a node boundary (including `node` itself) -- keep descending
        };
        if t.kind() == SyntaxKind::DocComment {
            found = Some(t.clone());
        } else if !t.kind().is_trivia() {
            break; // first real token anywhere in the subtree -- stop
        }
    }
    found
}

/// Strips a `/** ... */` doc comment down to its content: the `/**`/`*/`
/// delimiters, each line's leading `*` (Javadoc/ApexDoc convention), and
/// leading/trailing blank lines. A line that's *entirely* `*` characters
/// (the `***...***` divider style common in NPSP-style headers) is
/// treated as blank rather than left as a run of stars in the output.
pub(crate) fn clean_doc_comment(text: &str) -> String {
    let inner = text.strip_prefix("/**").unwrap_or(text);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);

    let lines: Vec<String> = inner
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if !trimmed.is_empty() && trimmed.chars().all(|c| c == '*') {
                return String::new();
            }
            let trimmed = trimmed.strip_prefix('*').unwrap_or(trimmed);
            let trimmed = trimmed.strip_prefix(' ').unwrap_or(trimmed);
            trimmed.trim_end().to_string()
        })
        .collect();

    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    lines[start.min(end)..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::clean_doc_comment;

    #[test]
    fn cleans_a_single_line_doc_comment() {
        assert_eq!(
            clean_doc_comment("/** Does something. */"),
            "Does something."
        );
    }

    #[test]
    fn cleans_a_multi_line_javadoc_style_comment() {
        let raw = "/**\n * Does something.\n * @param x the value\n */";
        assert_eq!(
            clean_doc_comment(raw),
            "Does something.\n@param x the value"
        );
    }

    #[test]
    fn treats_a_star_divider_line_as_blank() {
        let raw = "/***************\n* @description Blah\n***************/";
        assert_eq!(clean_doc_comment(raw), "@description Blah");
    }

    #[test]
    fn empty_doc_comment_cleans_to_empty_string() {
        assert_eq!(clean_doc_comment("/**\n *\n */"), "");
        assert_eq!(clean_doc_comment("/***/"), "");
    }
}
