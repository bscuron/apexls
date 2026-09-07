//! Shared plumbing for every subcommand that binds a whole Apex project
//! and then filters its report/output down to a set of requested paths
//! (`check`, `fix`): project-root detection, the path-filter predicate,
//! and the one path-argument error shape both commands can hit.

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
