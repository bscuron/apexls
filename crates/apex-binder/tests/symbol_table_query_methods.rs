//! Coverage for `SymbolTable::len`/`is_empty`/`by_name_ci` -- public API
//! surface no production code in this workspace happens to call yet
//! (every real call site reaches for a `Vec::len`/`is_empty` on some
//! other collection instead), so none of the three had ever run.

use apex_binder::BoundProgram;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

#[test]
fn len_and_is_empty_and_by_name_ci_reflect_the_bound_project() {
    let dir = write_fixture_dir(
        "symbol-table-query-methods",
        &[(
            "Foo.cls",
            "public class Foo { public Integer bar; public void run() { } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert!(!program.symbols.is_empty());
    assert!(
        program.symbols.len() >= 3,
        "expected at least Foo/bar/run to have been collected, got {}",
        program.symbols.len()
    );

    // Case-insensitive lookup: differently-cased queries for the same
    // real name should return the identical, non-empty candidate set.
    let lower = program.symbols.by_name_ci("foo");
    let upper = program.symbols.by_name_ci("FOO");
    let exact = program.symbols.by_name_ci("Foo");
    assert!(!lower.is_empty());
    assert_eq!(lower, upper);
    assert_eq!(lower, exact);

    // A name nothing declares returns an empty slice, not a panic.
    assert!(program
        .symbols
        .by_name_ci("TotallyUndeclaredName")
        .is_empty());
}
