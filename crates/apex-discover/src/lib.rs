//! Efficient discovery of the files a Salesforce repo actually needs to
//! be read for: `.cls`/`.trigger` Apex source, plus (for callers that
//! also need SObject/field schema, e.g. `apex-metadata`)
//! `.object-meta.xml`/`.field-meta.xml`.
//!
//! A plain recursive directory walk spends most of its time inside
//! metadata folders that can *never* contain any of those -- see
//! [`skip`] for the full prune list and why each entry is safe. Which
//! directories are safe to prune depends on what the caller needs:
//! `objects`/`fields` can never contain Apex source, but they're the
//! *only* place object/field metadata lives, so [`find_apex_files`] and
//! [`discover`] use different prune lists (see [`skip::should_skip_dir`]
//! vs [`skip::should_skip_dir_keep_metadata_dirs`]).
//!
//! [`discover`] classifies every file it finds in a single walk rather
//! than requiring one walk per category -- important for a caller that
//! wants both Apex source and metadata (a real apexls invocation always
//! will), since running `find_apex_files` and a hypothetical standalone
//! metadata walk back to back would mean reading every directory in the
//! tree twice.
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
/// every known non-Apex-bearing directory, including `objects`/`fields`
/// (metadata-only directories, safe to skip when metadata itself isn't
/// wanted). Extension matching is case-insensitive. Unreadable
/// directories/entries (permissions, races) are silently skipped rather
/// than failing the whole walk.
///
/// Prefer [`discover`] instead if the caller also needs
/// `.object-meta.xml`/`.field-meta.xml` files -- calling both this and a
/// separate metadata walk over the same `root` means reading every
/// directory in the tree twice.
pub fn find_apex_files(root: impl AsRef<Path>) -> Vec<PathBuf> {
    walk(root, skip::should_skip_dir, |found, path| {
        if is_apex_file(path) {
            found.apex_files.push(path.to_path_buf());
        }
    })
    .apex_files
}

/// Recursively find every `.cls`/`.trigger`/`.object-meta.xml`/
/// `.field-meta.xml` file under `root` in a single walk -- what any
/// caller needing both Apex source and SObject/field schema (a real
/// apexls invocation) should use instead of `find_apex_files` plus a
/// separate metadata walk.
#[hotpath::measure]
pub fn discover(root: impl AsRef<Path>) -> Discovery {
    walk(
        root,
        skip::should_skip_dir_keep_metadata_dirs,
        |found, path| {
            if is_apex_file(path) {
                found.apex_files.push(path.to_path_buf());
            } else if is_object_meta_file(path) {
                found.object_meta_files.push(path.to_path_buf());
            } else if is_field_meta_file(path) {
                found.field_meta_files.push(path.to_path_buf());
            }
        },
    )
}

/// The result of [`discover`]: every interesting file found under a
/// repo root, categorized by what it is. Empty `Vec`s, not an `Option`
/// or an error, for a category that has no matches -- an all-standard-
/// schema repo with no custom objects is a normal, valid input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub apex_files: Vec<PathBuf>,
    pub object_meta_files: Vec<PathBuf>,
    pub field_meta_files: Vec<PathBuf>,
}

fn is_apex_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger"))
}

fn is_object_meta_file(path: &Path) -> bool {
    ends_with_ci(path, ".object-meta.xml")
}

fn is_field_meta_file(path: &Path) -> bool {
    ends_with_ci(path, ".field-meta.xml")
}

fn ends_with_ci(path: &Path, suffix: &str) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n.len() >= suffix.len() && n[n.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    })
}

/// Shared parallel-walk driver behind [`find_apex_files`] and
/// [`discover`]: both are "prune directories per `should_skip`, classify
/// every file per `classify`" with a different `should_skip`/`classify`
/// pair, so the actual traversal/threading machinery lives here once.
fn walk(
    root: impl AsRef<Path>,
    should_skip: fn(&str) -> bool,
    classify: fn(&mut Discovery, &Path),
) -> Discovery {
    let (tx, rx) = mpsc::channel::<Discovery>();

    let walker = WalkBuilder::new(root.as_ref())
        .standard_filters(false)
        .build_parallel();

    let mut builder = CollectorBuilder {
        tx,
        should_skip,
        classify,
    };
    walker.visit(&mut builder);
    drop(builder); // drop this thread's Sender clone so `rx` below can end

    let mut total = Discovery::default();
    for found in rx {
        total.apex_files.extend(found.apex_files);
        total.object_meta_files.extend(found.object_meta_files);
        total.field_meta_files.extend(found.field_meta_files);
    }
    total
}

struct CollectorBuilder {
    tx: mpsc::Sender<Discovery>,
    should_skip: fn(&str) -> bool,
    classify: fn(&mut Discovery, &Path),
}

impl<'s> ParallelVisitorBuilder<'s> for CollectorBuilder {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Collector {
            tx: self.tx.clone(),
            should_skip: self.should_skip,
            classify: self.classify,
            found: Discovery::default(),
        })
    }
}

/// One per worker thread. Accumulates that thread's finds locally (no
/// cross-thread synchronization per file) and ships the whole batch home
/// in one message when the thread's walk work is done.
struct Collector {
    tx: mpsc::Sender<Discovery>,
    should_skip: fn(&str) -> bool,
    classify: fn(&mut Discovery, &Path),
    found: Discovery,
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
                .is_some_and(self.should_skip);
            return if prunable {
                WalkState::Skip
            } else {
                WalkState::Continue
            };
        }

        (self.classify)(&mut self.found, entry.path());
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
