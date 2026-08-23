//! Number, string, date/time, and currency literal scanning.
//!
//! Grammar reference (`BaseApexLexer.g4`):
//!
//! ```text
//! DateLiteral: Digit Digit Digit Digit '-' Digit Digit '-' Digit Digit;
//! TimeLiteral: Digit Digit ':' Digit Digit ':' Digit Digit
//!              ('.' Digit+)? ('z' | (('+'|'-') Digit+ (':' Digit+)?));
//! DateTimeLiteral: DateLiteral 't' TimeLiteral;
//! IntegralCurrencyLiteral: [a-z][a-z][a-z] Digit+;
//! IntegerLiteral: Digit Digit*;
//! LongLiteral: Digit Digit* [l];
//! NumberLiteral: Digit* '.' Digit Digit* [d]?;
//! StringLiteral: '\'' StringCharacters? '\'';
//! MultilineStringLiteral: '\'\'\'' [\r\n] (EscapeSequence|'\''|~['\\])*? '\'\'\'';
//! ```
//!
//! `Date`/`Time`/`DateTime`/`IntegralCurrency` literals are lexically
//! ambiguous with plain numbers/identifiers; ANTLR resolves this by
//! longest-match (then declaration order on ties), and the functions here
//! replicate that by construction: each `try_*` peeks the *entire* fixed
//! shape before committing, so a partial match (e.g. `2024-01` without the
//! trailing `-DD`) correctly falls through to plain numeric/identifier
//! scanning instead of being partially consumed.

use crate::cursor::Cursor;
use crate::token::TokenKind;

#[inline]
fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// Peek whether `len` bytes starting `offset` ahead of the cursor are all
/// ASCII digits, without consuming anything.
fn digits_at(cur: &Cursor, offset: usize, len: usize) -> bool {
    (0..len).all(|i| is_digit(cur.peek_at(offset + i)))
}

/// Try to scan a `DateLiteral` (`DDDD-DD-DD`) or, if a case-insensitive
/// `t` and a valid `TimeLiteral` immediately follow, a `DateTimeLiteral`.
/// Must be called with the cursor positioned at the first of the four
/// leading digits. Consumes on success; leaves the cursor untouched on
/// `None` so the caller can fall back to plain numeric scanning.
pub(crate) fn try_date_or_datetime(cur: &mut Cursor) -> Option<TokenKind> {
    if !(digits_at(cur, 0, 4)
        && cur.peek_at(4) == b'-'
        && digits_at(cur, 5, 2)
        && cur.peek_at(7) == b'-'
        && digits_at(cur, 8, 2))
    {
        return None;
    }
    // 10-byte DateLiteral confirmed.
    let is_t = matches!(cur.peek_at(10), b't' | b'T');
    if is_t {
        if let Some(time_len) = time_literal_len_at(cur, 11) {
            for _ in 0..11 + time_len {
                cur.bump_byte();
            }
            return Some(TokenKind::DateTimeLiteral);
        }
    }
    for _ in 0..10 {
        cur.bump_byte();
    }
    Some(TokenKind::DateLiteral)
}

/// Try to scan a standalone `TimeLiteral` (`DD:DD:DD` plus optional
/// fraction/offset). Must be called with the cursor at the first digit.
pub(crate) fn try_time(cur: &mut Cursor) -> Option<TokenKind> {
    let len = time_literal_len_at(cur, 0)?;
    for _ in 0..len {
        cur.bump_byte();
    }
    Some(TokenKind::TimeLiteral)
}

