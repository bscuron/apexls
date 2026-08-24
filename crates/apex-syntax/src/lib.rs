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
