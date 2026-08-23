//! Byte-offset <-> LSP `Position` conversion, parameterized by the
//! negotiated position encoding (see `PositionEncoding::negotiate`,
//! used by `main.rs`'s `initialize`). Not consumed by any capability
//! yet -- there isn't one -- but this is exactly the kind of
//! feature-independent infrastructure worth building ahead of its
//! first consumer, the same way `apex-binder`'s `AstPtr`/`SyntaxPtr`
//! were built before anything needed goto-definition.

use lsp_types::{Position, PositionEncodingKind};

/// Which LSP `positionEncodingKind` a `LineIndex` query assumes
/// `Position::character` is expressed in. LSP defaults to UTF-16 when
/// nothing is negotiated; UTF-8 is preferred whenever the client
/// supports it (`main.rs`'s `initialize`), since it needs zero
/// conversion against this project's own UTF-8 byte-offset-based
/// `TextSize`/`TextRange` (`rowan`/`apex-syntax`) -- UTF-16 and UTF-32
/// remain fully supported for clients that don't offer UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
    Utf32,
}

impl PositionEncoding {
    /// Picks the best encoding a client's advertised
    /// `general.positionEncodings` supports -- UTF-8 whenever offered
    /// (cheapest for us), UTF-16 otherwise (the LSP-mandated default,
    /// assumed supported even when a client lists nothing at all).
    pub fn negotiate(client_supported: Option<&[PositionEncodingKind]>) -> Self {
        let supports_utf8 =
            client_supported.is_some_and(|kinds| kinds.contains(&PositionEncodingKind::UTF8));
        if supports_utf8 {
            PositionEncoding::Utf8
        } else {
            PositionEncoding::Utf16
        }
    }
}

impl From<PositionEncoding> for PositionEncodingKind {
    fn from(encoding: PositionEncoding) -> Self {
        match encoding {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
            PositionEncoding::Utf32 => PositionEncodingKind::UTF32,
        }
    }
}

/// Precomputed byte offsets of every line start in some source text,
/// for fast byte-offset <-> `Position` conversion. Deliberately doesn't
/// own or cache the text itself -- callers already have it (in
/// `Backend::documents`), and recomputing a `LineIndex` on demand is a
/// single cheap linear scan, not worth the cache-invalidation
/// complexity of keeping one synchronized across edits before anything
/// actually needs to.
///
/// Not constructed anywhere outside its own unit tests yet -- no
/// capability consumes a position yet to need it -- hence the blanket
/// `allow` below rather than per-method ones; see the module doc
/// comment.
#[allow(dead_code)]
pub struct LineIndex {
    /// Byte offset of the start of each line; `line_starts[0] == 0`.
    line_starts: Vec<u32>,
}

