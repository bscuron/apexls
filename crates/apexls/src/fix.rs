//! `apexls fix [paths...]`: batch-applies every fixable diagnostic
//! `apexls check` would otherwise just report -- cargo-fix-style. Always
//! writes; there is no dry-run mode. Reuses the exact same
//! protocol-agnostic candidate-fix production and conflict resolution
//! (`apexls_server::resolve_fixes_for_file`) the LSP's own
//! `textDocument/codeAction` handlers are built on, so a fix behaves
//! identically whether it's applied by an editor's quick-fix menu or by
//! this batch command.
//!
//! Mirrors `check`'s own positioning exactly: always binds the *whole*
//! project (no `--root`, no partial-project bind), and `paths` is a pure
//! post-bind filter -- with no arguments, every fixable file in the
//! project is fixed; with one or more arguments, only files under one of
//! those paths are touched.
//!
//! Cascades: applying a fix can make another finding newly fixable (e.g.
//! removing a dead method removes its only call to a sibling method,
//! which is then itself dead). So a single bind-fix-write pass isn't
//! enough -- [`fix_project`] loops pass-by-pass, re-binding (via a
//! persistent [`apex_binder::BindCache`] so unchanged files' parses stay
//! cached) and re-fixing after each write, until a pass applies nothing
//! new. [`MAX_PASSES`] is a defensive cap only -- every pass strictly
//! deletes code and a fixed finding can't reappear, so convergence is
//! structurally guaranteed; the cap exists to fail loudly instead of
//! looping forever if that invariant is ever violated by a bug.

use crate::project::{canonicalize_filters, find_project_root, matches_any, ArgError};
use apex_binder::{BindCache, BoundProgram};
use apexls_server::CliFix;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Safety cap on the fix-rebind-fix loop -- see the module doc comment.
const MAX_PASSES: u32 = 20;

#[derive(Debug)]
struct FileOutcome {
    display_path: PathBuf,
    applied: Vec<CliFix>,
    conflicts: Vec<CliFix>,
}

