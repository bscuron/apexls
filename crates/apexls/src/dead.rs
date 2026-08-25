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

pub fn run(paths: &[PathBuf]) -> ExitCode {
    for p in paths {
        if !p.exists() {
            eprintln!("error: path does not exist: {}", p.display());
            return ExitCode::from(2);
        }
    }
    let filters: Vec<PathBuf> = match paths.iter().map(|p| p.canonicalize()).collect() {
        Ok(canon) => canon,
        Err(e) => {
            eprintln!("error: failed to resolve a path argument: {e}");
            return ExitCode::from(2);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = find_project_root(&cwd);
    let program = BoundProgram::from_files(&root);

    struct Finding {
        path: PathBuf,
        line: usize,
        col: usize,
        message: String,
    }
    let mut findings: Vec<Finding> = Vec::new();

    for file in program.files() {
        let file_path = program.file_path(file);
        let canon_file_path = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
        if !matches_any(&canon_file_path, &filters) {
            continue;
        }
        let dead = apex_binder::dead_symbols_in_file(&program, file);
        if dead.is_empty() {
            continue;
        }
        let text = program.syntax(file).text().to_string();
        let starts = line_starts(&text);
        let display_path = file_path.strip_prefix(&cwd).unwrap_or(file_path);
        for d in dead {
            let (line, col) = line_col(&starts, &text, d.name_range.start().into());
            findings.push(Finding {
                path: display_path.to_path_buf(),
                line,
                col,
                message: format!(
                    "{} '{}' is never used",
                    apex_binder::kind_label(d.kind, d.visibility),
                    d.name
                ),
            });
        }
    }

    findings.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));

    for f in &findings {
        println!("{}:{}:{}: {}", f.path.display(), f.line, f.col, f.message);
    }

    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("apexls-dead-cli-{name}-{}", std::process::id()));
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
        assert!(matches_any(Path::new("/proj/force-app/Foo/Bar.cls"), &filters));
        // `Foo2.cls` must not match a `Foo` directory filter -- this is
        // exactly what `Path::starts_with`'s component-awareness buys
        // over naive string prefixing.
        assert!(!matches_any(Path::new("/proj/force-app/Foo2.cls"), &filters));
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
        let nested = dir.join("force-app").join("main").join("default").join("classes");
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
}
