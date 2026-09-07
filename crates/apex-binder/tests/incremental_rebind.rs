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

/// Regression test for a real gap found reviewing the `stdlib-interfaces`
/// Wayfinder map's implementation: `SymbolTable::rebuild_indices` carries
/// `stdlib_implements` forward unchanged whenever `resolve_inheritance`
/// itself is skipped, so a type that *used to* implement a recognized
/// stdlib interface but was edited to no longer must still get a fresh,
/// empty entry written on the rebind that *does* rerun `resolve_inheritance`
/// -- otherwise the stale, no-longer-true entry survives forever in that
/// session, permanently (and wrongly) exempting `execute`/`start`/`finish`
/// from dead-code/visibility-narrowing detection even after the class no
/// longer implements `Database.Batchable` at all.
#[test]
fn stdlib_implements_does_not_survive_an_edit_that_removes_the_stdlib_interface_clause() {
    let dir = write_fixture_dir(
        "incremental-stdlib-implements-cleared",
        &[(
            "Foo.cls",
            "public class Foo implements Database.Batchable<SObject> {\n    \
                 public Database.QueryLocator start(Database.BatchableContext bc) { return null; }\n    \
                 public void execute(Database.BatchableContext bc, List<SObject> records) { }\n    \
                 public void finish(Database.BatchableContext bc) { }\n\
             }\n",
        )],
    );

    let mut cache = BindCache::default();
    let first = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    let foo = first.symbols.top_level("Foo").expect("Foo should be declared");
    assert!(
        !first.symbols.stdlib_implements(foo).is_empty(),
        "Foo implements Database.Batchable, expected a non-empty stdlib_implements entry"
    );

    // A declaration-changing edit (every parameter's own type changed,
    // which flips `declarations_changed`) that swaps the `implements`
    // clause from a stdlib interface to a project-local one (`Foo.Bar`,
    // resolving fine) -- Foo still has a real `implements` clause (still
    // present in `raw_extends`, unlike dropping the clause entirely,
    // which is a separate, pre-existing gap: a type with *zero* remaining
    // supertype clauses drops out of `raw_extends` altogether, so this
    // pass's per-type loop never revisits it at all -- the same
    // limitation `inherited_chain` itself already has, out of scope
    // here), so `raw_inheritance_inputs` changes and `resolve_inheritance`
    // actually reruns and revisits Foo.
    let edited = "public class Foo implements Foo.Bar {\n    \
             public Object start(Object bc) { return null; }\n    \
             public void execute(Object bc, List<Object> records) { }\n    \
             public void finish(Object bc) { }\n    \
             public interface Bar { }\n\
         }\n";
    std::fs::write(dir.join("Foo.cls"), edited).unwrap();

    let warm = BoundProgram::from_files_cached(&dir, &HashMap::new(), &mut cache);
    std::fs::remove_dir_all(&dir).ok();

    let foo_warm = warm.symbols.top_level("Foo").expect("Foo should still be declared");
    assert!(
        warm.symbols.stdlib_implements(foo_warm).is_empty(),
        "Foo no longer implements Database.Batchable after the edit -- its stale \
         stdlib_implements entry must not survive the incremental rebind"
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

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// Real-corpus-scale version of
/// [`declaration_changing_edit_via_incremental_cache_matches_a_cold_rebuild`]
/// above, targeting the exact bug class ticket 30/31 (Wayfinder
/// `apex-diagnostics` map, Stage 3 of the salsa migration) found and
/// fixed while implementing: `crate::inherit::resolve_inheritance` gained
/// a narrower trigger than `SymbolTable::rebuild_indices`'s own
/// `declarations_changed` gate, and the first version of that change let
/// `rebuild_indices` wipe already-correct `inherited_chain`/`direct_super`
/// data whenever `resolve_inheritance` was (correctly) skipped that
/// round. The small synthetic fixture above caught it first; this
/// re-runs the same shape at real scale (a real NPSP class's chain, a
/// real NPSP file edited) as this stage's own corpus validation, per
/// this map's Notes.
#[test]
fn a_real_npsp_classs_inherited_chain_survives_an_unrelated_declaration_change_in_another_file() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? \
         (git submodule update --init --recursive)"
    );

    let mut warm_cache = BindCache::default();
    let baseline = BoundProgram::from_files_cached(&root, &HashMap::new(), &mut warm_cache);

    // A real class with a non-empty `inherited_chain`, plus a *different*
    // class (in a different file) to edit -- ancestor names are compared
    // by name, not `SymbolId`, since the two further builds below number
    // files independently and could legitimately disagree on raw ids.
    let (probe_name, probe_file, ancestor_names_baseline) = baseline
        .symbols
        .iter()
        .find_map(|(id, s)| {
            let chain = baseline.symbols.inherited_chain(id);
            (s.kind == SymbolKind::Class && !chain.is_empty()).then(|| {
                let names: Vec<String> =
                    chain.iter().map(|&a| baseline.symbols.get(a).name.to_string()).collect();
                (s.name.to_string(), s.file, names)
            })
        })
        .expect("expected at least one real NPSP class with a non-empty inherited_chain");

    let edited_file = baseline
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.file != probe_file)
        .map(|(_, s)| s.file)
        .expect("expected at least one other real NPSP class in a different file");
    let edited_path = baseline.file_path(edited_file).to_path_buf();

    // A brand-new, declaration-changing method appended to that other
    // class's body -- forces `declarations_changed` (so `rebuild_indices`
    // runs) without touching any type's own declared name or
    // `extends`/`implements` clause anywhere, so the narrower
    // `resolve_inheritance` trigger should correctly decide it doesn't
    // need to rerun, and `probe_name`'s already-correct chain must
    // survive that skip intact.
    let original_text = std::fs::read_to_string(&edited_path).unwrap();
    let edited_text = original_text.replacen('{', "{ public void __stage3_probe_zzz() { } ", 1);
    let mut overrides = HashMap::new();
    overrides.insert(edited_path, edited_text);

    let warm = BoundProgram::from_files_cached(&root, &overrides, &mut warm_cache);
    let cold = BoundProgram::from_files_with_overrides(&root, &overrides);

    let ancestor_names_of = |program: &BoundProgram| -> Vec<String> {
        let id = program
            .symbols
            .iter()
            .find(|(_, s)| s.kind == SymbolKind::Class && s.name == probe_name)
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("{probe_name} should still be collected"));
        program
            .symbols
            .inherited_chain(id)
            .iter()
            .map(|&a| program.symbols.get(a).name.to_string())
            .collect()
    };

    assert_eq!(
        ancestor_names_of(&warm),
        ancestor_names_baseline,
        "the probe class's inherited_chain changed across an edit to an unrelated file -- \
         it shouldn't have"
    );
    assert_eq!(
        ancestor_names_of(&warm),
        ancestor_names_of(&cold),
        "warm incremental rebind's inherited_chain for {probe_name} disagreed with a cold \
         rebuild after an unrelated declaration change in a different file"
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
