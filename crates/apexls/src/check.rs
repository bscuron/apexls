//! `apexls check [paths...]`: batch, cargo-check-style diagnostics report
//! for the whole project, reusing exactly the analysis
//! `apexls-server`'s `textDocument/publishDiagnostics` is built on
//! (`apexls_server::diagnostics_for_file`) -- syntax errors, dead code,
//! unresolved references, type mismatches, and everything else that
//! combines into, now shared instead of living only inside an LSP
//! session.
//!
//! Always binds the *whole* detected project -- there's no `--root` flag
//! and no partial-project bind. `paths` (zero or more files/directories)
//! is a pure post-bind **report filter**: with no arguments, every file
//! in the project is reported on; with one or more arguments, only
//! findings in files under one of those paths are printed. Binding the
//! whole project regardless (rather than trying to bind just the
//! requested subset) is simpler and strictly more correct: cross-file
//! information (inheritance chains, project-wide references for `public`
//! candidates) elsewhere in the bind could be incomplete from a partial
//! project subset, and `apex-binder` has no existing hook for multi-root/
//! partial binding to build this on top of anyway.

use crate::project::{canonicalize_filters, find_project_root, matches_any, ArgError};
use apex_binder::BoundProgram;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug)]
struct Finding {
    path: PathBuf,
    line: usize,
    col: usize,
    severity: &'static str,
    message: String,
}

pub fn run(paths: &[PathBuf]) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let findings = match find_findings(paths, &cwd) {
        Ok(findings) => findings,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    for f in &findings {
        println!(
            "{}:{}:{}: {}: {}",
            f.path.display(),
            f.line,
            f.col,
            f.severity,
            f.message
        );
    }

    // Matches `cargo check`'s own exit convention: warnings are printed
    // but don't fail the run, only an actual error does.
    if findings.iter().any(|f| f.severity == "error") {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The actual report logic, factored out of [`run`] so it's testable
/// without touching the real process-global CWD (unsafe to mutate from a
/// parallel test binary) or capturing stdout -- `run` itself stays a thin
/// CWD-detection-plus-printing shell, the same "protocol-agnostic core,
/// thin glue" split `apex-binder`'s own capability modules already use.
fn find_findings(paths: &[PathBuf], cwd: &Path) -> Result<Vec<Finding>, ArgError> {
    let filters = canonicalize_filters(paths)?;

    let root = find_project_root(cwd);
    let program = BoundProgram::from_files(&root);

    // Each file's report is independent of every other's, so it
    // parallelizes over `rayon` exactly like `apexls dead` used to and
    // the bind that built `program` already does. The `Vec<Finding>`
    // -per-file results are flattened and globally sorted below rather
    // than printed as they land, since `program.files()`'s own order is
    // hash-set-derived, not path order -- streaming would make output
    // order vary run to run for no benefit once this loop is already
    // fast.
    let files: Vec<apex_binder::FileId> = program.files().collect();
    let mut findings: Vec<Finding> = files
        .par_iter()
        .filter_map(|&file| {
            let file_path = program.file_path(file);
            // Skip the syscall entirely when there's nothing to filter
            // against -- `matches_any` already treats an empty `filters`
            // as "matches everything" regardless of the canonicalized
            // path, so canonicalizing every file up front bought nothing
            // in the (default, no-arguments) whole-project case.
            if !filters.is_empty() {
                let canon_file_path = file_path
                    .canonicalize()
                    .unwrap_or_else(|_| file_path.to_path_buf());
                if !matches_any(&canon_file_path, &filters) {
                    return None;
                }
            }
            let diagnostics = apexls_server::diagnostics_for_file(&program, file);
            if diagnostics.is_empty() {
                return None;
            }
            let display_path = file_path.strip_prefix(cwd).unwrap_or(file_path);
            Some(
                diagnostics
                    .into_iter()
                    .map(|d| Finding {
                        path: display_path.to_path_buf(),
                        line: d.line,
                        col: d.col,
                        severity: d.severity,
                        message: d.message,
                    })
                    .collect::<Vec<Finding>>(),
            )
        })
        .flatten()
        .collect();

    findings.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apexls-check-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn find_findings_reports_a_syntax_error_with_a_correct_location() {
        let dir = temp_dir("findings-basic");
        std::fs::write(dir.join("Foo.cls"), "public class Foo {\n").unwrap();
        let findings = find_findings(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert!(!findings.is_empty(), "expected at least one diagnostic");
        assert!(findings.iter().any(|f| f.severity == "error"));
    }

    #[test]
    fn find_findings_reports_dead_code_as_a_warning() {
        let dir = temp_dir("findings-dead");
        std::fs::write(
            dir.join("Foo.cls"),
            "public class Foo {\n    private void helper() { }\n}\n",
        )
        .unwrap();
        let findings = find_findings(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(findings.len(), 1, "expected exactly one diagnostic");
        let f = &findings[0];
        assert_eq!(f.line, 2);
        assert_eq!(f.severity, "warning");
        assert!(
            f.message.contains("helper") && f.message.contains("never used"),
            "unexpected message: {}",
            f.message
        );
    }

    #[test]
    fn find_findings_is_empty_when_nothing_is_wrong() {
        let dir = temp_dir("findings-none");
        std::fs::write(
            dir.join("Foo.cls"),
            "public class Foo {\n    @AuraEnabled\n    public static void run() { }\n}\n",
        )
        .unwrap();
        let findings = find_findings(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            findings.is_empty(),
            "a platform-invocation-exempt public method should report nothing: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn find_findings_filters_to_only_the_requested_path() {
        let dir = temp_dir("findings-filtered");
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

        let findings =
            find_findings(&[dir.join("included")], &dir).expect("a real, existing path argument");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            findings.len(),
            1,
            "expected only the filtered-in file's finding: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
        assert!(findings[0].message.contains("deadHere"));
    }

    #[test]
    fn find_findings_rejects_a_nonexistent_path_argument() {
        let dir = temp_dir("findings-bad-path");
        let missing = dir.join("DoesNotExist.cls");
        let err = find_findings(std::slice::from_ref(&missing), &dir)
            .expect_err("a nonexistent path should error");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(err.1, 2);
        assert!(
            err.0.contains("does not exist"),
            "unexpected message: {}",
            err.0
        );
    }
}
