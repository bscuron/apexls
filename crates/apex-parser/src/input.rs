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
        Input::build(apex_lexer::tokenize(src))
    }

    /// Like [`Input::new`], but for a *pattern*: each hole in the source
    /// is folded into a single [`TokenKind::PatternHole`] before the
    /// parser ever sees it.
    ///
    /// `...` lexes as three separate `Dot`s, and `$NAME` as one ordinary
    /// `Identifier` (since `$` is a legal Apex identifier start). Folding
    /// them here rather than in the lexer means ordinary Apex lexing is
    /// untouched -- a hole token simply cannot arise from real source.
    /// Only *adjacent* dots fold, so `a . . . b` with spaces stays three
    /// dots.
    pub(crate) fn new_pattern(src: &str) -> Input {
        let raw = apex_lexer::tokenize(src);
        let mut folded: Vec<Token> = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            let t = raw[i];
            let dots = i + 2 < raw.len()
                && [0, 1, 2].iter().all(|n| raw[i + n].kind == TokenKind::Dot)
                && raw[i + 1].start == raw[i].start + 1
                && raw[i + 2].start == raw[i + 1].start + 1;
            if dots {
                folded.push(Token {
                    kind: TokenKind::PatternHole,
                    start: t.start,
                    len: 3,
                });
                i += 3;
                continue;
            }
            // `$...NAME` first: `$` alone lexes as an identifier (a dot is
            // not an identifier-continue character), so the plainer
            // `$NAME` rule below would otherwise claim the `$` and leave
            // the dots behind.
            let seq = t.kind == TokenKind::Identifier
                && t.len == 1
                && src[t.start as usize..].starts_with('$')
                && i + 4 < raw.len()
                && [1, 2, 3].iter().all(|n| raw[i + n].kind == TokenKind::Dot)
                && raw[i + 4].kind == TokenKind::Identifier
                && (1..=4).all(|n| raw[i + n].start == t.start + n as u32);
            if seq {
                folded.push(Token {
                    kind: TokenKind::PatternSeqCapture,
                    start: t.start,
                    len: 4 + raw[i + 4].len,
                });
                i += 5;
                continue;
            }
            // `$` is a legal Apex identifier start character, so `$NAME`
            // lexes as one ordinary `Identifier` -- there is no `$` token to
            // look for. A pattern therefore reserves leading-`$`
            // identifiers for captures, which real Apex code effectively
            // never uses.
            if t.kind == TokenKind::Identifier && src[t.start as usize..].starts_with('$') {
                folded.push(Token {
                    kind: TokenKind::PatternCapture,
                    start: t.start,
                    len: t.len,
                });
                i += 1;
                continue;
            }
            folded.push(t);
            i += 1;
        }
        Input::build(folded)
    }

    /// Where every hole sits in `src`, as `(start, len, is_capture)`.
    ///
    /// Shares [`Input::new_pattern`]'s folding, so a caller scanning a
    /// *replacement* template sees holes exactly where the pattern parser
    /// would -- string literals opaque, adjacent dots folded, `$NAME`
    /// recognised as one token.
    pub(crate) fn hole_spans(src: &str) -> Vec<(u32, u32, TokenKind)> {
        Input::new_pattern(src)
            .raw
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    TokenKind::PatternHole
                        | TokenKind::PatternCapture
                        | TokenKind::PatternSeqCapture
                )
            })
            .map(|t| (t.start, t.len, t.kind))
            .collect()
    }

    fn build(raw: Vec<Token>) -> Input {
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
