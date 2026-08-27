//! Ad-hoc heap profiling harness, not a permanent fixture -- answers
//! "what's actually eating apexls-server's ~180-200MB" by dhat-profiling
//! the exact steady-state shape `apexls-server::Backend` keeps resident
//! (a `BindCache` *and* a separately-owned `BoundProgram` snapshot alive
//! at once, per `crates/apexls-server/src/main.rs`'s `BindState`), over
//! the real NPSP corpus. Run with:
//!   cargo run -p apex-binder --release --example mem_profile
//! Writes `dhat-heap.json` (viewable at
//! https://nnethercote.github.io/dh_view/dh_view.html) and prints a
//! summary to stdout.

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    let _profiler = dhat::Profiler::new_heap();

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    assert!(
        root.exists(),
        "no NPSP corpus found at {}; is the submodule checked out?",
        root.display()
    );

    let mut cache = apex_binder::BindCache::default();
    let overrides = std::collections::HashMap::new();
    let program = apex_binder::BoundProgram::from_files_cached(&root, &overrides, &mut cache);

    let stats = dhat::HeapStats::get();
    println!(
        "curr: {} bytes / {} blocks   max: {} bytes / {} blocks",
        stats.curr_bytes, stats.curr_blocks, stats.max_bytes, stats.max_blocks
    );

    // Keep both alive until after the stats snapshot above -- this is
    // the point of the harness: measure the *retained* shape, not a
    // transient peak during construction. Deliberately leaked (not just
    // `black_box`ed) rather than let normally drop at the end of `main`:
    // `dhat-heap.json`'s per-allocation-site breakdown is only written
    // when `_profiler` itself drops, which (declared first) happens
    // *after* `cache`/`program` (declared later) already dropped -- so
    // without this, the JSON's own "still live at t-end" breakdown
    // reflects almost nothing (just process-lifetime statics), making
    // the interactive per-site view at https://nnethercote.github.io/dh_view/dh_view.html
    // useless for exactly the question this harness exists to answer.
    // `curr_bytes`/`max_bytes` above are unaffected either way, since
    // they were already captured before this point.
    std::mem::forget((cache, program));
}
