//! Parse errors and the `Parse` result type every entry point returns.
//!
//! Nothing in this crate panics on malformed *input* -- a syntax error is
//! always represented as an entry in `Parse::errors` plus a best-effort
//! tree (holes left by `Parser::expect` failures, bad spans wrapped in
//! `SyntaxKind::ErrorNode` by recovery), never an abort.

use apex_syntax::{GreenNode, SyntaxNode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Byte offset in the source the error was recorded at.
    pub offset: u32,
}

/// The result of any `apex-parser` entry point: a lossless green tree
/// (round-trips to the exact input even in the presence of errors) plus
/// whatever diagnostics were collected along the way.
pub struct Parse {
    pub(crate) green: GreenNode,
    pub errors: Vec<ParseError>,
}

impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl std::fmt::Debug for Parse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parse")
            .field("syntax", &self.syntax())
            .field("errors", &self.errors)
            .finish()
    }
}
