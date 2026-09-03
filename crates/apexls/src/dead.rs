//! `apexls dead [paths...]`: batch dead-code reporting, reusing exactly
//! the analysis `apexls-server`'s `textDocument/publishDiagnostics`/
//! `textDocument/codeAction` are built on (`apex_binder::dead_symbols_in_file`),
//! now shared instead of living only inside an LSP session.
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

use apex_binder::BoundProgram;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Walks upward from `start` looking for `sfdx-project.json` -- the real,
/// standard SFDX/Salesforce DX project-root marker -- falling back to
/// `start` itself if no marker is found anywhere above it. Every
/// subcommand that needs a project root always runs *in* the project;
/// there's no override flag, matching how `cargo`/`git` locate their own
/// project roots by upward walk rather than taking one as an argument.
fn find_project_root(start: &Path) -> PathBuf {
    let mut current = start;
    loop {
        if current.join("sfdx-project.json").is_file() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return start.to_path_buf(),
        }
    }
}

/// Whether `file_path` (already canonicalized) falls under one of
/// `filters` (also already canonicalized) -- a file filter matches only
/// itself, a directory filter matches itself and everything beneath it.
/// `Path::starts_with` is component-aware, not naive string prefixing, so
/// a `Foo2.cls` file correctly does not match a `Foo` directory filter.
/// An empty `filters` matches everything (the no-arguments, whole-project
/// case).
fn matches_any(file_path: &Path, filters: &[PathBuf]) -> bool {
    filters.is_empty() || filters.iter().any(|f| file_path.starts_with(f))
}

/// Byte offsets of the start of every line in `text`, `text[0..]`'s own
/// start included -- built once per file and reused across every finding
/// in it, rather than rescanning the file per finding.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// `offset`'s 1-based line/column against `starts` (from `line_starts`) --
/// column counted in `char`s, not bytes/UTF-16 code units, the
/// conventional choice for a plain-text CLI report (unlike the LSP
/// server, which must match whatever encoding the client negotiated).
fn line_col(starts: &[usize], text: &str, offset: usize) -> (usize, usize) {
    let line_idx = match starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i - 1,
    };
    let line_start = starts[line_idx];
    let col = text[line_start..offset].chars().count() + 1;
    (line_idx + 1, col)
}

#[derive(Debug)]
struct Finding {
    path: PathBuf,
    line: usize,
    col: usize,
    message: String,
}

/// A path argument that doesn't exist, or can't be canonicalized -- the
/// message to print to stderr, paired with the process exit code to use.
#[derive(Debug)]
struct ArgError(String, u8);

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
        println!("{}:{}:{}: {}", f.path.display(), f.line, f.col, f.message);
    }

    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The actual report logic, factored out of [`run`] so it's testable
/// without touching the real process-global CWD (unsafe to mutate from a
/// parallel test binary) or capturing stdout -- `run` itself stays a thin
/// CWD-detection-plus-printing shell, the same "protocol-agnostic core,
/// thin glue" split `apex-binder`'s own capability modules already use.
fn find_findings(paths: &[PathBuf], cwd: &Path) -> Result<Vec<Finding>, ArgError> {
    for p in paths {
        if !p.exists() {
            return Err(ArgError(
                format!("error: path does not exist: {}", p.display()),
                2,
            ));
        }
    }
    let filters: Vec<PathBuf> = match paths.iter().map(|p| p.canonicalize()).collect() {
        Ok(canon) => canon,
        Err(e) => {
            return Err(ArgError(
                format!("error: failed to resolve a path argument: {e}"),
                2,
            ));
        }
    };

    let root = find_project_root(cwd);
    let program = BoundProgram::from_files(&root);

    // Each file's report is independent of every other's -- computed
    // once (per-file `dead_symbols_in_file` result) and read-only from
    // there, so `program`'s per-file work parallelizes over `rayon`
    // exactly like the bind that built it already does. The `Vec<Finding>`
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
            let dead = apex_binder::dead_symbols_in_file(&program, file);
            if dead.is_empty() {
                return None;
            }
            let text = program.syntax(file).text().to_string();
            let starts = line_starts(&text);
            let display_path = file_path.strip_prefix(cwd).unwrap_or(file_path);
            Some(
                dead.into_iter()
                    .map(|d| {
                        let (line, col) = line_col(&starts, &text, d.name_range.start().into());
                        Finding {
                            path: display_path.to_path_buf(),
                            line,
                            col,
                            message: format!(
                                "{} '{}' is never used",
                                apex_binder::kind_label(d.kind, d.visibility),
                                d.name
                            ),
                        }
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
            std::env::temp_dir().join(format!("apexls-dead-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_filters_matches_everything() {
        assert!(matches_any(Path::new("/proj/Foo.cls"), &[]));
    }

    #[test]
    fn exact_file_match() {
        let filters = vec![PathBuf::from("/proj/force-app/Foo.cls")];
        assert!(matches_any(Path::new("/proj/force-app/Foo.cls"), &filters));
        assert!(!matches_any(Path::new("/proj/force-app/Bar.cls"), &filters));
    }

    #[test]
    fn directory_match_recurses_but_respects_path_components() {
        let filters = vec![PathBuf::from("/proj/force-app/Foo")];
        assert!(matches_any(
            Path::new("/proj/force-app/Foo/Bar.cls"),
            &filters
        ));
        // `Foo2.cls` must not match a `Foo` directory filter -- this is
        // exactly what `Path::starts_with`'s component-awareness buys
        // over naive string prefixing.
        assert!(!matches_any(
            Path::new("/proj/force-app/Foo2.cls"),
            &filters
        ));
    }

    #[test]
    fn line_col_finds_the_right_line_and_char_column() {
        let text = "public class Foo {\n    private void helper() { }\n}\n";
        let starts = line_starts(text);
        let offset = text.find("helper").unwrap();
        assert_eq!(line_col(&starts, text, offset), (2, 18));
    }

    #[test]
    fn root_detection_finds_a_nested_sfdx_project_json_from_a_deeper_cwd() {
        let dir = temp_dir("root-detection");
        std::fs::write(dir.join("sfdx-project.json"), "{}").unwrap();
        let nested = dir
            .join("force-app")
            .join("main")
            .join("default")
            .join("classes");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_project_root(&nested);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found, dir);
    }

    #[test]
    fn root_detection_falls_back_to_start_when_no_marker_exists_above() {
        let dir = temp_dir("root-detection-fallback");
        let found = find_project_root(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found, dir);
    }

    #[test]
    fn find_findings_reports_a_real_dead_symbol_with_a_correct_location() {
        let dir = temp_dir("findings-basic");
        std::fs::write(
            dir.join("Foo.cls"),
            "public class Foo {\n    private void helper() { }\n}\n",
        )
        .unwrap();
        let findings = find_findings(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(findings.len(), 1, "expected exactly one dead symbol");
        let f = &findings[0];
        assert_eq!(f.line, 2);
        assert!(
            f.message.contains("helper") && f.message.contains("never used"),
            "unexpected message: {}",
            f.message
        );
    }

    #[test]
    fn find_findings_is_empty_when_nothing_is_dead() {
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
