//! The flat event log the parser emits, and the sink that replays it (in
//! lockstep with the *raw*, trivia-inclusive token array) into a rowan
//! green tree.
//!
//! Decoupling parsing from tree construction this way is what makes
//! speculative parsing cheap: a failed attempt is just `events.truncate`,
//! since no tree ever existed to unwind (rowan's green trees are
//! immutable/append-only -- there's no "pop the last node" API).
//!
//! # Trivia attachment
//!
//! Between two significant tokens, scan the trivia run forward from the
//! first: everything up to **and including** the first newline-containing
//! `Whitespace` token becomes trailing trivia of the token before;
//! everything after that becomes leading trivia of the token after. No
//! newline in the run -> it's all trailing (keeps `stmt; // why` glued
//! together). Trivia before the first significant token is all leading
//! trivia of that token; trivia after the last one is flushed just before
//! the final (root-closing) `Finish`, so it still ends up nested inside
//! the root rather than orphaned.

use crate::input::Input;
use apex_lexer::{Token, TokenKind};
use apex_syntax::{GreenNode, GreenNodeBuilder, SyntaxKind};

pub(crate) enum Event {
    /// Placeholder until the matching `Marker` is completed, at which
    /// point `kind` is filled in. Always `Some` by the time `build` runs.
    Start {
        kind: Option<SyntaxKind>,
    },
    Finish,
    /// Consume exactly one significant token (kind/text read from the raw
    /// array at replay time, so it can never drift from what was lexed).
    Token,
}

pub(crate) fn build(src: &str, input: &Input, events: Vec<Event>) -> GreenNode {
    let raw = &input.raw;
    let mut builder = GreenNodeBuilder::new();
    let mut pos = 0usize;
    let last = events.len().saturating_sub(1);

    for (i, event) in events.into_iter().enumerate() {
        match event {
            Event::Start { kind } => {
                let kind = kind.expect("Marker completed before its tree was built");
                builder.start_node(kind.into());
            }
            Event::Token => {
                flush_leading(&mut pos, raw, src, &mut builder);
                push_raw(&mut pos, raw, src, &mut builder);
                flush_trailing(&mut pos, raw, src, &mut builder);
            }
            Event::Finish => {
                if i == last {
                    flush_all_remaining(&mut pos, raw, src, &mut builder);
                }
                builder.finish_node();
            }
        }
    }

    builder.finish()
}

fn is_newline_whitespace(tok: &Token, src: &str) -> bool {
    tok.kind == TokenKind::Whitespace && tok.text(src).contains(['\n', '\r'])
}

fn push_raw(pos: &mut usize, raw: &[Token], src: &str, builder: &mut GreenNodeBuilder) {
    let tok = &raw[*pos];
    builder.token(SyntaxKind::from_token_kind(tok.kind).into(), tok.text(src));
    *pos += 1;
}

/// Push trivia up to (not including) the next significant token.
fn flush_leading(pos: &mut usize, raw: &[Token], src: &str, builder: &mut GreenNodeBuilder) {
    while *pos < raw.len() && raw[*pos].kind.is_trivia() {
        push_raw(pos, raw, src, builder);
    }
}

/// Push trivia up to and including the first newline-containing
/// `Whitespace` token, then stop -- the "trailing half" of a trivia run.
fn flush_trailing(pos: &mut usize, raw: &[Token], src: &str, builder: &mut GreenNodeBuilder) {
    while *pos < raw.len() && raw[*pos].kind.is_trivia() {
        let is_nl = is_newline_whitespace(&raw[*pos], src);
        push_raw(pos, raw, src, builder);
        if is_nl {
            break;
        }
    }
}

fn flush_all_remaining(pos: &mut usize, raw: &[Token], src: &str, builder: &mut GreenNodeBuilder) {
    while *pos < raw.len() {
        push_raw(pos, raw, src, builder);
    }
}
