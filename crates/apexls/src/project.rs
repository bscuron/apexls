//! Shared plumbing for every subcommand that walks a whole Apex project
//! and filters its report down to a set of requested paths: project-root
//! detection, the path-filter predicate, the one path-argument error shape
//! they can all hit, and -- for the commands that only *parse* rather than
//! bind (`soql`, `query`) -- the parallel walk itself and the located-hit
//! record it produces.
//!
//! `check`/`fix` share the first group but not the walk, since they build a
//! whole `BoundProgram` instead of visiting files independently.

use apexls_server::LineIndex;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Walks upward from `start` looking for `sfdx-project.json` -- the real,
/// standard SFDX/Salesforce DX project-root marker -- falling back to
/// `start` itself if no marker is found anywhere above it. Every
/// subcommand that needs a project root always runs *in* the project;
/// there's no override flag, matching how `cargo`/`git` locate their own
/// project roots by upward walk rather than taking one as an argument.
pub(crate) fn find_project_root(start: &Path) -> PathBuf {
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
pub(crate) fn matches_any(file_path: &Path, filters: &[PathBuf]) -> bool {
    filters.is_empty() || filters.iter().any(|f| file_path.starts_with(f))
}

/// A path argument that doesn't exist, or can't be canonicalized -- the
/// message to print to stderr, paired with the process exit code to use.
#[derive(Debug)]
pub(crate) struct ArgError(pub String, pub u8);

/// Validates every one of `paths` exists, then canonicalizes them into the
/// filter list [`matches_any`] expects. Shared by every subcommand that
/// takes `paths` as a post-bind report/apply filter.
pub(crate) fn canonicalize_filters(paths: &[PathBuf]) -> Result<Vec<PathBuf>, ArgError> {
    for p in paths {
        if !p.exists() {
            return Err(ArgError(
                format!("error: path does not exist: {}", p.display()),
                2,
            ));
        }
    }
    paths
        .iter()
        .map(|p| p.canonicalize())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            ArgError(
                format!("error: failed to resolve a path argument: {e}"),
                2,
            )
        })
}

/// One located hit, printed ripgrep `--vimgrep`-style as
/// `path:line:col:text`. Shared by every grep-shaped subcommand so the
/// output contract lives in exactly one place.
#[derive(Debug)]
pub(crate) struct Site {
    pub(crate) path: PathBuf,
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) text: String,
}

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.path.display(),
            self.line,
            self.col,
            self.text
        )
    }
}

/// Build a [`Site`] for `node`, anchored on its *significant* span.
///
/// Never the raw `text_range()` -- see `apex_syntax::significant_range` for
/// why that would report a preceding comment's position and print the
/// comment as part of the hit. Returns `None` for a node with no
/// non-trivia tokens.
pub(crate) fn site_for(
    display_path: &Path,
    src: &str,
    index: &LineIndex,
    node: &apex_syntax::SyntaxNode,
) -> Option<Site> {
    let range = apex_syntax::significant_range(node)?;
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let (line, col) = index.line_col(src, start as u32);
    Some(Site {
        path: display_path.to_path_buf(),
        line,
        col,
        text: collapse(&src[start..end]),
    })
}

/// Squashes every run of whitespace (and every line break) down to a single
/// space, so one hit is one output line. Collapses runs inside a string
/// literal too -- accepted deliberately: this is a grep-shaped locator
/// format, and exact internal spacing is something to go read at
/// `path:line:col`, not something this output claims to preserve.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one discovered Apex file according to what kind of file it is.
///
/// A `.trigger` is not a compilation unit -- parsing one as a class yields
/// an error tree, silently dropping everything written inside it (the bug
/// commit `74f8746` fixed for `soql`). Keeping the dispatch here means the
/// next command to walk the project inherits the fix instead of
/// rediscovering it. Dispatched on the extension exactly as `apex_binder`
/// does (`db.rs`'s `trigger` input flag).
pub(crate) fn parse_apex_file(display_path: &Path, src: &str) -> apex_parser::Parse {
    let is_trigger = display_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
    if is_trigger {
        apex_parser::parse_trigger_unit(src)
    } else {
        apex_parser::parse_compilation_unit(src)
    }
}

/// Walk every Apex file in the project containing `cwd`, filtered to
/// `paths`, running `extract` on each and returning every hit sorted by
/// location.
///
/// Parse-only: nothing here builds a `BoundProgram`, which is what lets a
/// whole-corpus search run in well under a second instead of paying the
/// binder's ~249 MB peak.
pub(crate) fn walk_project<F>(
    paths: &[PathBuf],
    cwd: &Path,
    extract: F,
) -> Result<Vec<Site>, ArgError>
where
    F: Fn(&Path, &str) -> Vec<Site> + Sync,
{
    let filters = canonicalize_filters(paths)?;

    // Parsing happens on rayon's workers below, and a pathologically long
    // chain expression can overflow a default-sized stack purely on
    // *dropping* its parsed tree (see `apex_parser`'s module doc comment).
    // No `BoundProgram` is built here, so nothing else would have set the
    // pool up. Failure only means the global pool was already built (by an
    // earlier call in this process, or by a test binary), whose stack size
    // is then outside this command's control.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(apex_parser::RECOMMENDED_MIN_STACK_SIZE)
        .build_global();

    let root = find_project_root(cwd);
    let files = apex_discover::find_apex_files(&root);

    let mut sites: Vec<Site> = files
        .par_iter()
        .filter(|path| {
            // Skip the syscall entirely when there's nothing to filter
            // against -- `matches_any` treats an empty `filters` as
            // "matches everything" regardless, so canonicalizing every
            // discovered file up front would buy nothing in the (default,
            // no-arguments) whole-project case.
            filters.is_empty() || {
                let canon = path.canonicalize().unwrap_or_else(|_| (*path).clone());
                matches_any(&canon, &filters)
            }
        })
        .filter_map(|path| {
            // An unreadable file is skipped rather than failing the whole
            // run, matching how `apex_discover`'s own walk treats an entry
            // it can't read.
            let src = std::fs::read_to_string(path).ok()?;
            let path = path.as_path();
            let display_path = path.strip_prefix(cwd).unwrap_or(path);
            Some(extract(display_path, &src))
        })
        .flatten()
        .collect();

    // Globally sorted rather than printed as each file lands:
    // `find_apex_files`' order is walk order, which varies run to run with
    // a parallel walker.
    sites.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));

    Ok(sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apexls-project-cli-{name}-{}", std::process::id()));
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
    fn canonicalize_filters_rejects_a_nonexistent_path_argument() {
        let dir = temp_dir("canonicalize-bad-path");
        let missing = dir.join("DoesNotExist.cls");
        let err = canonicalize_filters(std::slice::from_ref(&missing))
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
