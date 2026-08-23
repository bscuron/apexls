//! Differential test for `discover` (the unified source+metadata walk),
//! same technique as `matches_naive_walk.rs`: compare against a naive
//! walk that recurses into everything and never prunes, run against the
//! real NPSP corpus. Also checks `discover`'s `apex_files` agrees exactly
//! with `find_apex_files`'s own pruned walk, since the two use different
//! prune lists (`discover` must not prune `objects`/`fields`) and should
//! still agree on every `.cls`/`.trigger` file either way.

use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

#[derive(Default)]
struct Naive {
    apex_files: Vec<PathBuf>,
    object_meta_files: Vec<PathBuf>,
    field_meta_files: Vec<PathBuf>,
}

fn naive_discover(dir: &Path, out: &mut Naive) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            naive_discover(&path, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger")
            })
        {
            out.apex_files.push(path);
        } else if name.to_ascii_lowercase().ends_with(".object-meta.xml") {
            out.object_meta_files.push(path);
        } else if name.to_ascii_lowercase().ends_with(".field-meta.xml") {
            out.field_meta_files.push(path);
        }
    }
}

#[test]
fn pruned_discover_matches_naive_walk() {
    let root = corpus_root();

    let mut expected = Naive::default();
    naive_discover(&root, &mut expected);
    assert!(
        !expected.apex_files.is_empty() && !expected.field_meta_files.is_empty(),
        "expected the NPSP submodule to be checked out under {}",
        root.display()
    );
    expected.apex_files.sort();
    expected.object_meta_files.sort();
    expected.field_meta_files.sort();

    let mut actual = apex_discover::discover(&root);
    actual.apex_files.sort();
    actual.object_meta_files.sort();
    actual.field_meta_files.sort();

    assert_eq!(
        actual.apex_files, expected.apex_files,
        "discover's apex_files disagrees with a naive walk"
    );
    assert_eq!(
        actual.object_meta_files, expected.object_meta_files,
        "discover's object_meta_files disagrees with a naive walk"
    );
    assert_eq!(
        actual.field_meta_files, expected.field_meta_files,
        "discover's field_meta_files disagrees with a naive walk"
    );
}

#[test]
fn discover_and_find_apex_files_agree_on_apex_files() {
    let root = corpus_root();

    let mut from_discover = apex_discover::discover(&root).apex_files;
    let mut from_find_apex_files = apex_discover::find_apex_files(&root);
    from_discover.sort();
    from_find_apex_files.sort();

    assert_eq!(
        from_discover, from_find_apex_files,
        "discover() and find_apex_files() must find the same .cls/.trigger files despite using different prune lists"
    );
}