/// Length of a `TimeLiteral` starting `offset` bytes ahead of the cursor,
/// or `None` if the mandatory `DD:DD:DD` core isn't present there. Pure
/// lookahead: never mutates the cursor.
fn time_literal_len_at(cur: &Cursor, offset: usize) -> Option<usize> {
    if !(digits_at(cur, offset, 2)
        && cur.peek_at(offset + 2) == b':'
        && digits_at(cur, offset + 3, 2)
        && cur.peek_at(offset + 5) == b':'
        && digits_at(cur, offset + 6, 2))
    {
        return None;
    }
    let mut len = offset + 8;

    // Optional fractional seconds: '.' Digit+
    if cur.peek_at(len) == b'.' && is_digit(cur.peek_at(len + 1)) {
        len += 1;
        while is_digit(cur.peek_at(len)) {
            len += 1;
        }
    }

    // Mandatory-in-grammar zone offset: 'z' | ('+'|'-') Digit+ (':' Digit+)?
    match cur.peek_at(len) {
        b'z' | b'Z' => len += 1,
        b'+' | b'-' => {
            len += 1;
            let digits_start = len;
            while is_digit(cur.peek_at(len)) {
                len += 1;
            }
            if len == digits_start {
                // No digits after the sign: the offset clause didn't
                // actually match, so neither did this TimeLiteral overall.
                return None;
            }
            if cur.peek_at(len) == b':' && is_digit(cur.peek_at(len + 1)) {
                len += 1;
                while is_digit(cur.peek_at(len)) {
                    len += 1;
                }
            }
        }
        _ => return None,
    }

    Some(len - offset)
}

/// Scan plain `IntegerLiteral` / `LongLiteral` / `NumberLiteral`. Must be
/// called with the cursor at the first digit, *after* `try_date_or_datetime`
/// and `try_time` have both already returned `None` for this position.
pub(crate) fn scan_number(cur: &mut Cursor) -> TokenKind {
    cur.eat_while_ascii(is_digit);

    if cur.first() == b'.' && is_digit(cur.second()) {
        cur.bump_byte(); // '.'
        cur.eat_while_ascii(is_digit);
        if matches!(cur.first(), b'd' | b'D') {
            cur.bump_byte();
        }
        return TokenKind::NumberLiteral;
    }

    if matches!(cur.first(), b'l' | b'L') {
        cur.bump_byte();
        return TokenKind::LongLiteral;
    }

    TokenKind::IntegerLiteral
}

/// Scan a `NumberLiteral` that starts with `.` (`Digit*` may be empty),
/// e.g. `.5`. Must be called with the cursor positioned *at* the `.`, and
/// only once the caller has confirmed a digit follows it — otherwise a
/// bare `.` is just the `DOT` token.
pub(crate) fn scan_number_from_dot(cur: &mut Cursor) -> TokenKind {
    cur.bump_byte(); // '.'
    cur.eat_while_ascii(is_digit);
    if matches!(cur.first(), b'd' | b'D') {
        cur.bump_byte();
    }
    TokenKind::NumberLiteral
}

/// Does `word` have the exact `[a-zA-Z]{3}[0-9]+` shape of an
/// `IntegralCurrencyLiteral`? Called by the identifier/keyword scanner on
/// the full maximal-munch identifier span it already collected — an
/// `IntegralCurrencyLiteral` is only ever the *whole* such span (ANTLR's
/// longest-match ties this rule against `Identifier` at equal length, and
/// this rule is declared first, so it wins whenever the shape fits).
pub(crate) fn is_currency_literal_shape(word: &[u8]) -> bool {
    word.len() > 3
        && word[..3].iter().all(u8::is_ascii_alphabetic)
        && word[3..].iter().all(u8::is_ascii_digit)
}

/// Scan a `'...'` `StringLiteral` or a `'''...'''` `MultilineStringLiteral`.
/// Must be called with the cursor positioned at the opening `'`.
///
/// Known simplification (tracked for the Phase-5 error-recovery pass):
/// the grammar's `EscapeSequence` fragment only recognizes a specific set
/// of escapes (`\b\t\n\f\r\"\'\\` and `\uXXXX`); an invalid escape like
/// `\q` makes the reference lexer fail to extend the token at all. Here we
/// treat `\` followed by *any* character as consumed, which is lenient
/// (never breaks a literal early on a malformed escape) rather than
/// strictly conformant. This must be revisited once the ANTLR-oracle
/// differential harness (`oracle/`) is running, since it's exactly the
/// kind of divergence that harness exists to catch.
pub(crate) fn scan_string(cur: &mut Cursor) -> TokenKind {
    if cur.starts_with("'''") && matches!(cur.peek_at(3), b'\r' | b'\n') {
        return scan_multiline_string(cur);
    }

    cur.bump_byte(); // opening '
    scan_quoted_body(cur, false);
    TokenKind::StringLiteral
}

