//! Parser performance regression suite, mirroring `apex-lexer`'s
//! `benches/lexer_bench.rs` and `apex-discover`'s `benches/discover_bench.rs`:
//! `corpus/*` for whole-corpus realistic throughput, `constructs/*` for
//! targeted per-grammar-area regressions. Run with `cargo bench -p
//! apex-parser`; see `lexer_bench.rs` for the `--save-baseline`/
//! `--baseline` before/after comparison workflow.

use apex_lexer::{Token, TokenKind};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use rayon::prelude::*;
use std::hint::black_box;

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

// ---- fragment extraction, duplicated from tests/roundtrip.rs rather
// than shared across the test/bench boundary (Cargo test/bench binaries
// are separate compilation units; the ~30 lines aren't worth a shared
// crate for a Phase 2 tool) ----

fn significant_tokens(src: &str) -> Vec<Token> {
    apex_lexer::tokenize(src)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect()
}

fn block_region_starts(tokens: &[Token]) -> Vec<usize> {
    (1..tokens.len())
        .filter(|&i| tokens[i].kind == TokenKind::LBrace && tokens[i - 1].kind == TokenKind::RParen)
        .collect()
}

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
                    return None;
                }
                brace -= 1;
                if brace == 0 && paren == 0 && brack == 0 {
                    return Some(i + 1);
                }
            }
            TokenKind::Semi if paren == 0 && brack == 0 && brace == 0 => return Some(i + 1),
            _ => {}
        }
    }
    None
}

fn extract_fragments(src: &str, tokens: &[Token]) -> Vec<String> {
    let mut fragments = Vec::new();
    for region_start in block_region_starts(tokens) {
        let mut pos = region_start + 1;
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

fn load_corpus_fragments() -> Vec<String> {
    let files = apex_discover::find_apex_files(corpus_root());
    assert!(
        !files.is_empty(),
        "no .cls/.trigger files found; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)"
    );
    files
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .flat_map(|src| {
            let tokens = significant_tokens(&src);
            extract_fragments(&src, &tokens)
        })
        .collect()
}

fn bench_corpus(c: &mut Criterion) {
    let fragments = load_corpus_fragments();
    let total_bytes: u64 = fragments.iter().map(|f| f.len() as u64).sum();

    let mut group = c.benchmark_group("corpus");
    group.throughput(Throughput::Bytes(total_bytes));
    // ~92k fragments per iteration adds up fast; trade sample count for a
    // suite that's still fast to run regularly (same tradeoff as
    // apex-discover's naive-walk baseline benchmark).
    group.sample_size(20);
    group.bench_function("parse_npsp_fragments", |b| {
        b.iter(|| {
            for fragment in &fragments {
                black_box(apex_parser::parse_statement(black_box(fragment)));
            }
        });
    });
    // Every fragment parses independently, so this is the "how much is
    // left on the table by not using every core" number.
    group.bench_function("parse_npsp_fragments_parallel", |b| {
        b.iter(|| {
            fragments.par_iter().for_each(|fragment| {
                black_box(apex_parser::parse_statement(black_box(fragment)));
            });
        });
    });
    group.finish();
}

fn bench_construct(c: &mut Criterion, name: &str, snippet: &str) {
    let mut group = c.benchmark_group("constructs");
    group.throughput(Throughput::Bytes(snippet.len() as u64));
    group.bench_function(name, |b| {
        b.iter(|| black_box(apex_parser::parse_statement(black_box(snippet))));
    });
    group.finish();
}

fn bench_constructs(c: &mut Criterion) {
    bench_construct(
        c,
        "binary_expr_precedence_chain",
        "x = a + b * c - d / e % f << g >> h & i | j ^ k && l || m ?? n;",
    );
    bench_construct(c, "postfix_call_chain", "a.b.c.d().e[0].f(g, h).i;");
    bench_construct(
        c,
        "control_flow",
        "if (a) { for (Integer i = 0; i < n; i++) { while (b) { c(); } } } else { d(); }",
    );
    // The one place Phase 2 pays a real speculative-reparse cost: local-
    // var-decl-vs-expr-stmt backtracking, isolated so a regression in the
    // checkpoint/rollback machinery specifically shows up here rather
    // than being buried in the corpus-wide average.
    bench_construct(
        c,
        "local_var_decl_backtrack_hit",
        "List<Map<String, Integer>> x = y;",
    );
    bench_construct(c, "local_var_decl_backtrack_miss", "foo.bar.baz.qux();");
}

criterion_group!(benches, bench_corpus, bench_constructs);
criterion_main!(benches);
