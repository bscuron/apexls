//! Fragment-extraction round-trip test against the real NPSP corpus.
//!
//! Phase 2 can't parse whole `.cls` files yet (no declarations), so
//! there's no whole-file round-trip to check the way `apex-lexer`'s
//! roundtrip test does. Instead: walk method-body-shaped `{ ... }`
//! regions (a `{` immediately following a `)`, a cheap but effective
//! heuristic that also happens to catch if/for/while/try bodies), slice
//! them into individual statement fragments by brace/paren/bracket-depth
//! tracking, and round-trip each one through `parse_statement` +
//! `apex_printer::render`.
//!
//! This holds *unconditionally*, errors or not: the tree-building sink
//! flushes every remaining raw token (including anything the grammar
//! didn't recognize) before the root node closes, so a fragment hitting
//! an unsupported construct (SOQL, a declaration, ...) still round-trips
//! -- it just does so with recorded errors and a less useful tree shape,
//! not dropped bytes. A round-trip *mismatch* is therefore always a real
//! bug (a byte genuinely lost or duplicated), never an expected-scope
//! gap, so this test has no "expected failure" filtering to get wrong.
//! The clean-parse rate is still reported separately as an informational
//! coverage metric.

use apex_lexer::{Token, TokenKind};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// Significant (non-trivia) tokens only, each paired with its original
/// byte span -- fragment boundaries are computed over this view, then
/// sliced back out of the raw source text.
fn significant_tokens(src: &str) -> Vec<Token> {
    apex_lexer::tokenize(src)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect()
}

/// Method-body-shaped region starts: the index (into `tokens`) of a
/// `{` immediately preceded by a `)`. Also catches if/for/while/try
/// bodies, which only broadens coverage.
fn block_region_starts(tokens: &[Token]) -> Vec<usize> {
    (1..tokens.len())
        .filter(|&i| tokens[i].kind == TokenKind::LBrace && tokens[i - 1].kind == TokenKind::RParen)
        .collect()
}

/// From `tokens[start]` (just after a region-opening `{`), the end index
/// (exclusive) of the next statement fragment: either a top-level `;`
/// (simple statements) or the matching `}` of a brace pair opened at
/// fragment-top-level (compound statements, or the region's own closing
/// brace when nothing more follows -- signaled by `None`).
fn next_fragment_end(tokens: &[Token], start: usize) -> Option<usize> {
    let (mut paren, mut brack, mut brace) = (0i32, 0i32, 0i32);
    for (offset, tok) in tokens[start..].iter().enumerate() {
        let i = start + offset;
        match tok.kind {
            TokenKind::LParen => paren += 1,
            TokenKind::RParen => paren -= 1,
            TokenKind::LBrack => brack += 1,
            TokenKind::RBrack => brack -= 1,
            TokenKind::LBrace => brace += 1,
            TokenKind::RBrace => {
                if brace == 0 {
                    // Closes something opened *outside* this fragment
                    // (the enclosing region) -- no more fragments here.
                    return None;
                }
                brace -= 1;
                if brace == 0 && paren == 0 && brack == 0 {
                    return Some(i + 1);
                }
            }
            TokenKind::Semi if paren == 0 && brack == 0 && brace == 0 => {
                return Some(i + 1);
            }
            _ => {}
        }
    }
    None
}

fn extract_fragments(src: &str, tokens: &[Token]) -> Vec<String> {
    let mut fragments = Vec::new();
    for region_start in block_region_starts(tokens) {
        let mut pos = region_start + 1; // just past the opening '{'
        while let Some(end) = next_fragment_end(tokens, pos) {
            let start_byte = tokens[pos].start as usize;
            let end_byte = tokens[end - 1].end() as usize;
            if start_byte < end_byte {
                fragments.push(src[start_byte..end_byte].to_string());
            }
            pos = end;
        }
    }
    fragments
}

#[test]
fn statement_fragments_round_trip_exactly() {
    let root = corpus_root();
    let files = apex_discover::find_apex_files(&root);
    assert!(
        !files.is_empty(),
        "no .cls/.trigger files found under {}; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)",
        root.display()
    );

    let mut checked = 0usize;
    let mut clean = 0usize;
    let mut mismatches: Vec<(String, String)> = Vec::new();

    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let tokens = significant_tokens(&src);
        for fragment in extract_fragments(&src, &tokens) {
            checked += 1;
            let parse = apex_parser::parse_statement(&fragment);
            let rendered = apex_printer::render(&parse.syntax());
            if parse.errors.is_empty() {
                clean += 1;
            }
            if rendered != fragment && mismatches.len() < 20 {
                mismatches.push((fragment.clone(), rendered));
            }
        }
    }

    assert!(
        checked > 1000,
        "expected a substantial number of extracted fragments, got {checked}"
    );
    println!(
        "statement_fragments_round_trip_exactly: {checked} fragments, {clean} parsed with zero \
         errors ({:.1}%), {} round-trip mismatches",
        100.0 * clean as f64 / checked as f64,
        mismatches.len(),
    );

    assert!(
        mismatches.is_empty(),
        "{} fragments failed to round-trip exactly (showing up to 20):\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .map(|(src, rendered)| format!("  src:      {src:?}\n  rendered: {rendered:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
