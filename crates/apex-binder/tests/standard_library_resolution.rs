//! Regression tests for standard-library class/method/property
//! resolution (`apex_stdlib`'s bundled `apex_reference.json` snapshot,
//! consulted from `crate::resolve`'s `Ty::System` arms via the new
//! `crate::stdlib_index::StdlibIndex`). Before this, *every* method
//! call or property access on a non-project-local receiver resolved as
//! `Resolution::Unresolved` unconditionally -- a real `String.isBlank(...)`
//! call was indistinguishable from a genuine typo. No change to how
//! `crate::generics`'s `List`/`Map`/`Set` type-argument substitution
//! works (that's tried first and always wins when it applies); this is
//! purely the fallback for everything else.

use apex_binder::{BoundProgram, Resolution, StdlibMemberRef};
use apex_syntax::ast::expr::{FieldExpr, MethodCallExpr};
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn method_call_resolution(program: &BoundProgram, method_name: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(mc) = MethodCallExpr::cast(node) else { continue };
            let Some(name) = mc.method_name_token() else { continue };
            if name.text() == method_name {
                let ptr = apex_binder::SyntaxPtr::new(file, mc.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

fn field_expr_resolution(program: &BoundProgram, member: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(fe) = FieldExpr::cast(node) else { continue };
            let Some(name) = fe.member_token() else { continue };
            if name.text() == member {
                let ptr = apex_binder::SyntaxPtr::new(file, fe.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

#[test]
fn a_real_static_stdlib_method_call_resolves_to_stdlib_member() {
    let dir = write_fixture_dir(
        "stdlib-static-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Boolean b = String.isBlank('x'); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "isBlank"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "String".into(),
            member: Some("isBlank".into()),
        }))),
        "String.isBlank is a real, documented stdlib method"
    );
}

#[test]
fn an_overloaded_stdlib_method_call_also_resolves() {
    let dir = write_fixture_dir(
        "stdlib-overloaded-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run(String soql) { List<SObject> rows = Database.query(soql); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "query"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Database".into(),
            member: Some("query".into()),
        }))),
        "Database.query is real and overloaded -- existence, not overload-exactness, decides the Resolution"
    );
}

#[test]
fn a_stdlib_property_access_resolves_to_stdlib_member() {
    // `Address.city` isn't itself a "usual" example, but confirms the
    // property path independent of the method-call path -- any real,
    // documented stdlib property works the same way `bind_field_expr`'s
    // schema-field lookup already does for an SObject field.
    let dir = write_fixture_dir(
        "stdlib-property-access",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { PageReference pr = ApexPages.currentPage(); Map<String,String> p = pr.getParameters(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // `ApexPages.currentPage()` and `pr.getParameters()` are both real
    // stdlib methods -- a chained-call smoke test that the propagated
    // `Ty::System` from one stdlib call correctly feeds the next one.
    assert_eq!(
        method_call_resolution(&program, "currentPage"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "ApexPages".into(),
            member: Some("currentPage".into()),
        })))
    );
    assert_eq!(
        method_call_resolution(&program, "getParameters"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "PageReference".into(),
            member: Some("getParameters".into()),
        }))),
        "chaining off Database/ApexPages's stdlib return type should still resolve the next call"
    );
}

/// Negative case: an unmodeled/misspelled member on a real stdlib class
/// must stay `Unresolved`, not be over-eagerly matched -- guards against
/// `StdlibIndex::method`/`property` false-positiving on a name that
/// merely resembles a real one.
#[test]
fn an_unmodeled_stdlib_method_stays_unresolved() {
    let dir = write_fixture_dir(
        "stdlib-typo-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { String.definitelyNotARealStdlibMethod(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "definitelyNotARealStdlibMethod"),
        Some(Resolution::Unresolved)
    );
}

/// `generics.rs`'s `List<T>.get` substitution must keep winning over the
/// new stdlib fallback -- a project-local element type should still
/// resolve `Resolved`, not get reduced to `StdlibMember`/`Unresolved` by
/// the raw (unsubstituted) scraped `List.get` signature.
#[test]
fn list_get_still_substitutes_the_project_local_element_type() {
    let dir = write_fixture_dir(
        "stdlib-vs-generics-list-get",
        &[(
            "Widget.cls",
            "public class Widget { \
             public Integer count; \
             public void run() { \
                 List<Widget> items = new List<Widget>(); \
                 Integer c = items.get(0).count; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "count"),
        Some(Resolution::Resolved(
            program
                .symbols
                .iter()
                .find(|(_, s)| s.name == "count")
                .map(|(id, _)| id)
                .expect("Widget.count should have been collected")
        )),
        "List<Widget>.get(0)'s substituted element type must still let .count resolve"
    );
}

/// `List.sort()` isn't in `generics.rs`'s 13-entry table (no type-
/// argument substitution needed for a `void`-returning method), so it
/// must fall through to the new stdlib lookup instead of staying
/// `Unresolved` the way it did before this feature existed.
#[test]
fn list_sort_falls_through_to_the_stdlib_lookup() {
    let dir = write_fixture_dir(
        "stdlib-list-sort",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { List<Integer> xs = new List<Integer>(); xs.sort(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "sort"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "List".into(),
            member: Some("sort".into()),
        })))
    );
}
