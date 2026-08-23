//! Differential correctness check for `BACKLOG.md` §2's incremental
//! rebind (`BoundProgram::from_files_cached`'s `BindCache`-backed fast
//! path): a body-only edit (touches a method's statements, not its
//! signature, so `Derived.cls`'s *declared shape* is unchanged) must
//! produce exactly the same resolutions via the warm incremental path as
//! a completely cold `from_files` rebuild of the same final text would.
//! The whole-corpus smoke tests elsewhere in this crate prove resolution
//! doesn't crash/regress in aggregate across `from_files` -- this is the
//! one test that specifically exercises `from_files_cached`'s reuse
//! logic (not just its cold-start behavior) and proves it never serves a
//! stale answer for the file that changed.

use apex_binder::{BindCache, BoundProgram, FileId, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::NameExpr;
use rowan::ast::AstNode;
use std::collections::HashMap;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

/// A `Resolution` described by symbol *name*/kind rather than raw
/// `SymbolId` -- `warm`/`cold` in the tests below are two entirely
/// independent `BoundProgram`s (`cold` built via a fresh, uncached
/// `from_files` call, with its own fresh `FileTable`/`SymbolId`
/// numbering), so comparing raw ids directly would spuriously fail
/// whenever `apex_discover::discover`'s directory walk happens to order
/// files differently between the two calls -- a real, harmless
/// nondeterminism in *numbering*, not a correctness difference. What
/// actually matters is whether both agree on *which declared symbol* a
/// reference names.
fn describe(program: &BoundProgram, resolution: &Resolution) -> String {
    let name_of = |id: apex_binder::SymbolId| {
        let s = program.symbols.get(id);
        format!("{:?}:{}", s.kind, s.name)
    };
    match resolution {
        Resolution::Resolved(id) => format!("Resolved({})", name_of(*id)),
        Resolution::Candidates(ids) => {
            let mut names: Vec<String> = ids.iter().copied().map(name_of).collect();
            names.sort();
            format!("Candidates({names:?})")
        }
        other => format!("{other:?}"),
    }
}

/// Every `NameExpr` resolution in `file`, in source order, described via
/// [`describe`] -- comparable across two independently-produced
/// `BoundProgram`s as long as both parsed the exact same file content
/// (so the AST shape, and thus this traversal order, matches).
fn name_expr_resolutions(program: &BoundProgram, file: FileId) -> Vec<Option<String>> {
    let root = program.syntax(file);
    root.descendants()
        .filter_map(NameExpr::cast)
        .map(|n| {
            program
                .resolution(SyntaxPtr::new(file, n.syntax()))
                .map(|r| describe(program, r))
        })
        .collect()
}

#[test]
fn body_only_edit_via_incremental_cache_matches_a_cold_rebuild() {
    let dir = write_fixture_dir(
        "incremental",
        &[
            (
                "Base.cls",
                "public virtual class Base { public Integer x; public void m() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public void n() { Integer y = x; m(); } }",
            ),
        ],
    );

    let mut cache = BindCache::default();
    let _first = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);

    // A body-only edit: one more statement inside `n()`, referencing
    // both the inherited field and a brand-new local -- `Derived`'s own
    // declared shape (its one method `n`, still no params, still
    // `void`) is completely unchanged, so this should hit the fast path
    // (only `Derived.cls` gets rebound, `Base.cls` untouched).
    let edited = "public class Derived extends Base { public void n() { Integer y = x; m(); Integer z = y; } }";
    std::fs::write(dir.join("Derived.cls"), edited).unwrap();

    let warm = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    let cold = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let warm_file = file_for_class(&warm, "Derived");
    let cold_file = file_for_class(&cold, "Derived");
    let warm_resolutions = name_expr_resolutions(&warm, warm_file);
    let cold_resolutions = name_expr_resolutions(&cold, cold_file);

    assert!(
        !warm_resolutions.is_empty(),
        "expected at least one NameExpr in the edited Derived.cls"
    );
    assert_eq!(
        warm_resolutions, cold_resolutions,
        "warm incremental rebind after a body-only edit disagreed with a cold rebuild"
    );
}

