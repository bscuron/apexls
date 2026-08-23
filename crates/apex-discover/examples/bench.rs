//! Quick timing check for `find_apex_files` against a real corpus (NPSP by
//! default), plus a naive unpruned walk for comparison. Run in release
//! mode -- debug builds are not representative:
//!
//!   cargo run --release -p apex-discover --example bench

use std::path::{Path, PathBuf};
use std::time::Instant;

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
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger"))
        {
            out.push(path);
        }
    }
}

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/npsp").to_string());
    const RUNS: usize = 7;

    let mut pruned_us = Vec::new();
    let mut found_count = 0;
    for _ in 0..RUNS {
        let start = Instant::now();
        let files = apex_discover::find_apex_files(&root);
        pruned_us.push(start.elapsed().as_micros());
        found_count = files.len();
    }

    let mut naive_us = Vec::new();
    for _ in 0..RUNS {
        let mut out = Vec::new();
        let start = Instant::now();
        naive_find(Path::new(&root), &mut out);
        naive_us.push(start.elapsed().as_micros());
    }

    let pruned_med = median(pruned_us);
    let naive_med = median(naive_us);

    println!("root: {root}");
    println!("files found: {found_count}");
    println!("pruned parallel walk: median {pruned_med} us over {RUNS} runs");
    println!("naive single-thread walk: median {naive_med} us over {RUNS} runs");
    println!(
        "speedup: {:.1}x",
        naive_med as f64 / pruned_med.max(1) as f64
    );
}
