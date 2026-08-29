//! Regression tests for the `object.fields.<FieldName>` describe-token
//! shorthand in `crate::resolve::bind_field_expr` -- real, documented
//! Apex syntax with a subtlety confirmed against a real org: the same-
//! looking `.fields.<FieldName>` means a *different* result type
//! depending on the receiver before it.
//!
//! - `<ObjectType>.fields.<FieldName>` (a bare SObject type name) yields
//!   `Schema.SObjectField` -- `Schema.SObjectField f = Account.fields.Name;`
//!   compiles.
//! - `SObjectType.<ObjectType>.fields.<FieldName>` (already an SObjectType-
//!   describe receiver, per `sobjectfield_token_resolution.rs`'s own
//!   `SObjectType.<ObjectName>` case) instead yields
//!   `Schema.DescribeFieldResult` --
//!   `Schema.SObjectField f2 = Schema.SObjectType.Account.fields.Name;`
//!   fails to compile with "Illegal assignment from Schema.DescribeFieldResult
//!   to Schema.SObjectField".
//!
//! Before this, `.fields` itself resolved (harmlessly) as
//! `Resolution::UnknownSchema` with no propagated `Ty` at all, so every
//! `.fields.<FieldName>` hop stayed `Resolution::Unresolved` regardless of
//! receiver -- hundreds of references in real NPSP code.

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

/// The bare-object-receiver ("token") mode: `Account.fields.Name` chains
/// into a real `Schema.SObjectField` method (`getDescribe()`).
#[test]
fn a_bare_object_fields_dot_field_chains_into_a_sobjectfield_method() {
    let dir = write_fixture_dir(
        "fields-dot-token-mode",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Schema.DescribeFieldResult d = Account.fields.Name.getDescribe(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
    );
    match method_call_resolution(&program, "getDescribe") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectField"),
        other => panic!("expected .getDescribe() to resolve as a real SObjectField method, got {other:?}"),
    }
}

/// The already-described-receiver ("describe") mode:
/// `SObjectType.Account.fields.Name` chains into a real
/// `Schema.DescribeFieldResult` method (`getLabel()`), a genuinely
/// different result type from the bare-object-receiver mode above.
#[test]
fn a_sobjecttype_prefixed_fields_dot_field_chains_into_a_describe_result_method() {
    let dir = write_fixture_dir(
        "fields-dot-describe-mode",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { String l = SObjectType.Account.fields.Name.getLabel(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
    );
    match method_call_resolution(&program, "getLabel") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "DescribeFieldResult"),
        other => panic!("expected .getLabel() to resolve as a real DescribeFieldResult method, got {other:?}"),
    }
}

/// The same shorthand off a project-local custom object/field, token mode.
#[test]
fn a_custom_object_fields_dot_field_chains_into_a_sobjectfield_method() {
    let dir = write_fixture_dir(
        "fields-dot-custom-object",
        &[
            (
                "objects/My_Object__c/My_Object__c.object-meta.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <label>My Object</label>
</CustomObject>"#,
            ),
            (
                "objects/My_Object__c/fields/Status__c.field-meta.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>Status__c</fullName>
    <type>Picklist</type>
</CustomField>"#,
            ),
            (
                "Foo.cls",
                "public class Foo { public void run() { Schema.DescribeFieldResult d = My_Object__c.fields.Status__c.getDescribe(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getDescribe") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectField"),
        other => panic!("expected .getDescribe() to resolve as a real SObjectField method, got {other:?}"),
    }
}

/// `.fields` itself has no real declaration of its own (it's compiler
/// magic, not a real field) -- resolves as `Resolution::UnknownSchema`,
/// matching what the generic schema fallback already gave it before this
/// fix existed, not a new `Resolution::Unresolved` regression.
#[test]
fn fields_itself_resolves_as_unknown_schema_not_unresolved() {
    let dir = write_fixture_dir(
        "fields-dot-fields-itself",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Schema.SObjectField f = Account.fields.Name; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "fields") {
        Some(Resolution::UnknownSchema(r)) => {
            assert_eq!(r.object.as_deref(), Some("Account"));
        }
        other => panic!("expected Resolution::UnknownSchema for the bare 'fields' hop, got {other:?}"),
    }
}
