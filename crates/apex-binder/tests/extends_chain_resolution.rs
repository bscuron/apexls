//! Hand-written, small multi-file fixtures verifying inherited-member
//! lookup correctness (positive + negative cases) -- the whole-corpus
//! smoke tests prove resolution doesn't crash/regress in aggregate, but
//! can't prove *which specific* symbol a reference resolved to is the
//! *right* one; that needs a scenario small enough to check by hand.
//! Follows `apex-metadata`'s own fixture-on-disk pattern (a temp dir
//! under the OS temp root, cleaned up after the assertions run).

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::NameExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

/// Every `NameExpr` in `file`'s tree whose plain-identifier token equals
/// `name`, in source order, alongside its recorded `Resolution` (`None`
/// if the walk never resolved that particular occurrence, which itself
/// would be a bug for these fixtures -- every `x`/`m` reference here is
/// inside a real method body Pass 2 should have walked).
fn name_expr_resolutions(
    program: &BoundProgram,
    file: apex_binder::FileId,
    name: &str,
) -> Vec<Option<Resolution>> {
    let root = program.syntax(file);
    root.descendants()
        .filter_map(NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == name))
        .map(|n| program.refs.get(SyntaxPtr::new(n.syntax())).cloned())
        .collect()
}

#[test]
fn inherited_field_and_method_resolve_across_extends_but_not_between_unrelated_siblings() {
    let dir = write_fixture_dir(
        "extends",
        &[
            (
                "Base.cls",
                "public virtual class Base { public Integer x; public void m() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public void n() { x = 1; m(); } }",
            ),
            (
                "Sibling.cls",
                "public class Sibling { public void p() { Integer y = x; } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let (base_field_id, _) = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "x")
        .expect("Base.x should have been collected");
    let base_method_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "m")
        .map(|(id, _)| id)
        .expect("Base.m should have been collected");

    let derived_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Derived")
        .map(|(_, s)| s.file)
        .expect("Derived should have been collected");
    let sibling_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Sibling")
        .map(|(_, s)| s.file)
        .expect("Sibling should have been collected");

    // Positive: `Derived.n()`'s `x = 1;` resolves through `extends` to
    // `Base.x` -- exactly one `x` in that file's whole tree.
    let derived_x = name_expr_resolutions(&program, derived_file, "x");
    assert_eq!(
        derived_x.len(),
        1,
        "expected exactly one `x` NameExpr in Derived.cls"
    );
    assert_eq!(derived_x[0], Some(Resolution::Resolved(base_field_id)));

    // `m()` is an unqualified call, so it's a `CallExpr`, not a
    // `NameExpr` -- resolved as a (single-member) `Candidates` set per
    // v1's never-pick-a-single-overload rule, keyed by the whole
    // `CallExpr` node rather than a bare name token.
    let root = program.syntax(derived_file);
    let call_ptr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::CallExpr::cast)
        .map(|c| SyntaxPtr::new(c.syntax()))
        .expect("Derived.n() should contain one CallExpr (`m()`)");
    assert_eq!(
        program.refs.get(call_ptr).cloned(),
        Some(Resolution::Candidates(vec![base_method_id]))
    );

    // Negative: `Sibling.p()`'s `x` has no relation to `Base` at all --
    // must not resolve to `Base.x` (or anything else).
    let sibling_x = name_expr_resolutions(&program, sibling_file, "x");
    assert_eq!(
        sibling_x.len(),
        1,
        "expected exactly one `x` NameExpr in Sibling.cls"
    );
    assert_eq!(sibling_x[0], Some(Resolution::Unresolved));
}
