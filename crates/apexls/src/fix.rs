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

use crate::project::{canonicalize_filters, find_project_root, matches_any, ArgError};
use apex_binder::BoundProgram;
use apexls_server::CliFix;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
fn fix_project(paths: &[PathBuf], cwd: &Path) -> Result<Vec<FileOutcome>, ArgError> {
    let filters = canonicalize_filters(paths)?;

    let root = find_project_root(cwd);
    let program = BoundProgram::from_files(&root);

    let files: Vec<apex_binder::FileId> = program.files().collect();
    // Resolving each file's candidate fixes is independent of every
    // other's, so it parallelizes the same way `check::find_findings`
    // does; only the actual disk write below happens sequentially, since
    // that's real file I/O rather than in-memory computation.
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

    let mut outcomes = Vec::with_capacity(per_file.len());
    for (file_path, display_path, resolution) in per_file {
        if let Some(new_text) = &resolution.new_text {
            std::fs::write(&file_path, new_text).map_err(|e| {
                ArgError(
                    format!("error: failed to write {}: {e}", file_path.display()),
                    1,
                )
            })?;
        }
        outcomes.push(FileOutcome {
            display_path,
            applied: resolution.applied,
            conflicts: resolution.conflicts,
        });
    }

    Ok(outcomes)
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
