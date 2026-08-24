//! Direct, LSP-layer-independent coverage of `ReferenceTable`'s reverse
//! index (`ReferenceTable::references_to`/`BoundProgram::references_to`)
//! -- the primitive `textDocument/references`/`textDocument/documentHighlight`
//! are built on (`BACKLOG.md` §3). Follows `extends_chain_resolution.rs`'s
//! fixture-on-disk pattern.

use apex_binder::{BoundProgram, SymbolKind};

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

#[test]
fn references_to_a_method_crosses_file_boundaries() {
    let dir = write_fixture_dir(
        "refs-cross-file",
        &[
            ("Base.cls", "public class Base { public void greet() { } }"),
            (
                "CallerA.cls",
                "public class CallerA { public void run() { new Base().greet(); } }",
            ),
            (
                "CallerB.cls",
                "public class CallerB { public void run() { new Base().greet(); } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greet_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "greet")
        .map(|(id, _)| id)
        .expect("Base.greet should have been collected");

    let caller_a_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "CallerA")
        .map(|(_, s)| s.file)
        .expect("CallerA should have been collected");
    let caller_b_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "CallerB")
        .map(|(_, s)| s.file)
        .expect("CallerB should have been collected");

    let refs: Vec<_> = program.references_to(greet_id).collect();
    assert_eq!(
        refs.len(),
        2,
        "expected exactly 2 references to Base.greet, project-wide: {refs:?}"
    );
    assert!(
        refs.iter().any(|ptr| ptr.file() == caller_a_file),
        "expected a reference in CallerA.cls: {refs:?}"
    );
    assert!(
        refs.iter().any(|ptr| ptr.file() == caller_b_file),
        "expected a reference in CallerB.cls: {refs:?}"
    );
}

#[test]
fn references_to_scoped_to_one_file_excludes_other_files() {
    let dir = write_fixture_dir(
        "refs-scoped",
        &[
            ("Base.cls", "public class Base { public void greet() { } }"),
            (
                "CallerA.cls",
                "public class CallerA { public void run() { new Base().greet(); } }",
            ),
            (
                "CallerB.cls",
                "public class CallerB { public void run() { new Base().greet(); } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greet_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "greet")
        .map(|(id, _)| id)
        .expect("Base.greet should have been collected");
    let caller_a_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "CallerA")
        .map(|(_, s)| s.file)
        .expect("CallerA should have been collected");

    let refs: Vec<_> = program
        .references_to_in_file(caller_a_file, greet_id)
        .collect();
    assert_eq!(
        refs.len(),
        1,
        "expected exactly 1 reference to Base.greet scoped to CallerA.cls: {refs:?}"
    );
    assert_eq!(refs[0].file(), caller_a_file);
}

#[test]
fn references_to_reaches_every_candidate_of_an_ambiguous_overload() {
    // Both overloads take a plain `Object` -- a system type, never
    // eliminated by `narrow_by_overload`'s type-based narrowing (see its
    // doc comment: "system-vs-system comparisons are deliberately never
    // attempted") -- so the call site stays `Resolution::Candidates`
    // rather than narrowing to one `Resolved` id.
    let dir = write_fixture_dir(
        "refs-candidates",
        &[
            (
                "Ambiguous.cls",
                "public class Ambiguous { \
                 public void process(Object a) { } \
                 public void process(Object b) { } \
             }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 public void run(Ambiguous a) { a.process(null); } \
             }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let process_ids: Vec<_> = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "process")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        process_ids.len(),
        2,
        "expected both process(Object) overloads to be collected"
    );

    for &id in &process_ids {
        let refs: Vec<_> = program.references_to(id).collect();
        assert_eq!(
            refs.len(),
            1,
            "expected the ambiguous call site to be recorded as a reference \
             to every candidate overload, including {id:?}: {refs:?}"
        );
    }
}
