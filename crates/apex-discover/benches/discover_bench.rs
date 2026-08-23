//! Directory-walk performance regression suite.
//!
//! Tracks two things across runs: the pruned parallel walk we actually
//! ship, and a naive unpruned single-threaded walk kept purely as a fixed
//! reference point (it isn't shipped code, just a stable "no cleverness"
//! baseline so a regression in the pruned walk's *relative* speedup is
//! visible even if absolute numbers drift with machine/disk noise).
//!
//! Run with `cargo bench -p apex-discover`. See apex-lexer's
//! `benches/lexer_bench.rs` for the baseline-comparison workflow.

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn naive_find(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            naive_find(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger")
            })
        {
            out.push(path);
        }
    }
}

fn bench_walk(c: &mut Criterion) {
    let root = corpus_root();
    assert!(
        !apex_discover::find_apex_files(&root).is_empty(),
        "no .cls/.trigger files found under {}; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)",
        root.display()
    );

    let mut group = c.benchmark_group("walk");
    // The naive walk is ~40x slower than the pruned one (see module docs);
    // at the default 100 samples that's tens of seconds just for the
    // fixed-reference baseline, so trade sample count for a suite that's
    // still fast to run regularly.
    group.sample_size(20);
    group.bench_function("pruned_parallel", |b| {
        b.iter(|| black_box(apex_discover::find_apex_files(black_box(&root))));
    });
    group.bench_function("naive_single_thread", |b| {
        b.iter(|| {
            let mut out = Vec::new();
            naive_find(black_box(&root), &mut out);
            black_box(out)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_walk);
criterion_main!(benches);
