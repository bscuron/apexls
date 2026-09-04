//! THROWAWAY PROTOTYPE -- Wayfinder `apex-diagnostics` map, ticket 28
//! (`.scratch/apex-diagnostics/issues/28-salsa-parse-nodecache-prototype.md`).
//! Not wired into `apex_binder::BoundProgram::from_files_cached`'s real
//! call path, not production code, safe to delete once ticket 28's
//! resolution has captured its numbers.
//!
//! Question: ticket 27 chose to promote parsing itself to a per-file
//! salsa-tracked query (file text as a `#[salsa::input]`). Today's real
//! `parse_one` (`crates/apex-binder/src/lib.rs`) shares one
//! `apex_parser::NodeCache` per `rayon::fold()` segment across many
//! files -- specifically because a naive fresh-`NodeCache`-per-file
//! version regressed warm single-file-edit rebind by ~47% (measured via
//! `cargo bench -p apex-binder`). Salsa's clone-per-thread parallel model
//! (confirmed in ticket 27 against salsa 0.28's own source) has no
//! `fold()` accumulator to share a `NodeCache` through. Does a
//! `thread_local!` `NodeCache` -- one per `rayon` worker thread, reused
//! across every file that thread happens to parse over a run -- preserve
//! close to today's benefit?
//!
//! Run with:
//!   cargo run -p apex-binder --release --example salsa_nodecache_prototype
//!
//! Prints wall-clock numbers for two cases, comparing today's
//! fold-shared-`NodeCache` baseline against a salsa-tracked,
//! thread-local-`NodeCache` alternative:
//!   1. Cold: parse the whole real NPSP corpus (~1070 files) from
//!      scratch -- the case fold-segment sharing was built to help
//!      (cross-file keyword/punctuation-token interning amortization).
//!   2. Warm: one file's text changes, every other file's `Parse` is
//!      still valid -- the specific case that regressed 47% when a prior
//!      naive attempt gave every file its own fresh `NodeCache`.

use apex_parser::{Parse, parse_compilation_unit_with_cache};
use apex_syntax::NodeCache;
use rayon::prelude::*;
use salsa::Setter;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn load_corpus() -> Vec<(PathBuf, String)> {
    apex_discover::find_apex_files(corpus_root())
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path, text))
        })
        .collect()
}

// -- Baseline: today's real shape (`parse_one` in lib.rs), minus the
// stat/`Freshness` machinery this prototype isn't testing -- just the
// part in question: one `NodeCache` shared across every file in a
// `rayon::fold()` segment.
fn parse_all_fold_shared(files: &[(PathBuf, String)]) -> Duration {
    let start = Instant::now();
    files
        .par_iter()
        .with_min_len(64)
        .fold(NodeCache::default, |mut cache, (_, text)| {
            std::hint::black_box(parse_compilation_unit_with_cache(text, &mut cache));
            cache
        })
        .for_each(|_| {});
    start.elapsed()
}

// -- Candidate: per-file salsa-tracked `parse` query, backed by a
// `thread_local!` `NodeCache` instead of a `fold()`-scoped one.
#[salsa::db]
#[derive(Clone, Default)]
struct ProtoDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for ProtoDb {}

#[salsa::input]
struct FileTextInput {
    text: String,
}

thread_local! {
    static THREAD_NODE_CACHE: RefCell<NodeCache> = RefCell::new(NodeCache::default());
}

#[salsa::tracked(no_eq, returns(clone))]
fn parse_query(db: &dyn salsa::Database, input: FileTextInput) -> Parse {
    THREAD_NODE_CACHE.with(|cache| {
        parse_compilation_unit_with_cache(input.text(db).as_str(), &mut cache.borrow_mut())
    })
}

fn parse_all_salsa_threadlocal_cold(files: &[(PathBuf, String)]) -> Duration {
    let start = Instant::now();
    let mut db = ProtoDb::default();
    // Sequential input-setting phase first (ticket 27's confirmed
    // gotcha: writing a salsa input cancels every other in-flight query
    // on every other clone -- inputs and parallel reads never overlap).
    let inputs: Vec<FileTextInput> = files
        .iter()
        .map(|(_, text)| FileTextInput::new(&mut db, text.clone()))
        .collect();
    // Parallel read phase: `Storage<Db>` (hence `ProtoDb`) isn't `Sync`,
    // so a rayon closure can't hold a *shared reference* to one outer
    // `db` and clone it per call -- the clone has to happen up front,
    // owned per item, exactly matching salsa's own parallel test
    // precedent (`db_t1`/`db_t2`, cloned and moved into each thread, not
    // borrowed).
    let owned: Vec<(FileTextInput, ProtoDb)> =
        inputs.iter().map(|&input| (input, db.clone())).collect();
    owned.into_par_iter().for_each(|(input, db)| {
        std::hint::black_box(parse_query(&db, input));
    });
    start.elapsed()
}

