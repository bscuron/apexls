//! Regression tests for standard SObject/field schema resolution
//! (`apex_stdlib`'s bundled snapshot, merged into `SchemaIndex` --
//! `crates/apex-binder/src/schema_index.rs::merge_sobjects`). Before
//! this, *every* standard-object field access (`acct.Name`) resolved as
//! `Resolution::UnknownSchema` (or `Unresolved`, for a standard object
//! with zero local customization) since `apex_metadata` never had any
//! schema for standard objects at all -- see that crate's own module
//! doc comment. No `crate::resolve` code changed to make this work:
//! `bind_field_expr`'s existing `schema.field(...)`/`schema.object(...)`
//! branch just started finding real data.

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef};
use apex_syntax::ast::expr::FieldExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

/// Finds the `FieldExpr` whose member name is `member`, across every
/// file in `program` -- there's only ever one file in these fixtures,
/// but this avoids hardcoding which one.
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
fn a_standard_object_field_access_resolves_to_schema_object() {
    let dir = write_fixture_dir(
        "standard-field-access",
        &[(
            "Foo.cls",
            "public class Foo { public void run(Account acct) { String n = acct.Name; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
        "Account.Name should resolve against apex_stdlib's bundled standard schema"
    );
}

/// `Account.OwnerId`'s bundled `reference_to: ["User"]` should continue
/// the `Ty` chain, exactly like a local custom lookup field's
/// `reference_to` already does -- so `.Username` on the result also
/// resolves against the (bundled) `User` schema, not `Unresolved`.
#[test]
fn a_standard_lookup_fields_reference_to_continues_the_type_chain() {
    let dir = write_fixture_dir(
        "standard-lookup-chain",
        &[(
            "Foo.cls",
            "public class Foo { public void run(Account acct) { String u = acct.OwnerId.Username; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "OwnerId"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Account".into(),
            field: Some("OwnerId".into()),
        }))),
    );
    assert_eq!(
        field_expr_resolution(&program, "Username"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "User".into(),
            field: Some("Username".into()),
        }))),
        "acct.OwnerId.Username should resolve past the lookup hop, not stop at Unresolved"
    );
}