#[allow(dead_code)]
impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// Converts a byte offset into `text` to an LSP `Position`, per
    /// `encoding`. `offset` is clamped to `text.len()`.
    pub fn to_position(&self, text: &str, offset: u32, encoding: PositionEncoding) -> Position {
        let offset = offset.min(text.len() as u32);
        let line = self.line_of_offset(offset);
        let line_start = self.line_starts[line];
        let line_text = &text[line_start as usize..offset as usize];
        let character = match encoding {
            PositionEncoding::Utf8 => line_text.len() as u32,
            PositionEncoding::Utf16 => line_text.encode_utf16().count() as u32,
            PositionEncoding::Utf32 => line_text.chars().count() as u32,
        };
        Position {
            line: line as u32,
            character,
        }
    }

    /// Converts an LSP `Position` back to a byte offset into `text`,
    /// per `encoding`. `None` if `position.line` is past the end of
    /// `text`; a `character` past the end of its line clamps to the
    /// line's own end (matching how most LSP clients behave when a
    /// position runs slightly stale relative to the server's copy).
    pub fn to_offset(
        &self,
        text: &str,
        position: Position,
        encoding: PositionEncoding,
    ) -> Option<u32> {
        let line = position.line as usize;
        let line_start = *self.line_starts.get(line)?;
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(text.len() as u32)
            .min(text.len() as u32);
        // Trim the line's own trailing newline out of its "end" for
        // character-counting purposes -- a position can't point past
        // it onto the next line's content.
        let line_text = &text[line_start as usize..line_end as usize];
        // Two separate statements, not one chained expression: each
        // `unwrap_or(line_text)` must fall back to the *previous*
        // binding, not the original unstripped slice -- inside a single
        // `let line_text = ...` expression, `line_text` still refers to
        // the prior binding throughout, which would silently undo the
        // `\n` strip whenever a line has no `\r` to strip afterward.
        let line_text = line_text.strip_suffix('\n').unwrap_or(line_text);
        let line_text = line_text.strip_suffix('\r').unwrap_or(line_text);

        let target = position.character as usize;
        let byte_offset_in_line = match encoding {
            // UTF-8 "character" offsets *are* byte offsets by
            // definition (LSP spec): no counting needed at all.
            PositionEncoding::Utf8 => target.min(line_text.len()),
            PositionEncoding::Utf16 => {
                let mut byte_offset = 0;
                let mut utf16_count = 0;
                for ch in line_text.chars() {
                    if utf16_count >= target {
                        break;
                    }
                    utf16_count += ch.len_utf16();
                    byte_offset += ch.len_utf8();
                }
                byte_offset
            }
            PositionEncoding::Utf32 => line_text
                .char_indices()
                .nth(target)
                .map_or(line_text.len(), |(i, _)| i),
        };
        Some(line_start + byte_offset_in_line as u32)
    }

    fn line_of_offset(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next_line) => next_line - 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_prefers_utf8_when_offered() {
        let offered = [PositionEncodingKind::UTF16, PositionEncodingKind::UTF8];
        assert_eq!(
            PositionEncoding::negotiate(Some(&offered)),
            PositionEncoding::Utf8
        );
    }

    #[test]
    fn negotiate_falls_back_to_utf16_when_utf8_not_offered() {
        let offered = [PositionEncodingKind::UTF16, PositionEncodingKind::UTF32];
        assert_eq!(
            PositionEncoding::negotiate(Some(&offered)),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn negotiate_falls_back_to_utf16_when_nothing_offered() {
        assert_eq!(PositionEncoding::negotiate(None), PositionEncoding::Utf16);
    }

    #[test]
    fn ascii_single_line_round_trips() {
        let text = "hello world";
        let index = LineIndex::new(text);
        for offset in [0u32, 1, 5, 11] {
            for encoding in [
                PositionEncoding::Utf8,
                PositionEncoding::Utf16,
                PositionEncoding::Utf32,
            ] {
                let pos = index.to_position(text, offset, encoding);
                assert_eq!(pos.line, 0);
                assert_eq!(
                    index.to_offset(text, pos, encoding),
                    Some(offset),
                    "round-trip failed for offset {offset} with {encoding:?}"
                );
            }
        }
    }

    #[test]
    fn multiline_offsets_land_on_the_right_line() {
        let text = "line0\nline1\nline2";
        let index = LineIndex::new(text);
        // Byte offset 6 is the 'l' starting "line1".
        let pos = index.to_position(text, 6, PositionEncoding::Utf8);
        assert_eq!(pos, Position::new(1, 0));
        // Byte offset 12 is the 'l' starting "line2".
        let pos = index.to_position(text, 12, PositionEncoding::Utf8);
        assert_eq!(pos, Position::new(2, 0));
        assert_eq!(
            index.to_offset(text, Position::new(2, 0), PositionEncoding::Utf8),
            Some(12)
        );
    }

    #[test]
    fn multibyte_utf8_character_counts_correctly_per_encoding() {
        // "café" -- 'é' is 2 bytes in UTF-8, 1 code unit in UTF-16/UTF-32.
        let text = "café";
        let index = LineIndex::new(text);
        let end = text.len() as u32; // 5 bytes: c-a-f-é(2 bytes)
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf8)
                .character,
            5
        );
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf16)
                .character,
            4
        );
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf32)
                .character,
            4
        );
    }

    #[test]
    fn surrogate_pair_character_counts_correctly_per_encoding() {
        // U+1F600 GRINNING FACE: 4 bytes in UTF-8, a surrogate *pair*
        // (2 code units) in UTF-16, 1 code point in UTF-32.
        let text = "a\u{1F600}b";
        let index = LineIndex::new(text);
        let end = text.len() as u32; // 1 + 4 + 1 = 6 bytes
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf8)
                .character,
            6
        );
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf16)
                .character,
            4 // 'a' + 2 surrogate units + 'b'
        );
        assert_eq!(
            index
                .to_position(text, end, PositionEncoding::Utf32)
                .character,
            3 // 'a' + 1 code point + 'b'
        );

        // And back: UTF-16 position 3 (right after the surrogate pair,
        // before 'b') must land on the byte offset right after the
        // 4-byte emoji, not split it in half.
        let pos = Position::new(0, 3);
        assert_eq!(index.to_offset(text, pos, PositionEncoding::Utf16), Some(5));
    }

    #[test]
    fn position_past_line_end_clamps_instead_of_panicking() {
        let text = "short\nlines";
        let index = LineIndex::new(text);
        let pos = Position::new(0, 1000);
        let offset = index.to_offset(text, pos, PositionEncoding::Utf8);
        assert_eq!(offset, Some(5)); // end of "short", before the '\n'
    }

    #[test]
    fn line_past_end_of_document_returns_none() {
        let text = "one line";
        let index = LineIndex::new(text);
        assert_eq!(
            index.to_offset(text, Position::new(5, 0), PositionEncoding::Utf8),
            None
        );
    }
}
