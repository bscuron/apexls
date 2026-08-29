//! Regression tests for two related schema-describe-token shapes in
//! `crate::resolve::bind_field_expr`:
//!
//! - `<ObjectType>.<Field>` (a bare SObject *type* name, not an instance,
//!   dotted with a field API name) -- real, documented Apex shorthand for
//!   a `Schema.SObjectField` describe token, confirmed against a real
//!   org: `Schema.SObjectField f = Account.Name;` compiles and
//!   `f.getDescribe()` works. Before this, the field's own scalar/
//!   relationship type was always propagated regardless of whether the
//!   receiver was a bare type name or a real instance -- correct for the
//!   instance case (`acct.Name` is really a `String`), but wrong here,
//!   where a chained `.getDescribe()`/`.getName()`/`.getLabel()` call
//!   (hundreds of references in NPSP alone) always stayed `Unresolved`
//!   since a `String`/`Boolean`/... has no such method.
//! - `SObjectType.<ObjectName>` (bare, reversed order from the above) --
//!   a genuinely *different* compiler-magic idiom, confirmed against a
//!   real org to evaluate to a `Schema.DescribeSObjectResult`, not a
//!   `Schema.SObjectType` token (`SObjectType.Account`'s live runtime
//!   type dumped as `Schema.DescribeSObjectResult`). Before this it
//!   propagated the object's own type instead, so `.getName()`/
//!   `.getLabel()` chained off it stayed `Unresolved` the same way.

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

/// `Account.Name` (bare type, no instance) chains into `.getDescribe()` --
/// resolves as a real `Schema.SObjectField` method, not `Unresolved`.
#[test]
fn a_bare_standard_object_type_field_token_chains_into_getdescribe() {
    let dir = write_fixture_dir(
        "sobjectfield-token-standard",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Schema.DescribeFieldResult d = Account.Name.getDescribe(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getDescribe") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectField"),
        other => panic!("expected .getDescribe() to resolve as a real SObjectField method, got {other:?}"),
    }
}

/// The same shorthand off a project-local custom object/field.
#[test]
fn a_bare_custom_object_type_field_token_chains_into_getdescribe() {
    let dir = write_fixture_dir(
        "sobjectfield-token-custom",
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
                "public class Foo { public void run() { Schema.DescribeFieldResult d = My_Object__c.Status__c.getDescribe(); } }",
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

/// The exact same field name accessed off a real *instance* still
/// resolves as the field's own scalar value type (`String`), not
/// `SObjectField` -- confirms the bare-type-name detection doesn't
/// misfire on an ordinary instance field read.
#[test]
fn an_instance_field_access_still_resolves_as_the_scalar_field_value() {
    let dir = write_fixture_dir(
        "sobjectfield-token-instance-not-shadowed",
        &[(
            "Foo.cls",
            "public class Foo { public void run(Account acct) { String trimmed = acct.Name.trim(); } }",
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
    match method_call_resolution(&program, "trim") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "String"),
        other => panic!("expected .trim() on an instance field read to resolve as a real String method, got {other:?}"),
    }
}

/// The bare `SObjectType.<ObjectName>` idiom -- a genuinely different
/// compiler-magic construct from `<ObjectName>.SObjectType` (confirmed
/// against a real org: `SObjectType.Account`'s live runtime type is
/// `Schema.DescribeSObjectResult`, not `Schema.SObjectType`) -- chains
/// into a real `DescribeSObjectResult` method (`getName()`).
#[test]
fn a_bare_sobjecttype_dot_object_name_chains_into_a_describe_result_method() {
    let dir = write_fixture_dir(
        "sobjecttype-bare-prefix",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { String n = SObjectType.Account.getName(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getName") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "DescribeSObjectResult"),
        other => panic!("expected .getName() to resolve as a real DescribeSObjectResult method, got {other:?}"),
    }
}
