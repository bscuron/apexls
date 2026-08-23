//! Separators, operators, and trivia (whitespace/comments) scanning.
//!
//! Notably absent from the operator set, by grammar design (and mirrored
//! here): plain `<<` and `>>` shift tokens. `BaseApexLexer.g4` only
//! defines the compound-assignment forms (`<<=`, `>>=`, `>>>=`); a bare
//! `<<` or `>>` in source is always lexed as two/three separate `<`/`>`
//! tokens. This is the classic Java-grammar trick that lets the *parser*
//! disambiguate `List<List<Integer>>` (nested generics closing) from a
//! real shift expression, rather than the lexer guessing.

use crate::cursor::Cursor;
use crate::literal;
use crate::token::TokenKind;

/// Scan one separator or operator token. Must be called with the cursor
/// positioned at the start of a byte that isn't whitespace, a comment
/// opener, a quote, a digit, or an identifier-start byte (the main
/// dispatcher tries those first). `.` reaching here means the caller
/// already ruled out `NumberLiteral` (a `.` followed by a digit).
pub(crate) fn scan(cur: &mut Cursor) -> TokenKind {
    let b0 = cur.first();
    match b0 {
        b'(' => single(cur, TokenKind::LParen),
        b')' => single(cur, TokenKind::RParen),
        b'{' => single(cur, TokenKind::LBrace),
        b'}' => single(cur, TokenKind::RBrace),
        b'[' => literal::try_find_literal(cur).unwrap_or_else(|| single(cur, TokenKind::LBrack)),
        b']' => single(cur, TokenKind::RBrack),
        b';' => single(cur, TokenKind::Semi),
        b',' => single(cur, TokenKind::Comma),
        b'.' => single(cur, TokenKind::Dot),
        b'@' => single(cur, TokenKind::AtSign),
        b'~' => single(cur, TokenKind::Tilde),
        b':' => single(cur, TokenKind::Colon),

        b'=' => {
            if cur.starts_with("===") {
                take(cur, 3, TokenKind::TripleEqual)
            } else if cur.starts_with("==") {
                take(cur, 2, TokenKind::Equal)
            } else if cur.starts_with("=>") {
                take(cur, 2, TokenKind::MapTo)
            } else {
                single(cur, TokenKind::Assign)
            }
        }
        b'!' => {
            if cur.starts_with("!==") {
                take(cur, 3, TokenKind::TripleNotEqual)
            } else if cur.starts_with("!=") {
                take(cur, 2, TokenKind::NotEqual)
            } else {
                single(cur, TokenKind::Bang)
            }
        }
        b'>' => {
            if cur.starts_with(">>>=") {
                take(cur, 4, TokenKind::URShiftAssign)
            } else if cur.starts_with(">>=") {
                take(cur, 3, TokenKind::RShiftAssign)
            } else {
                single(cur, TokenKind::Gt)
            }
        }
        b'<' => {
            if cur.starts_with("<<=") {
                take(cur, 3, TokenKind::LShiftAssign)
            } else if cur.starts_with("<>") {
                take(cur, 2, TokenKind::LessAndGreater)
            } else {
                single(cur, TokenKind::Lt)
            }
        }
        b'?' => {
            if cur.starts_with("?.") {
                take(cur, 2, TokenKind::QuestionDot)
            } else if cur.starts_with("??") {
                take(cur, 2, TokenKind::Coal)
            } else {
                single(cur, TokenKind::Question)
            }
        }
        b'&' => {
            if cur.starts_with("&&") {
                take(cur, 2, TokenKind::And)
            } else if cur.starts_with("&=") {
                take(cur, 2, TokenKind::AndAssign)
            } else {
                single(cur, TokenKind::BitAnd)
            }
        }
        b'|' => {
            if cur.starts_with("||") {
                take(cur, 2, TokenKind::Or)
            } else if cur.starts_with("|=") {
                take(cur, 2, TokenKind::OrAssign)
            } else {
                single(cur, TokenKind::BitOr)
            }
        }
        b'^' => {
            if cur.starts_with("^=") {
                take(cur, 2, TokenKind::XorAssign)
            } else {
                single(cur, TokenKind::Caret)
            }
        }
        b'+' => {
            if cur.starts_with("++") {
                take(cur, 2, TokenKind::Inc)
            } else if cur.starts_with("+=") {
                take(cur, 2, TokenKind::AddAssign)
            } else {
                single(cur, TokenKind::Add)
            }
        }
        b'-' => {
            if cur.starts_with("--") {
                take(cur, 2, TokenKind::Dec)
            } else if cur.starts_with("-=") {
                take(cur, 2, TokenKind::SubAssign)
            } else {
                single(cur, TokenKind::Sub)
            }
        }
        b'*' => {
            if cur.starts_with("*=") {
                take(cur, 2, TokenKind::MulAssign)
            } else {
                single(cur, TokenKind::Mul)
            }
        }
        b'/' => {
            if cur.starts_with("/=") {
                take(cur, 2, TokenKind::DivAssign)
            } else {
                single(cur, TokenKind::Div)
            }
        }

        _ => single(cur, TokenKind::Unknown),
    }
}

