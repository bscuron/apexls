//! Regression test for a real bug found via a user report: clicking
//! directly on `super` (or `this`) in `super.method()`/`this.field` was
//! taking goto-definition to the *member* being accessed instead of the
//! superclass/enclosing class -- `Expr::This`/`Expr::Super` computed a
//! type for chaining purposes but never recorded a `Resolution` of their
//! own, so `BoundProgram::resolution_at`'s ancestor climb fell through
//! past them to the nearest enclosing node that *did* have one (the
//! `FieldExpr`/`MethodCallExpr`). Follows `extends_chain_resolution.rs`'s
//! fixture-on-disk pattern.

use apex_binder::{BoundProgram, Resolution, SymbolKind};
use apex_syntax::ast::expr::{Expr, MethodCallExpr};
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

/// A byte offset strictly inside the single `super`/`this` keyword token
/// found as a `MethodCallExpr`'s target -- the exact position a click on
/// the bare keyword itself (not the member after the dot) lands on.
fn keyword_target_mid_offset(program: &BoundProgram, file: apex_binder::FileId) -> rowan::TextSize {
    let root = program.syntax(file);
    let target = root
        .descendants()
        .find_map(MethodCallExpr::cast)
        .and_then(|mc| mc.target())
        .expect("expected a MethodCallExpr with a this/super target");
    let range = match target {
        Expr::This(t) => t.syntax().text_range(),
        Expr::Super(s) => s.syntax().text_range(),
        other => panic!("expected This/Super target, got {other:?}"),
    };
    range.start() + rowan::TextSize::from(1)
}

#[test]
fn clicking_super_in_a_qualified_call_resolves_to_the_superclass_not_the_method() {
    let dir = write_fixture_dir(
        "super-click",
        &[
            (
                "Base.cls",
                "public virtual class Base { public virtual void greet() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public override void greet() { super.greet(); } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Base")
        .map(|(id, _)| id)
        .expect("Base should have been collected");

    let derived_file = file_for_class(&program, "Derived");
    let offset = keyword_target_mid_offset(&program, derived_file);

    assert_eq!(
        program.resolution_at(derived_file, offset).cloned(),
        Some(Resolution::Resolved(base_id)),
        "clicking on `super` itself must resolve to the superclass, not the method it qualifies"
    );
}

#[test]
fn clicking_this_in_a_qualified_call_resolves_to_the_enclosing_class_not_the_method() {
    let dir = write_fixture_dir(
        "this-click",
        &[(
            "Widget.cls",
            "public class Widget { \
             public void run() { this.helper(); } \
             public void helper() { } \
         }",
        )],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Widget")
        .map(|(id, _)| id)
        .expect("Widget should have been collected");

    let widget_file = file_for_class(&program, "Widget");
    let offset = keyword_target_mid_offset(&program, widget_file);

    assert_eq!(
        program.resolution_at(widget_file, offset).cloned(),
        Some(Resolution::Resolved(widget_id)),
        "clicking on `this` itself must resolve to the enclosing class, not the method it qualifies"
    );
}
