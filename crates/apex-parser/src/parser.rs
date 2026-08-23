//! Core parser primitives: the significant-token cursor, the event-log
//! `Marker`/`CompletedMarker` API, speculative-parse checkpoints, and the
//! generic error-recovery helpers every `grammar/` module builds on.

use crate::errors::ParseError;
use crate::event::Event;
use crate::input::Input;
use apex_syntax::SyntaxKind;

pub(crate) struct Parser<'t> {
    input: &'t Input,
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

impl<'t> Parser<'t> {
    pub(crate) fn new(input: &'t Input) -> Self {
        Parser {
            input,
            pos: 0,
            events: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub(crate) fn finish(self) -> (Vec<Event>, Vec<ParseError>) {
        (self.events, self.errors)
    }

    // ---- lookahead ----

    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        SyntaxKind::from_token_kind(self.input.kind(self.pos + n))
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.at(SyntaxKind::Eof)
    }

    // ---- consuming ----

    /// Consume the current token as-is. No-op (does not panic) at EOF, so
    /// callers recovering from errors can't accidentally run past the end
    /// -- `bump` is always safe to call unconditionally.
    pub(crate) fn bump(&mut self) {
        if self.at_eof() {
            return;
        }
        self.events.push(Event::Token);
        self.pos += 1;
    }

    /// Consume the current token if it matches `kind`; otherwise record an
    /// error *without* consuming, so the caller's node still completes
    /// with a hole rather than eating a token that belongs to whatever
    /// comes next.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            self.error(format!("expected {kind:?}, found {:?}", self.current()));
            false
        }
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        let offset = self.input.raw_index(self.pos).map_or_else(
            || self.source_len(),
            |raw_idx| self.input.raw[raw_idx as usize].start,
        );
        self.errors.push(ParseError {
            message: message.into(),
            offset,
        });
    }

    fn source_len(&self) -> u32 {
        self.input.raw.last().map_or(0, |t| t.end())
    }

    // ---- tree building: markers ----

    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start { kind: None });
        Marker { pos }
    }

    // ---- speculative parsing ----

    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            pos: self.pos,
            events_len: self.events.len(),
            errors_len: self.errors.len(),
        }
    }

    /// Discard everything parsed since `checkpoint` -- cheap, since it's
    /// just truncating two `Vec`s, no tree ever having been built.
    pub(crate) fn rollback(&mut self, checkpoint: Checkpoint) {
        self.pos = checkpoint.pos;
        self.events.truncate(checkpoint.events_len);
        self.errors.truncate(checkpoint.errors_len);
    }

    // ---- recovery ----

    /// Guards a "parse the next thing" loop against infinite-looping on
    /// input that no sub-parser can make progress on: if `pos` didn't
    /// advance, force-consume one token wrapped in an `ErrorNode` so the
    /// loop is guaranteed to terminate on any input, including fuzzed
    /// byte soup.
    pub(crate) fn bump_if_no_progress(&mut self, pos_before: usize) {
        if self.pos == pos_before && !self.at_eof() {
            let m = self.start();
            self.error(format!("unexpected {:?}", self.current()));
            self.bump();
            m.complete(self, SyntaxKind::ErrorNode);
        }
    }

    /// Skip forward, wrapping skipped tokens in an `ErrorNode` (keeping
    /// the tree lossless even mid-recovery), until `at_resync` holds or
    /// EOF is reached. `RBrace` is deliberately left for the caller to
    /// decide about -- skipping it would eat the closing brace of an
    /// enclosing scope, so resync sets should stop *before* it rather
    /// than include it, unless the caller wants it consumed too.
    pub(crate) fn recover_until(&mut self, resync: impl Fn(SyntaxKind) -> bool) {
        if self.at_eof() || resync(self.current()) {
            return;
        }
        let m = self.start();
        while !self.at_eof() && !resync(self.current()) {
            self.bump();
        }
        m.complete(self, SyntaxKind::ErrorNode);
    }

    /// Current position in the significant-token cursor -- only meant for
    /// no-progress bookkeeping (see [`Self::parse_list`]); grammar code
    /// otherwise has no business inspecting raw positions directly.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Repeatedly calls `parse_one` until `at_end` holds or EOF, guarding
    /// every iteration against zero progress -- the shared "parse a
    /// statement list / when-clause list / catch-clause list" loop shape
    /// used throughout `grammar::statements`.
    pub(crate) fn parse_list(
        &mut self,
        at_end: impl Fn(&Parser<'_>) -> bool,
        mut parse_one: impl FnMut(&mut Parser<'_>),
    ) {
        while !self.at_eof() && !at_end(self) {
            let before = self.pos();
            parse_one(self);
            self.bump_if_no_progress(before);
        }
    }
}

pub(crate) struct Checkpoint {
    pos: usize,
    events_len: usize,
    errors_len: usize,
}

/// An opened, not-yet-completed tree node.
#[must_use]
pub(crate) struct Marker {
    pos: usize,
}

impl Marker {
    pub(crate) fn complete(self, p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        match &mut p.events[self.pos] {
            Event::Start { kind: k } => *k = Some(kind),
            _ => unreachable!("Marker did not point at its own Start event"),
        }
        p.events.push(Event::Finish);
        CompletedMarker {
            start_pos: self.pos,
        }
    }
}

/// An already-completed tree node, which can still be retroactively
/// wrapped in a new enclosing node via `precede` -- the mechanism that
/// makes left-recursive-shaped grammar (binary expression chains, postfix
/// `.`/`()`/`[]` chains) constructible in one forward pass.
pub(crate) struct CompletedMarker {
    start_pos: usize,
}

impl CompletedMarker {
    /// Open a new node whose first child is this already-completed one
    /// (and whose later children are whatever the caller parses next,
    /// before completing the returned `Marker`). Implemented by inserting
    /// a new `Start` placeholder right before this node's own `Start`
    /// event -- everything from there onward, up to wherever the new
    /// marker is eventually completed, becomes the new node's contents.
    pub(crate) fn precede(self, p: &mut Parser<'_>) -> Marker {
        p.events.insert(self.start_pos, Event::Start { kind: None });
        Marker {
            pos: self.start_pos,
        }
    }
}