#[inline]
fn single(cur: &mut Cursor, kind: TokenKind) -> TokenKind {
    cur.bump_byte();
    kind
}

#[inline]
fn take(cur: &mut Cursor, n: usize, kind: TokenKind) -> TokenKind {
    for _ in 0..n {
        cur.bump_byte();
    }
    kind
}

/// `WS: [ \t\r\n]+ -> channel(WHITESPACE_CHANNEL);`
/// Must be called with the cursor at a whitespace byte.
pub(crate) fn scan_whitespace(cur: &mut Cursor) -> TokenKind {
    cur.eat_while_ascii(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C));
    TokenKind::Whitespace
}

/// `LINE_COMMENT: '//' ~[\r\n]* -> channel(COMMENT_CHANNEL);`
/// Must be called with the cursor at `//`.
pub(crate) fn scan_line_comment(cur: &mut Cursor) -> TokenKind {
    cur.bump_byte();
    cur.bump_byte();
    cur.eat_while_raw(|b| b != b'\r' && b != b'\n');
    TokenKind::LineComment
}

/// `DOC_COMMENT: '/**' .*? '*/'` vs `COMMENT: '/*' .*? '*/'`.
/// Must be called with the cursor at `/*`. See module docs on the
/// `/**/`-is-not-a-doc-comment quirk this reproduces deliberately: an
/// empty `/**/` only has two stars available for `/**`'s own three-star
/// prefix plus a closing `*/`, so it can't close as a doc comment and
/// falls back to a plain (non-doc) comment, matching ANTLR longest-match.
pub(crate) fn scan_slash_star_comment(cur: &mut Cursor) -> TokenKind {
    let is_doc_prefix = cur.starts_with("/**");
    let close = find_star_slash(cur, 2);

    let (kind, end) = match close {
        Some(p) if is_doc_prefix && p >= 3 => (TokenKind::DocComment, p + 2),
        Some(p) => (TokenKind::BlockComment, p + 2),
        None => (TokenKind::BlockComment, cur.remaining()), // unterminated: consume to EOF
    };

    for _ in 0..end {
        cur.bump_byte();
    }
    kind
}

/// First offset `i >= start` (relative to the cursor) where bytes `i` and
/// `i+1` are `*` and `/`, or `None` if no such pair exists before EOF.
fn find_star_slash(cur: &Cursor, start: usize) -> Option<usize> {
    let rem = cur.remaining();
    let mut i = start;
    while i + 1 < rem {
        if cur.peek_at(i) == b'*' && cur.peek_at(i + 1) == b'/' {
            return Some(i);
        }
        i += 1;
    }
    None
}
