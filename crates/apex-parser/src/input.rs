//! A trivia-filtered view over the lexer's raw token stream.
//!
//! The parser (and every function in `grammar/`) only ever sees
//! significant tokens -- trivia is invisible to grammar code by
//! construction. The raw, trivia-inclusive stream is kept alongside it
//! (as `Input::raw`) for the tree-building sink (`event.rs`) to walk in
//! lockstep and attach trivia correctly.

use apex_lexer::{Token, TokenKind};

pub(crate) struct Input {
    /// Every token the lexer produced, trivia included.
    pub(crate) raw: Vec<Token>,
    /// Indices into `raw`, one per non-trivia token, in source order --
    /// what the parser's cursor actually walks.
    significant: Vec<u32>,
}

impl Input {
    pub(crate) fn new(src: &str) -> Input {
        let raw = apex_lexer::tokenize(src);
        let significant = raw
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i as u32)
            .collect();
        Input { raw, significant }
    }

    /// The kind of the `n`th significant token, or `Eof` past the end --
    /// callers never need to bounds-check lookahead themselves.
    pub(crate) fn kind(&self, n: usize) -> TokenKind {
        self.significant
            .get(n)
            .map_or(TokenKind::Eof, |&raw_idx| self.raw[raw_idx as usize].kind)
    }

    /// Index into `raw` of the `n`th significant token, if any.
    pub(crate) fn raw_index(&self, n: usize) -> Option<u32> {
        self.significant.get(n).copied()
    }
}
