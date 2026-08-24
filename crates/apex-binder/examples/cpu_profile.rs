//! Ad-hoc `hotpath` profiling harness, not a permanent fixture -- breaks
//! down *where inside* the bind pipeline (discover -> parse -> Pass 1
//! collect -> Pass 1.5 inherit -> Pass 2 resolve -> merge) the already-
//! measured whole-call totals actually go: `corpus/bind_npsp_full`'s
//! ~431ms cold `BoundProgram::from_files` and
//! `corpus/warm_rebind_after_one_file_edit`'s ~17ms warm
//! `from_files_cached` rebind (both from `benches/binder_bench.rs`,
//! narrated in `BACKLOG.md` §2), which only measure the outside of each
//! call. Mirrors that bench's two scenarios exactly so the totals here
//! are directly comparable to those baselines. Run with:
//!   cargo run -p apex-binder --release --features hotpath,hotpath-alloc --example cpu_profile
//! `hotpath-cpu` (sampling-based CPU attribution) is Linux/macOS only --
//! omit it on Windows, where this was written; add it back on a
//! supported platform for CPU-sample attribution alongside timing.
//! Prints a per-function calls/avg/P95/total/%-total report to stdout on
//! each scenario's guard drop.

use hotpath::{CountingAllocator, HotpathGuardBuilder};
use std::collections::HashMap;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::new();

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn main() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {}; is the submodule checked out? (git submodule update --init --recursive)",
        root.display()
    );

    println!("=== cold: BoundProgram::from_files ===");
    {
        let _guard = HotpathGuardBuilder::new("cold_bind_npsp_full").build();
        std::hint::black_box(apex_binder::BoundProgram::from_files(std::hint::black_box(
            &root,
        )));
    }

    println!("\n=== warm: from_files_cached after one file's body-only edit ===");
    {
        let mut cache = apex_binder::BindCache::default();
        apex_binder::BoundProgram::from_files_cached(&root, &HashMap::new(), &mut cache);

        let target = apex_discover::find_apex_files(&root)
            .into_iter()
            .next()
            .expect("corpus has at least one file to simulate editing");
        let original = std::fs::read_to_string(&target).unwrap();
        let mut overrides = HashMap::new();
        overrides.insert(target, format!("{original}\n// edit"));

        let _guard = HotpathGuardBuilder::new("warm_rebind_after_one_file_edit").build();
        std::hint::black_box(apex_binder::BoundProgram::from_files_cached(
            std::hint::black_box(&root),
            std::hint::black_box(&overrides),
            &mut cache,
        ));
    }
}
