//! Lexer performance regression suite.
//!
//! Two kinds of benchmark, both tracked by Criterion across runs so a
//! regression shows up as "Performance has regressed" in `cargo bench`
//! output:
//!
//! - `corpus/tokenize_npsp`: whole-corpus throughput (NPSP's ~1070 real
//!   `.cls`/`.trigger` files), the single number that best represents
//!   "is the lexer still fast on real code."
//! - `constructs/*`: targeted micro-benchmarks, one per scanning hot path
//!   (identifiers/keywords, string literals, numeric/date literals,
//!   comments/whitespace, operators). A corpus-wide regression tells you
//!   *that* something got slower; these tell you *what*.
//!
//! Run with `cargo bench -p apex-lexer`. Criterion automatically compares
//! against the previous run's saved baseline (`target/criterion/`) and
//! flags statistically significant regressions/improvements. To compare
//! two specific points (e.g. this branch vs `main`) explicitly:
//!
//!   git checkout main   && cargo bench -p apex-lexer -- --save-baseline main
//!   git checkout mybranch && cargo bench -p apex-lexer -- --baseline main

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use rayon::prelude::*;
use std::hint::black_box;

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn load_corpus() -> Vec<String> {
    let files = apex_discover::find_apex_files(corpus_root());
    assert!(
        !files.is_empty(),
        "no .cls/.trigger files found; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)"
    );
    files
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

fn bench_corpus(c: &mut Criterion) {
    let sources = load_corpus();
    let total_bytes: u64 = sources.iter().map(|s| s.len() as u64).sum();

    let mut group = c.benchmark_group("corpus");
    group.throughput(Throughput::Bytes(total_bytes));
    group.bench_function("tokenize_npsp", |b| {
        b.iter(|| {
            for src in &sources {
                for token in apex_lexer::tokenize(black_box(src)) {
                    black_box(token);
                }
            }
        });
    });
    // Every file's tokenization is 100% independent of every other's, so
    // this is the "how much is left on the table by not using every core"
    // number -- contrast directly against tokenize_npsp above.
    group.bench_function("tokenize_npsp_parallel", |b| {
        b.iter(|| {
            sources.par_iter().for_each(|src| {
                for token in apex_lexer::tokenize(black_box(src)) {
                    black_box(token);
                }
            });
        });
    });
    group.finish();
}

fn bench_construct(c: &mut Criterion, group_name: &str, name: &str, snippet: &str) {
    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Bytes(snippet.len() as u64));
    group.bench_function(name, |b| {
        b.iter(|| {
            for token in apex_lexer::tokenize(black_box(snippet)) {
                black_box(token);
            }
        });
    });
    group.finish();
}

fn bench_constructs(c: &mut Criterion) {
    // Repeated so each snippet is a large-enough sample for a stable
    // per-byte throughput number, not dominated by fixed per-call overhead.
    let identifiers_and_keywords = r#"
        public with sharing class Account_TDTM extends TDTM_Runnable {
            private static final String STATUS = 'Active';
            global override DmlWrapper run(List<SObject> newList, List<SObject> oldList,
                TDTM_Runnable.Action triggerAction, Schema.DescribeSObjectResult objResult) {
                DmlWrapper dmlWrapper = new DmlWrapper();
                for (Account acc : (List<Account>) newList) {
                    if (acc.npe01__SYSTEMIsIndividual__c && acc.Type == STATUS) {
                        dmlWrapper.objectsToUpdate.add(acc);
                    }
                }
                return dmlWrapper;
            }
        }
    "#
    .repeat(20);

    let string_literals = r#"
        String a = 'hello world, this is a fairly typical string literal';
        String b = 'escapes: \'quote\' \n\t\\ end';
        String c = '''
            a multi-line
            triple-quoted string
            with several lines of content
        ''';
        String d = 'another short one';
    "#
    .repeat(20);

    let numeric_and_date_literals = r#"
        Integer a = 42;
        Long b = 9999999999L;
        Decimal c = 3.14159d;
        Decimal d = .5;
        Date e = 2024-01-15;
        Datetime f = 2024-01-15T12:30:00.500Z;
        Time g = 12:30:00Z;
        Integer h = 1000000;
    "#
    .repeat(20);

    let comments_and_whitespace = r#"
        /**
         * A fairly typical doc comment block explaining what a method
         * does, its parameters, and its return value.
         * @param acc the account to process
         * @return whether processing succeeded
         */
        public Boolean process(Account acc) {
            // a line comment
            /* an inline block comment */ return true;
        }

    "#
    .repeat(20);

    let operators = r#"
        Boolean r = (a == b && c != d) || (e === f && g !== h);
        Integer x = a + b - c * d / e % f;
        x += 1; x -= 1; x *= 2; x /= 2;
        Boolean y = a?.b?.c ?? defaultValue;
        List<List<Integer>> nested = new List<List<Integer>>();
        Map<String, Object> m = new Map<String, Object>{'k' => v};
    "#
    .repeat(20);

    bench_construct(
        c,
        "constructs",
        "identifiers_and_keywords",
        &identifiers_and_keywords,
    );
    bench_construct(c, "constructs", "string_literals", &string_literals);
    bench_construct(
        c,
        "constructs",
        "numeric_and_date_literals",
        &numeric_and_date_literals,
    );
    bench_construct(
        c,
        "constructs",
        "comments_and_whitespace",
        &comments_and_whitespace,
    );
    bench_construct(c, "constructs", "operators", &operators);
}

/// Single long, contiguous spans (as opposed to `constructs`' many short
/// ones) -- this is where a SIMD "find the next interesting byte" win
/// actually shows up, since the scalar byte-by-byte loop it replaces pays
/// its per-byte cost across the whole run instead of being dominated by
/// fixed per-token overhead.
fn bench_long_runs(c: &mut Criterion) {
    let long_line_comment = format!("// {}\n", "x".repeat(4000));
    let long_block_comment = format!("/* {} */\n", "x".repeat(4000));
    let long_string_literal = format!("'{}'\n", "x".repeat(4000));

    bench_construct(c, "long_runs", "long_line_comment", &long_line_comment);
    bench_construct(c, "long_runs", "long_block_comment", &long_block_comment);
    bench_construct(c, "long_runs", "long_string_literal", &long_string_literal);
}

criterion_group!(benches, bench_corpus, bench_constructs, bench_long_runs);
criterion_main!(benches);
