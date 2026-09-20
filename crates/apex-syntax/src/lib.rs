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
pub type NodeCache = rowan::NodeCache;

pub use rowan::ast::AstNode;
pub use rowan::NodeOrToken;
pub use rowan::{TextRange, TextSize};

/// A node's own *significant* source span: first non-trivia token to last,
/// ignoring the whitespace and comments the tree-builder attaches as
/// children *inside* the node.
///
/// Use this, never the raw `node.text_range()`, for anything that reports
/// a position or splices text. A node's green span can be wider than the
/// construct it names at *both* ends -- the sink starts a node before
/// flushing its first token's leading trivia and flushes trailing trivia
/// before finishing it (see `apex_parser`'s `event` module) -- so a
/// statement preceded by a comment reports the comment's position, and an
/// edit computed from the raw range eats the comment and the newline after
/// it. [`ast::Name::ident_range`] documents the same hazard for the one
/// case that hit it first; this is the general form.
///
/// Returns `None` only for a node with no non-trivia tokens at all (an
/// empty file, or a node that is pure trivia).
pub fn significant_range(node: &SyntaxNode) -> Option<TextRange> {
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia());
    let first = tokens.next()?;
    let last = tokens.last().unwrap_or_else(|| first.clone());
    Some(TextRange::new(
        first.text_range().start(),
        last.text_range().end(),
    ))
}
