//! Regression tests for Apex's `switch on <expr> { when <Type> <binding>
//! { ... } }` type-pattern form -- a real, common Apex construct (e.g.
//! `switch on trigger.new[0] { when Account acc { ... } }`) that, before
//! this, had never been exercised by any binder-level test: every other
//! fixture in this suite that used `switch` only ever used the
//! value-pattern form (`when 1, 2 { ... }`), which has no binding
//! variable at all. `crate::resolve::BodyBinder::bind_stmt`'s
//! `Stmt::Switch` arm resolves the `when` clause's own type reference,
//! declares the binding as a local of that type, and scopes it to the
//! `when` clause's own body.

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef, SymbolKind};
use apex_syntax::ast::expr::{FieldExpr, NameExpr};
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
fn a_switch_type_binding_declares_a_local_of_that_type_scoped_to_its_when_clause() {
    const SRC: &str = "public class Foo { \
        public void run(Object x) { \
            switch on x { \
                when Account acc { \
                    String n = acc.Name; \
                } \
                when else { \
                } \
            } \
        } \
    }";
    let dir = write_fixture_dir("switch-type-binding", &[("Foo.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");

    // The binding itself (`acc`) must have been declared as a real
    // `SwitchBindingVar` local of type `Account`.
    let acc_sym = program
        .symbols
        .iter()
        .find(|(_, s)| s.file == file && s.kind == SymbolKind::SwitchBindingVar && s.name == "acc")
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("expected a SwitchBindingVar named `acc`"));
    assert_eq!(
        program.symbols.get(acc_sym).type_name.as_deref(),
        Some("Account"),
        "the binding should carry its when-clause's declared type"
    );

    // The binding must actually be visible (scoped) inside its own
    // when-clause body: the bare `acc` read on the RHS of `String n =
    // acc.Name` should resolve to that same local, not `Unresolved`.
    let root = program.syntax(file);
    let acc_read = root
        .descendants()
        .filter_map(NameExpr::cast)
        .find(|n| n.name_token().is_some_and(|t| t.text() == "acc"))
        .expect("expected a NameExpr reading `acc`");
    let ptr = apex_binder::SyntaxPtr::new(file, acc_read.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(acc_sym)),
        "the binding should be visible inside its own when-clause body"
    );

    // And its declared type must actually be usable: `.Name` on it
    // should resolve against `Account`'s schema, the same as any other
    // Account-typed local.
    let name_access = root
        .descendants()
        .filter_map(FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "Name"))
        .expect("expected a FieldExpr accessing `.Name`");
    let ptr = apex_binder::SyntaxPtr::new(file, name_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
        "the binding's declared type should resolve its own field access"
    );
}

/// The binding must be scoped to *only* its own when-clause -- a sibling
/// `when` block's body (or code after the whole `switch`) must not see
/// it.
#[test]
fn a_switch_type_binding_is_not_visible_outside_its_own_when_clause() {
    const SRC: &str = "public class Foo { \
        public void run(Object x) { \
            switch on x { \
                when Account acc { \
                } \
                when else { \
                    Object leaked = acc; \
                } \
            } \
        } \
    }";
    let dir = write_fixture_dir("switch-type-binding-scoped", &[("Foo.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let root = program.syntax(file);
    let leaked_read = root
        .descendants()
        .filter_map(NameExpr::cast)
        .find(|n| n.name_token().is_some_and(|t| t.text() == "acc"))
        .expect("expected a NameExpr reading `acc` in the else-branch");
    let ptr = apex_binder::SyntaxPtr::new(file, leaked_read.syntax());
    assert_ne!(
        program.resolution(ptr).cloned(),
        None,
        "resolution_at should still find *some* recorded resolution (Unresolved), not nothing"
    );
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Unresolved),
        "a when-clause's own binding must not leak into a sibling when-clause's body"
    );
}
