//! Hand-written fixtures verifying `SymbolTable::is_visible_from`
//! (`BACKLOG.md` §4's cross-file visibility enforcement): a `private`
//! member of an unrelated class must never appear as a resolution
//! candidate, while `private` access within the same top-level type
//! (including nested classes) and `protected` access from a subclass
//! must keep working exactly as before. Follows the same fixture-on-disk
//! pattern as `extends_chain_resolution.rs`.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{MethodCallExpr, NameExpr};
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

fn name_expr_resolution(
    program: &BoundProgram,
    file: apex_binder::FileId,
    name: &str,
) -> Option<Resolution> {
    let root = program.syntax(file);
    let matches: Vec<Option<Resolution>> = root
        .descendants()
        .filter_map(NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == name))
        .map(|n| {
            program
                .resolution(SyntaxPtr::new(file, n.syntax()))
                .cloned()
        })
        .collect();
    assert_eq!(matches.len(), 1, "expected exactly one `{name}` NameExpr");
    matches.into_iter().next().unwrap()
}

#[test]
fn a_private_field_is_invisible_from_an_unrelated_class() {
    let dir = write_fixture_dir(
        "visibility-private",
        &[
            ("Base.cls", "public class Base { private Integer secret; }"),
            (
                "Outsider.cls",
                "public class Outsider { public void p() { Integer y = secret; } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let outsider_file = file_for_class(&program, "Outsider");
    assert_eq!(
        name_expr_resolution(&program, outsider_file, "secret"),
        Some(Resolution::Unresolved),
        "a private field of an unrelated class must never resolve"
    );
}

#[test]
fn a_private_field_stays_visible_from_within_its_own_declaring_class() {
    // Every prior fixture test already relies on this implicitly (a
    // class's own methods can always see its own private members) --
    // this is an explicit regression guard specifically for
    // `is_visible_from`'s same-top-level-type check, isolated from
    // inheritance/overload narrowing so a future break here can't hide
    // behind those other mechanisms.
    let dir = write_fixture_dir(
        "visibility-same-class",
        &[(
            "Widget.cls",
            "public class Widget { \
             private Integer secret; \
             public void p() { Integer y = secret; } \
         }",
        )],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let secret_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "secret")
        .map(|(id, _)| id)
        .expect("Widget.secret should have been collected");
    let widget_file = file_for_class(&program, "Widget");
    assert_eq!(
        name_expr_resolution(&program, widget_file, "secret"),
        Some(Resolution::Resolved(secret_id)),
        "a class must always be able to see its own private members"
    );
}

#[test]
fn a_protected_field_is_visible_from_a_subclass_but_not_from_an_unrelated_class() {
    let dir = write_fixture_dir(
        "visibility-protected",
        &[
            (
                "Base.cls",
                "public virtual class Base { protected Integer guarded; }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public void p() { Integer y = guarded; } }",
            ),
            (
                "Outsider.cls",
                "public class Outsider { public void p() { Integer y = guarded; } }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let guarded_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "guarded")
        .map(|(id, _)| id)
        .expect("Base.guarded should have been collected");

    let derived_file = file_for_class(&program, "Derived");
    assert_eq!(
        name_expr_resolution(&program, derived_file, "guarded"),
        Some(Resolution::Resolved(guarded_id)),
        "a protected field must be visible from a subclass"
    );

    let outsider_file = file_for_class(&program, "Outsider");
    assert_eq!(
        name_expr_resolution(&program, outsider_file, "guarded"),
        Some(Resolution::Unresolved),
        "a protected field must not be visible from an unrelated class"
    );
}

/// Regression test for a real bug found via a user report (goto-definition
/// failing on `bindingResolver.bySharingMode(...)` in NPSP's
/// `Application.cls`, where `bindingResolver`'s declared type is the
/// interface `fflib_IAppBindingResolver`): an Apex interface method
/// declaration carries no access modifier at all -- every interface
/// member is implicitly public -- but `ModifierSet`'s modifier-less
/// default is `Private` (correct for a *class* member, Apex's real
/// default there). Before the fix, an interface method's `Symbol` ended
/// up `Private`, so `is_visible_from` rejected every cross-class call to
/// it, same as this test's `Impl.cls`/`Caller.cls` shape.
#[test]
fn an_interface_methods_implicit_public_visibility_is_reachable_across_classes() {
    let dir = write_fixture_dir(
        "visibility-interface-method",
        &[
            (
                "Greeter.cls",
                "public interface Greeter { String greet(); }",
            ),
            (
                "Impl.cls",
                "public class Impl implements Greeter { public String greet() { return 'hi'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 private static Greeter g = new Impl(); \
                 public void run() { String s = g.greet(); } \
             }",
            ),
        ],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // `Impl.greet` (a concrete override) shares the same name -- looked
    // up by container (`Greeter`'s own `SymbolId`), not just by name, so
    // this test can't pass by accident against the wrong declaration.
    let greeter_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Interface && s.name == "Greeter")
        .map(|(id, _)| id)
        .expect("Greeter should have been collected");
    let greet_id = program
        .symbols
        .iter()
        .find(|(_, s)| {
            s.kind == SymbolKind::Method && s.name == "greet" && s.container == Some(greeter_id)
        })
        .map(|(id, _)| id)
        .expect("Greeter.greet should have been collected");

    let caller_file = file_for_class(&program, "Caller");
    let root = program.syntax(caller_file);
    let call = root
        .descendants()
        .find_map(MethodCallExpr::cast)
        .expect("g.greet() should be a MethodCallExpr");
    let ptr = SyntaxPtr::new(caller_file, call.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(greet_id)),
        "an interface method with no explicit modifier is implicitly \
         public and must resolve from an unrelated class"
    );
}