/// Shared scanning loop for both string forms: SIMD-jump (via
/// `memchr::memchr2`) to the next `'` or `\`, since everything in between
/// -- including non-ASCII content, which never equals either ASCII byte
/// value -- is uninteresting and can be skipped as a single block instead
/// of decoded/inspected byte-by-byte or char-by-char.
///
/// A `'` closes a plain string immediately. For a multiline string it
/// only closes if two more `'` immediately follow (the grammar's `'''`
/// terminator); otherwise it's just a lone-quote content character, per
/// `MultilineStringLiteral`'s body alternative `'\''`.
fn scan_quoted_body(cur: &mut Cursor, multiline: bool) {
    loop {
        match memchr::memchr2(b'\'', b'\\', cur.rest()) {
            None => {
                cur.advance_to_end(); // unterminated; stop at EOF
                break;
            }
            Some(idx) => {
                cur.advance(idx);
                match cur.first() {
                    b'\\' => {
                        cur.bump_byte();
                        if !cur.is_eof() {
                            cur.bump_byte();
                        }
                    }
                    _ if !multiline => {
                        cur.bump_byte(); // closing '
                        break;
                    }
                    _ if cur.starts_with("'''") => {
                        cur.bump_byte();
                        cur.bump_byte();
                        cur.bump_byte(); // closing '''
                        break;
                    }
                    _ => {
                        cur.bump_byte(); // lone ' is just content
                    }
                }
            }
        }
    }
}

/// Length of a run of `WS` charset bytes (` \t\r\n` + form feed) starting
/// `offset` ahead of the cursor. `WS` itself is ASCII-only per the
/// grammar, so a plain byte scan (no UTF-8 decode) is correct.
fn ws_len_at(cur: &Cursor, offset: usize) -> usize {
    let rem = cur.remaining();
    let mut i = offset;
    while i < rem && matches!(cur.peek_at(i), b' ' | b'\t' | b'\r' | b'\n' | 0x0C) {
        i += 1;
    }
    i - offset
}

/// Case-insensitive ASCII match of `lit` starting `offset` ahead of the
/// cursor, without consuming.
fn matches_ci_at(cur: &Cursor, offset: usize, lit: &[u8]) -> bool {
    if offset + lit.len() > cur.remaining() {
        return false;
    }
    (0..lit.len()).all(|i| cur.peek_at(offset + i).eq_ignore_ascii_case(&lit[i]))
}

/// Try to scan a SOSL `FindLiteral` (`[find '...']`) or `FindLiteralAlt`
/// (`[find {...}]`). Must be called with the cursor at the `[`. Pure
/// lookahead until the whole shape is confirmed; consumes only on success,
/// so the caller can fall back to plain `LBRACK` on `None` (the common
/// case — most `[` in Apex source open a SOQL query, not a SOSL find).
pub(crate) fn try_find_literal(cur: &mut Cursor) -> Option<TokenKind> {
    let mut off = 1; // past '['
    off += ws_len_at(cur, off);
    if !matches_ci_at(cur, off, b"find") {
        return None;
    }
    off += 4;
    let ws = ws_len_at(cur, off);
    if ws == 0 {
        return None; // WS is mandatory between 'find' and the opening delimiter
    }
    off += ws;

    let (close, alt) = match cur.peek_at(off) {
        b'\'' => (b'\'', false),
        b'{' => (b'}', true),
        _ => return None,
    };
    off += 1;

    let rem = cur.remaining();
    loop {
        if off >= rem {
            return None; // unterminated: not a FindLiteral at all
        }
        match cur.peek_at(off) {
            b if b == close => {
                off += 1;
                break;
            }
            b'\\' if off + 1 < rem => off += 2,
            _ => off += 1,
        }
    }

    for _ in 0..off {
        cur.bump_byte();
    }
    Some(if alt {
        TokenKind::FindLiteralAlt
    } else {
        TokenKind::FindLiteral
    })
}

fn scan_multiline_string(cur: &mut Cursor) -> TokenKind {
    cur.bump_byte();
    cur.bump_byte();
    cur.bump_byte(); // opening '''
    cur.bump_byte(); // the mandatory \r or \n right after it

    scan_quoted_body(cur, true);
    TokenKind::MultilineStringLiteral
}
