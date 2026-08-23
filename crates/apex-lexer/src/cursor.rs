//! A byte-oriented scanning cursor over source text.
//!
//! Apex source is overwhelmingly ASCII (keywords, punctuation, digits are
//! all ASCII; only `Identifier` continuation bytes can be non-ASCII per
//! `JavaLetter`/`JavaLetterOrDigit` in the grammar). So the hot scanning
//! path indexes raw bytes directly — no per-character UTF-8 decode, no
//! bounds-checked `Option` unwrapping in the common case — and only falls
//! back to decoding a full `char` when a byte `>= 0x80` is actually seen.

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    src: &'a str,
    pos: u32,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub(crate) fn new(src: &'a str) -> Self {
        Cursor {
            bytes: src.as_bytes(),
            src,
            pos: 0,
        }
    }

    #[inline]
    pub(crate) fn pos(&self) -> u32 {
        self.pos
    }

    #[inline]
    pub(crate) fn is_eof(&self) -> bool {
        self.pos as usize >= self.bytes.len()
    }

    /// Byte `offset` positions ahead of the cursor, or `0` (NUL) past EOF.
    ///
    /// A real embedded NUL byte in the source would read the same as EOF
    /// here, but every loop in this lexer gates on [`Self::is_eof`]
    /// separately rather than trusting a `0` byte to mean "end of input",
    /// so an embedded NUL is scanned like any other byte, not misread as
    /// end-of-file.
    #[inline]
    fn byte_at(&self, offset: usize) -> u8 {
        *self.bytes.get(self.pos as usize + offset).unwrap_or(&0)
    }

    #[inline]
    pub(crate) fn first(&self) -> u8 {
        self.byte_at(0)
    }

    #[inline]
    pub(crate) fn second(&self) -> u8 {
        self.byte_at(1)
    }

    /// Byte `offset` positions ahead of the cursor, `0` (NUL) past EOF.
    /// Public lookahead counterpart to `first`/`second`, for
    /// scanning fixed-shape literals (dates/times) that need to peek
    /// further ahead before committing to consuming anything.
    #[inline]
    pub(crate) fn peek_at(&self, offset: usize) -> u8 {
        self.byte_at(offset)
    }

    /// Does the input starting at the cursor equal `s` byte-for-byte?
    #[inline]
    pub(crate) fn starts_with(&self, s: &str) -> bool {
        self.bytes[self.pos as usize..].starts_with(s.as_bytes())
    }

    /// Consume and return one byte. Never advances past EOF.
    #[inline]
    pub(crate) fn bump_byte(&mut self) -> u8 {
        let b = self.first();
        if !self.is_eof() {
            self.pos += 1;
        }
        b
    }

    /// Decode and consume one full `char` (the non-ASCII fallback path).
    /// The source is a `&str`, so this is always a valid UTF-8 decode.
    #[inline]
    pub(crate) fn bump_char(&mut self) -> Option<char> {
        if self.is_eof() {
            return None;
        }
        let ch = self.src[self.pos as usize..].chars().next()?;
        self.pos += ch.len_utf8() as u32;
        Some(ch)
    }

    /// Advance while `pred` holds on the current byte, ASCII fast path.
    /// Stops (without consuming) at EOF or the first non-ASCII byte, so
    /// callers needing non-ASCII continuation (identifiers) must switch to
    /// [`Self::bump_char`] themselves once this returns.
    #[inline]
    pub(crate) fn eat_while_ascii(&mut self, mut pred: impl FnMut(u8) -> bool) {
        while !self.is_eof() {
            let b = self.first();
            if b >= 0x80 || !pred(b) {
                break;
            }
            self.pos += 1;
        }
    }

    /// Bytes remaining in the source from the cursor's current position.
    #[inline]
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.pos as usize
    }

    /// Peek the `char` at the cursor without consuming it.
    #[inline]
    pub(crate) fn peek_char(&self) -> Option<char> {
        if self.is_eof() {
            return None;
        }
        self.src[self.pos as usize..].chars().next()
    }

    /// Raw byte slice `[start, end)`, for classifying an already-scanned
    /// span (keyword lookup, currency-literal shape check).
    #[inline]
    pub(crate) fn slice(&self, start: u32, end: u32) -> &'a [u8] {
        &self.bytes[start as usize..end as usize]
    }

    /// The unconsumed remainder of the source, as bytes. The natural
    /// haystack for `memchr`-based scanning (comments, string literals):
    /// callers search this for the next delimiter byte(s), then
    /// [`Self::advance`] by the returned index rather than looping
    /// byte-by-byte themselves.
    #[inline]
    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos as usize..]
    }

    /// Advance the cursor by `n` bytes without inspecting them. Callers
    /// are responsible for `n` landing on a valid boundary -- safe
    /// whenever `n` came from a `memchr` search over [`Self::rest`],
    /// since every needle byte searched for in this lexer is ASCII and an
    /// ASCII byte value can never occur as a UTF-8 continuation byte.
    #[inline]
    pub(crate) fn advance(&mut self, n: usize) {
        self.pos += n as u32;
    }

    /// Advance the cursor to EOF (an unterminated comment/string: nothing
    /// left to do but consume the rest of the source).
    #[inline]
    pub(crate) fn advance_to_end(&mut self) {
        self.pos = self.bytes.len() as u32;
    }
}
