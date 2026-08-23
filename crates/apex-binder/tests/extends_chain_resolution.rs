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
    // `NameExpr` -- `Base` declares exactly one `m`, so arity-based
    // overload resolution alone (zero args, zero-param `m`) narrows the
    // inherited-through-`extends` candidate set to a single `Resolved`.
    let root = program.syntax(derived_file);
    let call_ptr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::CallExpr::cast)
        .map(|c| SyntaxPtr::new(c.syntax()))
        .expect("Derived.n() should contain one CallExpr (`m()`)");
    assert_eq!(
        program.refs.get(call_ptr).cloned(),
        Some(Resolution::Resolved(base_method_id))
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

/// Every `CallExpr`/`MethodCallExpr` in `file`, in source order,
/// alongside its recorded `Resolution` -- used below to inspect
/// specific overloaded calls without needing to know their exact byte
/// offsets.
fn call_resolutions(program: &BoundProgram, file: apex_binder::FileId) -> Vec<Option<Resolution>> {
    let root = program.syntax(file);
    root.descendants()
        .filter_map(apex_syntax::ast::expr::CallExpr::cast)
        .map(|c| program.refs.get(SyntaxPtr::new(c.syntax())).cloned())
        .collect()
}

#[test]
fn overload_resolution_narrows_by_arity_then_by_known_argument_types() {
    let dir = write_fixture_dir(
        "overloads",
        &[
            ("Widget.cls", "public class Widget { }"),
            ("Gadget.cls", "public class Gadget { }"),
            (
                "Toolbox.cls",
                "public class Toolbox { \
                 public void handle(Widget w) { } \
                 public void handle(Widget w, Widget w2) { } \
                 public void handle(Gadget g) { } \
                 public void run() { \
                     Widget w = new Widget(); \
                     handle(w); \
                     handle(w, w); \
                     handle(1); \
                 } \
             }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let toolbox = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Toolbox")
        .map(|(id, _)| id)
        .expect("Toolbox should have been collected");
    let handle_overloads: Vec<apex_binder::SymbolId> = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "handle")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        handle_overloads.len(),
        3,
        "expected all three `handle` overloads to be collected"
    );

    let one_widget = handle_overloads
        .iter()
        .copied()
        .find(|&id| {
            let params = program.symbols.params(id);
            params.len() == 1
                && program.symbols.get(params[0]).type_name.as_deref() == Some("Widget")
        })
        .expect("handle(Widget) should exist");
    let two_widgets = handle_overloads
        .iter()
        .copied()
        .find(|&id| program.symbols.params(id).len() == 2)
        .expect("handle(Widget, Widget) should exist");

    let toolbox_file = program.symbols.get(toolbox).file;
    let calls = call_resolutions(&program, toolbox_file);
    assert_eq!(calls.len(), 3, "expected three CallExprs in Toolbox.run()");

    // `handle(w)`: two arity-1 candidates (`handle(Widget)`,
    // `handle(Gadget)`); `w`'s known type (`Widget`) rules out the
    // `Gadget` overload, leaving exactly one.
    assert_eq!(calls[0], Some(Resolution::Resolved(one_widget)));

    // `handle(w, w)`: only one candidate has arity 2 at all, so arity
    // alone resolves it -- no type check needed.
    assert_eq!(calls[1], Some(Resolution::Resolved(two_widgets)));

    // `handle(1)`: arity narrows to the same two arity-1 candidates as
    // `handle(w)`, but an integer literal's type isn't inferred (v1
    // tracks no primitive/system types), so neither can be ruled out --
    // stays genuinely ambiguous.
    let Some(Resolution::Candidates(remaining)) = &calls[2] else {
        panic!("expected handle(1) to stay Candidates, got {:?}", calls[2]);
    };
    assert_eq!(remaining.len(), 2);
}