fn main() {
    let files = load_corpus();
    assert!(
        !files.is_empty(),
        "no NPSP corpus found at {}; is the submodule checked out? (git submodule update --init --recursive)",
        corpus_root().display()
    );
    println!("corpus: {} real files", files.len());

    // --- Case 1: cold full-corpus parse ---
    // Warm the OS page cache / JIT-ish effects with one untimed pass of
    // each approach before taking the timed sample.
    let _ = parse_all_fold_shared(&files);
    let fold_cold = parse_all_fold_shared(&files);

    let _ = parse_all_salsa_threadlocal_cold(&files);
    let salsa_cold = parse_all_salsa_threadlocal_cold(&files);

    println!("\n== Case 1: cold full-corpus parse ({} files) ==", files.len());
    println!("  fold-shared NodeCache (today's real shape): {fold_cold:?}");
    println!("  salsa + thread-local NodeCache:              {salsa_cold:?}");
    let cold_delta = salsa_cold.as_secs_f64() / fold_cold.as_secs_f64() - 1.0;
    println!("  delta: {:+.1}%", cold_delta * 100.0);

    // --- Case 2: warm single-file edit ---
    // Single-file-parse timing is microsecond-scale -- a single sample
    // is dominated by noise (this is the exact case that's supposed to
    // decide the ticket, so it gets real sampling, not one shot each).
    // Each of `SAMPLES` iterations edits with a unique suffix (matching
    // `binder_bench.rs::bench_warm_single_edit`'s own rationale) so
    // nothing coincidentally short-circuits on a repeated identical
    // input; report the median, robust to the occasional scheduler-noise
    // outlier a mean isn't.
    const SAMPLES: usize = 300;

    fn median(mut samples: Vec<Duration>) -> Duration {
        samples.sort();
        samples[samples.len() / 2]
    }

    // Baseline: reparse just the one dirty file with a *fresh* NodeCache
    // each time (matching `parse_one`'s real per-call behavior -- its
    // fold-scoped `NodeCache` is created fresh every `from_files_cached`
    // call, never persisted across edits, so a dirty file that hits no
    // early-return cache entry always gets a genuinely empty
    // `NodeCache`; every other file is skipped entirely via `Freshness`,
    // never touched by this timed section at all).
    let base_text = &files[0].1;
    let warm_baseline = median(
        (0..SAMPLES)
            .map(|n| {
                let edited = format!("{base_text}\n// edit {n}");
                let start = Instant::now();
                let mut cache = NodeCache::default();
                std::hint::black_box(parse_compilation_unit_with_cache(&edited, &mut cache));
                start.elapsed()
            })
            .collect(),
    );

    // Candidate: db already warm (every file's input set once, matching
    // steady state after the cold run above), only the one dirty file's
    // input changes and only that file's query gets re-run each
    // iteration -- matching `BindCache`'s existing dirty-gating
    // (untouched files are never re-queried) -- against a `NodeCache`
    // that's accumulated real interning from every file this thread has
    // ever parsed (the cold-case warm-up above, run on this same
    // thread), not a fresh one -- the genuine structural difference from
    // the baseline: today's code can never carry `NodeCache` state
    // *across* separate edit-response calls at all, salsa's thread-local
    // can.
    let mut db = ProtoDb::default();
    let inputs: Vec<FileTextInput> = files
        .iter()
        .map(|(_, text)| FileTextInput::new(&mut db, text.clone()))
        .collect();
    let owned: Vec<(FileTextInput, ProtoDb)> =
        inputs.iter().map(|&input| (input, db.clone())).collect();
    owned.into_par_iter().for_each(|(input, db)| {
        std::hint::black_box(parse_query(&db, input));
    });

    let warm_salsa = median(
        (0..SAMPLES)
            .map(|n| {
                let edited = format!("{base_text}\n// edit {n}");
                let start = Instant::now();
                inputs[0].set_text(&mut db).to(edited);
                std::hint::black_box(parse_query(&db, inputs[0]));
                start.elapsed()
            })
            .collect(),
    );

    println!(
        "\n== Case 2: warm single-file-edit rebind (median of {SAMPLES} samples) =="
    );
    println!("  fresh NodeCache per dirty file (today's real shape): {warm_baseline:?}");
    println!("  salsa + thread-local NodeCache (steady state):       {warm_salsa:?}");
    let warm_delta = warm_salsa.as_secs_f64() / warm_baseline.as_secs_f64() - 1.0;
    println!("  delta: {:+.1}%", warm_delta * 100.0);
}
