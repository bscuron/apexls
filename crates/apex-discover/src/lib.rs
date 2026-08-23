//! Efficient `.cls`/`.trigger` discovery for Salesforce Apex repos.
//!
//! A plain recursive directory walk spends most of its time inside
//! metadata folders that can *never* contain Apex source (`aura`, `lwc`,
//! `objects`, `layouts`, ...). Since we know we're walking a Salesforce
//! repo, those directories are pruned by name instead of recursed into --
//! see [`skip`] for the full list and why each entry is safe.
//!
//! Traversal itself uses `ignore`'s parallel walker (the same one
//! ripgrep uses): directory reads are spread across a thread pool, which
//! is where the real performance win over a single-threaded walk comes
//! from on large trees. `.gitignore`/hidden-file auto-filtering is turned
//! off (`standard_filters(false)`) -- pruning is driven entirely by our
//! own explicit, tested skip list in [`skip`], not by whatever a repo's
//! `.gitignore` happens to say.

mod skip;

use ignore::{
    DirEntry, Error as IgnoreError, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Recursively find every `.cls`/`.trigger` file under `root`, pruning
/// known non-Apex-bearing directories along the way. Extension matching
/// is case-insensitive. Unreadable directories/entries (permissions,
/// races) are silently skipped rather than failing the whole walk.
pub fn find_apex_files(root: impl AsRef<Path>) -> Vec<PathBuf> {
    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();

    let walker = WalkBuilder::new(root.as_ref())
        .standard_filters(false)
        .build_parallel();

    let mut builder = CollectorBuilder { tx };
    walker.visit(&mut builder);
    drop(builder); // drop this thread's Sender clone so `rx` below can end

    rx.into_iter().flatten().collect()
}

fn is_apex_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger"))
}

struct CollectorBuilder {
    tx: mpsc::Sender<Vec<PathBuf>>,
}

impl<'s> ParallelVisitorBuilder<'s> for CollectorBuilder {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Collector {
            tx: self.tx.clone(),
            found: Vec::new(),
        })
    }
}

/// One per worker thread. Accumulates that thread's finds locally (no
/// cross-thread synchronization per file) and ships the whole batch home
/// in one message when the thread's walk work is done.
struct Collector {
    tx: mpsc::Sender<Vec<PathBuf>>,
    found: Vec<PathBuf>,
}

impl ParallelVisitor for Collector {
    fn visit(&mut self, entry: Result<DirEntry, IgnoreError>) -> WalkState {
        let Ok(entry) = entry else {
            return WalkState::Continue;
        };

        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if is_dir {
            let prunable = entry
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(skip::should_skip_dir);
            return if prunable {
                WalkState::Skip
            } else {
                WalkState::Continue
            };
        }

        if is_apex_file(entry.path()) {
            self.found.push(entry.into_path());
        }
        WalkState::Continue
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        // Ignore send errors: they only happen if the receiver already
        // hung up, meaning the caller stopped waiting for results.
        let _ = self.tx.send(std::mem::take(&mut self.found));
    }
}
