//! Regression test for ticket 39's 2d: `ApexPages.Severity` (and its
//! `CONFIRM`/`ERROR`/`FATAL`/`INFO`/`WARNING` constants) was entirely
//! missing from the stdlib snapshot, even though `ApexPages`'s own scraped
//! methods (`hasMessages(ApexPages.Severity)`) and `ApexPages.Message`'s own
//! constructor/`getSeverity()` reference the type by name. The scraper
//! captured every method that *references* the nested enum without ever
//! emitting the enum's own definition page -- the same "referenced but
//! never defined" gap as `Schema.SObjectTypeFields`/`SObjectTypeFieldSets`
//! (ticket 39's 2b). Real, common NPSP idiom (e.g.
//! `ADDR_CopyAddrHHObjBTN_CTRL.cls:152`): `ApexPages.Severity.ERROR`, used
//! to build an `ApexPages.Message`.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::FieldExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Foo.cls"), src).unwrap();
    dir
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

/// `ApexPages.Severity.ERROR` resolves as a real static member of the
/// `Severity` enum, not `Unresolved`.
#[test]
fn apexpages_severity_error_resolves_as_a_real_stdlib_member() {
    let dir = write_fixture_dir(
        "apexpages-severity-error",
        "public class Foo { public void run() { ApexPages.Severity s = ApexPages.Severity.ERROR; } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "ERROR") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "Severity"),
        other => panic!("expected ApexPages.Severity.ERROR to resolve as a real Severity member, got {other:?}"),
    }
}

/// Every documented constant resolves, not just `ERROR`.
#[test]
fn every_apexpages_severity_constant_resolves() {
    for constant in ["CONFIRM", "ERROR", "FATAL", "INFO", "WARNING"] {
        let dir = write_fixture_dir(
            &format!("apexpages-severity-{constant}"),
            &format!(
                "public class Foo {{ public void run() {{ ApexPages.Severity s = ApexPages.Severity.{constant}; }} }}"
            ),
        );
        let program = BoundProgram::from_files(&dir);
        std::fs::remove_dir_all(&dir).ok();

        match field_expr_resolution(&program, constant) {
            Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "Severity"),
            other => panic!("expected ApexPages.Severity.{constant} to resolve as a real Severity member, got {other:?}"),
        }
    }
}

/// A real NPSP idiom: passing the constant straight into `ApexPages.Message`'s
/// constructor, then chaining `.getSeverity()` off the constructed message.
#[test]
fn severity_constant_flows_into_an_apexpages_message_constructor() {
    let dir = write_fixture_dir(
        "apexpages-severity-message-ctor",
        "public class Foo { public void run() { ApexPages.Message m = new ApexPages.Message(ApexPages.Severity.ERROR, 'summary'); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "ERROR") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "Severity"),
        other => panic!("expected ApexPages.Severity.ERROR to resolve as a real Severity member, got {other:?}"),
    }
}