pub fn run(paths: &[PathBuf]) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let outcomes = match fix_project(paths, &cwd) {
        Ok(outcomes) => outcomes,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    let mut any_conflicts = false;
    for outcome in &outcomes {
        for f in &outcome.applied {
            println!(
                "{}:{}:{}: fixed: {}",
                outcome.display_path.display(),
                f.line,
                f.col,
                f.description
            );
        }
        for f in &outcome.conflicts {
            println!(
                "{}:{}:{}: skipped (conflicts with another fix): {}",
                outcome.display_path.display(),
                f.line,
                f.col,
                f.description
            );
            any_conflicts = true;
        }
    }

    // A conflict is left exactly as `check` reported it and needs a human
    // to resolve by hand; an applied fix doesn't. Matches `check`'s own
    // exit convention of reserving failure for something still needing
    // attention.
    if any_conflicts {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The actual fix-and-write logic, factored out of [`run`] so it's
/// testable without touching the real process-global CWD -- see
/// `check::find_findings`'s own doc comment for why.
///
/// Loops pass-by-pass until a pass applies nothing (see the module doc
/// comment on cascades). The returned outcomes report every fix applied
/// across every pass (flattened, a caller can't tell which pass found
/// which fix), but only the *final* pass's conflicts -- an earlier pass's
/// conflict can stop being one once a later cascade fix changes the code
/// around it, so only the end state is worth a human's attention.
fn fix_project(paths: &[PathBuf], cwd: &Path) -> Result<Vec<FileOutcome>, ArgError> {
    let filters = canonicalize_filters(paths)?;
    let root = find_project_root(cwd);
    let mut cache = BindCache::default();

    let mut applied_by_file: BTreeMap<PathBuf, Vec<CliFix>> = BTreeMap::new();
    let mut conflicts_by_file: BTreeMap<PathBuf, Vec<CliFix>> = BTreeMap::new();

    for pass in 1..=MAX_PASSES {
        let program = BoundProgram::from_files_cached(&root, &HashMap::new(), &mut cache);
        let files: Vec<apex_binder::FileId> = program.files().collect();
        // Resolving each file's candidate fixes is independent of every
        // other's, so it parallelizes the same way `check::find_findings`
        // does; only the actual disk write below happens sequentially,
        // since that's real file I/O rather than in-memory computation.
        let mut per_file: Vec<(PathBuf, PathBuf, apexls_server::CliFixResolution)> = files
            .par_iter()
            .filter_map(|&file| {
                let file_path = program.file_path(file);
                if !filters.is_empty() {
                    let canon_file_path = file_path
                        .canonicalize()
                        .unwrap_or_else(|_| file_path.to_path_buf());
                    if !matches_any(&canon_file_path, &filters) {
                        return None;
                    }
                }
                let resolution = apexls_server::resolve_fixes_for_file(&program, file);
                if resolution.applied.is_empty() && resolution.conflicts.is_empty() {
                    return None;
                }
                let display_path = file_path.strip_prefix(cwd).unwrap_or(file_path).to_path_buf();
                Some((file_path.to_path_buf(), display_path, resolution))
            })
            .collect();

        per_file.sort_by(|a, b| a.1.cmp(&b.1));

        conflicts_by_file.clear();
        let mut pass_applied_count = 0;
        for (file_path, display_path, resolution) in per_file {
            if let Some(new_text) = &resolution.new_text {
                std::fs::write(&file_path, new_text).map_err(|e| {
                    ArgError(
                        format!("error: failed to write {}: {e}", file_path.display()),
                        1,
                    )
                })?;
            }
            pass_applied_count += resolution.applied.len();
            if !resolution.applied.is_empty() {
                applied_by_file
                    .entry(display_path.clone())
                    .or_default()
                    .extend(resolution.applied);
            }
            if !resolution.conflicts.is_empty() {
                conflicts_by_file.insert(display_path, resolution.conflicts);
            }
        }

        if pass_applied_count == 0 {
            let mut paths: Vec<PathBuf> = applied_by_file
                .keys()
                .chain(conflicts_by_file.keys())
                .cloned()
                .collect();
            paths.sort();
            paths.dedup();
            return Ok(paths
                .into_iter()
                .map(|p| FileOutcome {
                    applied: applied_by_file.remove(&p).unwrap_or_default(),
                    conflicts: conflicts_by_file.remove(&p).unwrap_or_default(),
                    display_path: p,
                })
                .collect());
        }
        if pass == MAX_PASSES {
            return Err(ArgError(
                format!(
                    "error: fix did not converge after {MAX_PASSES} passes -- a fix may be \
                     oscillating; please report this"
                ),
                1,
            ));
        }
    }

    unreachable!("loop always returns via convergence or the MAX_PASSES error above")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apexls-fix-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn removes_a_dead_private_method_and_writes_the_file() {
        let dir = temp_dir("dead-method");
        let file = dir.join("Foo.cls");
        std::fs::write(&file, "public class Foo {\n    private void helper() { }\n}\n").unwrap();

        let outcomes = fix_project(&[], &dir).expect("no path arguments to fail on");
        let rewritten = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].applied.len(), 1);
        assert!(outcomes[0].applied[0].description.contains("helper"));
        assert!(outcomes[0].conflicts.is_empty());
        assert!(
            !rewritten.contains("helper"),
            "expected the dead method to be removed: {rewritten}"
        );
        assert!(rewritten.contains("public class Foo"));
    }

    #[test]
    fn a_dead_method_containing_a_dead_local_is_removed_as_one_nested_fix() {
        let dir = temp_dir("nested-dead");
        let file = dir.join("Foo.cls");
        std::fs::write(
            &file,
            "public class Foo {\n    private void helper() {\n        Integer unused = 1;\n    }\n}\n",
        )
        .unwrap();

        let outcomes = fix_project(&[], &dir).expect("no path arguments to fail on");
        let rewritten = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // The whole dead method (which subsumes the dead local inside it)
        // is the only fix that actually lands -- the nested local-variable
        // fix is silently dropped, not double-applied or conflicted.
        assert_eq!(outcomes[0].applied.len(), 1);
        assert!(outcomes[0].applied[0].description.contains("helper"));
        assert!(outcomes[0].conflicts.is_empty());
        assert!(!rewritten.contains("helper"));
        assert!(!rewritten.contains("unused"));
    }

    #[test]
    fn a_fix_that_makes_a_sibling_method_newly_dead_is_caught_in_a_second_pass() {
        let dir = temp_dir("cascade");
        let file = dir.join("Foo.cls");
        // `helper1` is dead (nothing calls it) but its own body is
        // `helper2`'s only caller -- so removing `helper1` in pass 1
        // makes `helper2` newly dead, only catchable by re-binding and
        // fixing again.
        std::fs::write(
            &file,
            "public class Foo {\n    private void helper1() { helper2(); }\n    private void helper2() { }\n}\n",
        )
        .unwrap();

        let outcomes = fix_project(&[], &dir).expect("no path arguments to fail on");
        let rewritten = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].applied.len(), 2, "expected both cascade passes' fixes flattened together: {:?}", outcomes[0].applied);
        assert!(outcomes[0].applied.iter().any(|f| f.description.contains("helper1")));
        assert!(outcomes[0].applied.iter().any(|f| f.description.contains("helper2")));
        assert!(outcomes[0].conflicts.is_empty());
        assert!(!rewritten.contains("helper1"));
        assert!(!rewritten.contains("helper2"));
        assert!(rewritten.contains("public class Foo"));
    }

    #[test]
    fn leaves_clean_files_untouched() {
        let dir = temp_dir("clean");
        let file = dir.join("Foo.cls");
        // A public, zero-reference method still needs a real exemption
        // (like `check.rs`'s own "nothing wrong" test) or it's genuinely
        // dead code -- not a bug in `fix`, just what "clean" requires here.
        let original =
            "public class Foo {\n    @AuraEnabled\n    public static void run() { }\n}\n";
        std::fs::write(&file, original).unwrap();

        let outcomes = fix_project(&[], &dir).expect("no path arguments to fail on");
        let after = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(outcomes.is_empty());
        assert_eq!(after, original);
    }

    #[test]
    fn fix_project_filters_to_only_the_requested_path() {
        let dir = temp_dir("filtered");
        std::fs::create_dir_all(dir.join("included")).unwrap();
        std::fs::create_dir_all(dir.join("excluded")).unwrap();
        std::fs::write(
            dir.join("included").join("Included.cls"),
            "public class Included {\n    private void deadHere() { }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("excluded").join("Excluded.cls"),
            "public class Excluded {\n    private void alsoDead() { }\n}\n",
        )
        .unwrap();

        let outcomes = fix_project(&[dir.join("included")], &dir)
            .expect("a real, existing path argument");
        let excluded_after =
            std::fs::read_to_string(dir.join("excluded").join("Excluded.cls")).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].applied[0].description.contains("deadHere"));
        assert!(
            excluded_after.contains("alsoDead"),
            "the excluded file must not be touched: {excluded_after}"
        );
    }

    #[test]
    fn fix_project_rejects_a_nonexistent_path_argument() {
        let dir = temp_dir("bad-path");
        let missing = dir.join("DoesNotExist.cls");
        let err = fix_project(std::slice::from_ref(&missing), &dir)
            .expect_err("a nonexistent path should error");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(err.1, 2);
        assert!(err.0.contains("does not exist"), "unexpected message: {}", err.0);
    }
}
