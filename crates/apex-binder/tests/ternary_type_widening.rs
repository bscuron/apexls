//! Fixtures verifying a ternary's two branches widen to a real common
//! type (`crate::conversions::widen`, wired into `crate::resolve`'s
//! `Expr::Ternary` arm) instead of the old behavior of silently
//! preferring the `then` branch's type regardless of whether the
//! branches actually relate. Real Apex ternary semantics were verified
//! against a live connected org before this was built -- see
//! `crate::conversions::widen`'s own doc comment for the specific
//! evidence, including the surprising finding that two sibling classes
//! sharing only a common *ancestor* (not a direct relationship) don't
//! widen at all in real Apex.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
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

/// Whether `target`'s own subtree contains a `TernaryExpr` -- true both
/// when `target` *is* one directly and when it's a `ParenExpr` wrapping
/// one (`(cond ? a : b).member`, required by Apex's own grammar for
/// postfix access on a ternary -- `crate::resolve`'s `Expr::Paren` arm
/// transparently passes the inner expression's `Ty` through, so the
/// wrapping paren doesn't affect what actually resolves, only how this
/// helper has to look for the fixture node).
fn wraps_a_ternary(target: &apex_syntax::ast::Expr) -> bool {
    target
        .syntax()
        .descendants()
        .any(|d| apex_syntax::ast::expr::TernaryExpr::can_cast(d.kind()))
}

/// A `FieldExpr` chained *directly* off a `TernaryExpr` (no intermediate
/// declared-type variable in between) -- the only way to observe the
/// ternary's own inferred type, as opposed to a variable's declared type.
fn field_expr_over_ternary(program: &BoundProgram, file: apex_binder::FileId) -> SyntaxPtr {
    let root = program.syntax(file);
    root.descendants()
        .find_map(apex_syntax::ast::expr::FieldExpr::cast)
        .filter(|f| f.target().is_some_and(|t| wraps_a_ternary(&t)))
        .map(|f| SyntaxPtr::new(file, f.syntax()))
        .expect("a FieldExpr directly over a TernaryExpr should exist")
}

#[test]
fn a_ternary_between_a_base_and_a_direct_subtype_widens_to_the_base() {
    // `Dog` directly `extends Animal` -- real Apex confirmed (via a live
    // org probe) to widen a ternary between them to the base type,
    // exactly the same directional rule `crate::conversions::type_compatible`
    // already applies to a plain assignment.
    let dir = write_fixture_dir(
        "ternary-base-subtype",
        &[
            ("Animal.cls", "public class Animal { public String name; }"),
            ("Dog.cls", "public class Dog extends Animal {}"),
            (
                "Widget.cls",
                "public class Widget { \
                     public void run() { \
                         Boolean flag = true; \
                         Animal a = new Animal(); \
                         Dog d = new Dog(); \
                         String n = (flag ? a : d).name; \
                     } \
                 }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let name_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "name")
        .map(|(id, _)| id)
        .expect("Animal.name should have been collected");

    let widget_file = file_for_class(&program, "Widget");
    let field_ptr = field_expr_over_ternary(&program, widget_file);
    assert_eq!(
        program.resolution(field_ptr).cloned(),
        Some(Resolution::Resolved(name_id)),
        "(flag ? a : d).name should resolve through the widened Animal type"
    );
}

#[test]
fn a_ternary_between_two_unrelated_sibling_classes_stays_unresolved() {
    // `Dog`/`Cat` both `extends Animal` but have no *direct* relationship
    // to each other -- real Apex refuses to compile this ternary at all
    // (`Incompatible types in ternary operator: Cat, Dog`, confirmed live)
    // rather than widening to their shared `Animal` ancestor. `apexls`
    // doesn't reject on type errors, but the ternary's inferred type must
    // honestly stay unknown here, not silently guess `Dog` (the old
    // `then`-preferring behavior) or `Animal` (a common-ancestor guess
    // real Apex itself doesn't make).
    let dir = write_fixture_dir(
        "ternary-unrelated-siblings",
        &[
            ("Animal.cls", "public class Animal { public String name; }"),
            ("Dog.cls", "public class Dog extends Animal { public String breed; }"),
            ("Cat.cls", "public class Cat extends Animal { public String coatColor; }"),
            (
                "Widget.cls",
                "public class Widget { \
                     public void run() { \
                         Boolean flag = true; \
                         Dog d = new Dog(); \
                         Cat c = new Cat(); \
                         String n = (flag ? d : c).name; \
                     } \
                 }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_file = file_for_class(&program, "Widget");
    let field_ptr = field_expr_over_ternary(&program, widget_file);
    assert_eq!(
        program.resolution(field_ptr).cloned(),
        Some(Resolution::Unresolved),
        "(flag ? d : c).name must not guess a type real Apex itself refuses to widen to"
    );
}

#[test]
fn a_ternary_between_a_real_object_type_and_sobject_widens_to_sobject() {
    // `getSObjectType` is chained *directly* off the ternary (no
    // intermediate declared-type variable, same reasoning as
    // `field_expr_over_ternary` above) -- proving this resolves as a real
    // stdlib member call proves the ternary's own inferred type is a
    // real `SObject`-shaped `Ty`, not that some other declaration's type
    // happened to already be `SObject`.
    let dir = write_fixture_dir(
        "ternary-sobject-widen",
        &[(
            "Widget.cls",
            "public class Widget { \
                 public void run() { \
                     Boolean flag = true; \
                     Account acc = new Account(); \
                     SObject generic = acc; \
                     Schema.SObjectType t = (flag ? acc : generic).getSObjectType(); \
                 } \
             }",
        )],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_file = file_for_class(&program, "Widget");
    let root = program.syntax(widget_file);
    let call_ptr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::MethodCallExpr::cast)
        .filter(|c| c.target().is_some_and(|t| wraps_a_ternary(&t)))
        .map(|c| SyntaxPtr::new(widget_file, c.syntax()))
        .expect("(flag ? acc : generic).getSObjectType() should be a MethodCallExpr over a TernaryExpr");
    assert!(
        matches!(program.resolution(call_ptr), Some(Resolution::StdlibMember(_))),
        "(flag ? acc : generic).getSObjectType() should resolve through the widened SObject type"
    );
}
