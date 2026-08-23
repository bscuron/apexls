//! Differential test: `find_apex_files`'s pruned walk must find exactly
//! the same `.cls`/`.trigger` files as a naive walk that recurses into
//! *everything* and never prunes. This is the actual proof (not just an
//! assertion) that the directory-name skip list in `apex-discover::skip`
//! never drops a real Apex file -- run against the same real-world NPSP
//! corpus used by `apex-lexer`'s round-trip test.

use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn naive_find_apex_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            naive_find_apex_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger")
            })
        {
            out.push(path);
        }
    }
}

#[test]
fn pruned_walk_matches_naive_walk() {
    let root = corpus_root();

    let mut expected = Vec::new();
    naive_find_apex_files(&root, &mut expected);
    assert!(
        !expected.is_empty(),
        "no .cls/.trigger files found under {}; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)",
        root.display()
    );
    expected.sort();

    let mut actual = apex_discover::find_apex_files(&root);
    actual.sort();

    assert_eq!(
        actual, expected,
        "pruned walk disagrees with the naive walk -- the skip list in \
         apex_discover::skip is dropping (or over-including) real files"
    );
}
