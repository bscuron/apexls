//! Regression tests for the `object.fieldSets.<FieldSetName>` describe-token
//! shorthand in `crate::resolve::bind_field_expr` -- the same compiler-magic
//! shape `object_fields_dot_resolution.rs` covers for `.fields`, but for
//! `.fieldSets`. Real, common NPSP idiom:
//! `Schema.SObjectType.Allocation__c.fieldSets.ManageAllocationsAdditionalFields.getFields();`
//!
//! Unlike `.fields` (whose result type differs by receiver mode --
//! `Schema.SObjectField` vs `Schema.DescribeFieldResult`), `.fieldSets.<Name>`
//! always yields the same real type, `Schema.FieldSet`, regardless of
//! whether the receiver was a bare object type or an already-described
//! `Schema.DescribeSObjectResult`.
//!
//! Before this, `.fieldSets` itself resolved (harmlessly) as
//! `Resolution::Unresolved` with no propagated `Ty` at all, so every
//! `.fieldSets.<Name>` hop -- and everything chained off it -- stayed
//! unresolved regardless of receiver.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::{FieldExpr, MethodCallExpr};
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        let path = dir.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
    }
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

/// The describe-receiver mode (the shape every real NPSP call site uses):
/// `Schema.SObjectType.Account.fieldSets.Name.getFields()` chains into a
/// real `Schema.FieldSet` method.
#[test]
fn a_sobjecttype_prefixed_fieldsets_dot_name_chains_into_a_fieldset_method() {
    let dir = write_fixture_dir(
        "fieldsets-dot-describe-mode",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { List<Schema.FieldSetMember> f = Schema.SObjectType.Account.fieldSets.MyFieldSet.getFields(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "MyFieldSet"),
        Some(Resolution::UnknownSchema(Box::new(apex_binder::UnknownSchemaRef {
            object: Some("Account".into()),
            field: Some("MyFieldSet".into()),
        }))),
    );
    match method_call_resolution(&program, "getFields") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "FieldSet"),
        other => panic!("expected .getFields() to resolve as a real FieldSet method, got {other:?}"),
    }
}

/// The bare-object-receiver mode: `Account.fieldSets.Name` chains into the
/// same `Schema.FieldSet` method (no separate "token" type the way
/// `.fields` has).
#[test]
fn a_bare_object_fieldsets_dot_name_chains_into_a_fieldset_method() {
    let dir = write_fixture_dir(
        "fieldsets-dot-token-mode",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { List<Schema.FieldSetMember> f = Account.fieldSets.MyFieldSet.getFields(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getFields") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "FieldSet"),
        other => panic!("expected .getFields() to resolve as a real FieldSet method, got {other:?}"),
    }
}

/// `.fieldSets` itself has no real declaration of its own (it's compiler
/// magic, not a real field) -- resolves as `Resolution::UnknownSchema`,
/// harmless since `unknown_schema_diagnostics` only fires on `SoqlFieldName`
/// nodes, never this plain `FieldExpr` hop.
#[test]
fn fieldsets_itself_resolves_as_unknown_schema_not_unresolved() {
    let dir = write_fixture_dir(
        "fieldsets-dot-fieldsets-itself",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Schema.FieldSet fs = Schema.SObjectType.Account.fieldSets.MyFieldSet; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "fieldSets") {
        Some(Resolution::UnknownSchema(r)) => {
            assert_eq!(r.object.as_deref(), Some("Account"));
        }
        other => panic!("expected Resolution::UnknownSchema for the bare 'fieldSets' hop, got {other:?}"),
    }
}
