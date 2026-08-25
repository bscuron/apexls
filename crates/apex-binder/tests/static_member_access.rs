//! Regression test for a real bug found via a user report (goto-definition
//! failing for `UTIL_UnitTestData_TEST.createMultipleTestContacts` in the
//! real NPSP corpus): `bind_name_expr` tried lexical scope, then member
//! lookup on the enclosing type, and gave up -- there was no fallback for
//! a bare name that's itself a project-local *type*, used as the receiver
//! of a static member/method access (`UtilClass.staticMethod(...)`,
//! `MyClass.MY_CONSTANT`) rather than as a value. Follows the same
//! fixture-on-disk pattern as `visibility_resolution.rs`.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{CallExpr, MethodCallExpr};
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

#[test]
fn a_static_method_call_qualified_by_another_classs_bare_name_resolves() {
    let dir = write_fixture_dir(
        "static-method-call",
        &[
            (
                "Util.cls",
                "public class Util { public static Integer helper() { return 1; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { Integer x = Util.helper(); } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let helper_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "helper")
        .map(|(id, _)| id)
        .expect("Util.helper should have been collected");

    let caller_file = file_for_class(&program, "Caller");
    let root = program.syntax(caller_file);
    let call = root
        .descendants()
        .find_map(MethodCallExpr::cast)
        .expect("Util.helper() should be a MethodCallExpr");
    let ptr = SyntaxPtr::new(caller_file, call.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(helper_id)),
        "a static method call qualified by another class's bare name should resolve"
    );
}

#[test]
fn an_instance_member_of_the_same_name_shadows_a_type_name_in_expression_position() {
    // If `Util` is *also* the name of a field/local on the enclosing
    // type, that member must win over the (much rarer, and real-Apex
    // would-be-a-compile-error-anyway) same-named type -- matching real
    // Apex/Java semantics, and confirming the type-name fallback is only
    // ever consulted *after* member lookup, not instead of it.
    let dir = write_fixture_dir(
        "static-method-call-shadowed",
        &[
            ("Util.cls", "public class Util { }"),
            (
                "Caller.cls",
                "public class Caller { \
                 public Integer Util; \
                 public void run() { Integer y = Util; } \
             }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let util_field_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "Util")
        .map(|(id, _)| id)
        .expect("Caller.Util field should have been collected");

    let caller_file = file_for_class(&program, "Caller");
    let root = program.syntax(caller_file);
    let name_expr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::NameExpr::cast)
        .expect("`Util` should be a NameExpr");
    let ptr = SyntaxPtr::new(caller_file, name_expr.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(util_field_id)),
        "a same-named instance field must shadow the type name"
    );
}

/// Regression test for a real user report against the NPSP corpus:
/// `RelationshipsTreeGrid_TEST.generateContactWithName` -- a `static`
/// method on the *outer* test class, called unqualified from a nested
/// `RelationshipsSelectorStub` class declared inside it -- was flagged
/// dead and its call site failed goto-definition, both because
/// `bind_call_expr`/`bind_name_expr` only ever checked the call site's
/// *immediate* enclosing type (the nested class itself) and its
/// `extends`/`implements` chain, never climbing out to the lexically
/// enclosing outer class the way `resolve_type_ref` already did for type
/// references. Real Apex resolves an unqualified name through the whole
/// enclosing-scope chain, not just the innermost level.
#[test]
fn an_unqualified_call_from_a_nested_class_resolves_a_static_method_on_the_outer_class() {
    let dir = write_fixture_dir(
        "nested-class-outer-static-call",
        &[(
            "Outer.cls",
            "public class Outer { \
                 static Integer helper() { return 1; } \
                 class Inner { \
                     void run() { Integer x = helper(); } \
                 } \
             }",
        )],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let helper_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "helper")
        .map(|(id, _)| id)
        .expect("Outer.helper should have been collected");

    let file = file_for_class(&program, "Outer");
    let root = program.syntax(file);
    let call = root
        .descendants()
        .find_map(CallExpr::cast)
        .expect("helper() should be a bare CallExpr");
    let ptr = SyntaxPtr::new(file, call.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(helper_id)),
        "an unqualified call from a nested class should resolve a static method on its outer class"
    );
}