#[test]
fn declaration_changing_edit_via_incremental_cache_matches_a_cold_rebuild() {
    let dir = write_fixture_dir(
        "incremental-decl",
        &[
            (
                "Base.cls",
                "public virtual class Base { public Integer x; public void m() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public void n() { Integer y = x; m(); } }",
            ),
        ],
    );

    let mut cache = BindCache::default();
    let _first = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);

    // A declaration-changing edit: `Base` gains a brand-new method that
    // `Derived.n()` now also calls -- `Base`'s declared shape changed,
    // which must force a full rebind (of *every* file, including
    // `Derived.cls`, which itself has no textual change at all this
    // time) rather than the body-only fast path.
    let edited_base =
        "public virtual class Base { public Integer x; public void m() { } public void q() { } }";
    std::fs::write(dir.join("Base.cls"), edited_base).unwrap();

    let warm = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    let cold = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let warm_file = file_for_class(&warm, "Derived");
    let cold_file = file_for_class(&cold, "Derived");
    let warm_resolutions = name_expr_resolutions(&warm, warm_file);
    let cold_resolutions = name_expr_resolutions(&cold, cold_file);

    assert!(
        !warm_resolutions.is_empty(),
        "expected at least one NameExpr in Derived.cls"
    );
    assert_eq!(
        warm_resolutions, cold_resolutions,
        "warm incremental rebind after a declaration-changing edit in a *different* file \
         disagreed with a cold rebuild"
    );
}

/// `BindCache`'s directory walk (`discovery`) is only ever redone when
/// something suggests it's stale (see `BoundProgram::from_files_cached`'s
/// doc comment) -- so a file deleted between two calls has to
/// self-correct a different way: it just fails to read this call, and
/// that failure alone is what prunes its stale symbols out of the
/// persistent `SymbolTable`. This proves that actually happens, rather
/// than a deleted file's declarations lingering forever because the
/// cached walk still lists it.
#[test]
fn deleting_a_file_leaves_no_stale_symbols_even_with_a_stale_cached_walk() {
    let dir = write_fixture_dir(
        "incremental-delete",
        &[
            ("Alpha.cls", "public class Alpha { }"),
            ("Beta.cls", "public class Beta { }"),
        ],
    );

    let mut cache = BindCache::default();
    let first = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    assert!(
        first
            .symbols
            .iter()
            .any(|(_, s)| s.kind == SymbolKind::Class && s.name == "Beta"),
        "Beta should have been collected on the first call"
    );

    std::fs::remove_file(dir.join("Beta.cls")).unwrap();
    let second = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        !second
            .symbols
            .iter()
            .any(|(_, s)| s.kind == SymbolKind::Class && s.name == "Beta"),
        "Beta.cls was deleted -- its symbols should be gone, not stale"
    );
    assert!(
        second
            .symbols
            .iter()
            .any(|(_, s)| s.kind == SymbolKind::Class && s.name == "Alpha"),
        "Alpha.cls was untouched and should still be present"
    );
    assert_eq!(
        second.file_count(),
        1,
        "the deleted file should no longer be counted"
    );
}

/// The inverse case: a file created *and opened* after the first call
/// (its content arrives only via `overrides`, exactly like a real
/// editor's `didOpen` for a brand-new file) must still get bound on the
/// very next call, without needing a fresh `BindCache` -- proving the
/// `overrides`-vs-cached-walk staleness check actually triggers a
/// rediscovery rather than only ever trusting the first walk.
#[test]
fn a_newly_created_and_opened_file_is_picked_up_without_resetting_the_cache() {
    let dir = write_fixture_dir(
        "incremental-create",
        &[("Alpha.cls", "public class Alpha { }")],
    );

    let mut cache = BindCache::default();
    let _first = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);

    // Simulate the editor creating (and writing to disk) a brand-new
    // file, then sending `didOpen` for it -- its content shows up in
    // `overrides` for a path the cached walk has never heard of.
    let gamma_path = dir.join("Gamma.cls");
    std::fs::write(&gamma_path, "public class Gamma { }").unwrap();
    let mut overrides = HashMap::new();
    overrides.insert(gamma_path, "public class Gamma { }".to_string());

    let second = BoundProgram::from_files_cached(&dir, &overrides, &mut cache);
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        second
            .symbols
            .iter()
            .any(|(_, s)| s.kind == SymbolKind::Class && s.name == "Gamma"),
        "a newly created, already-open file should be bound on the very next call"
    );
    assert_eq!(second.file_count(), 2);
}
