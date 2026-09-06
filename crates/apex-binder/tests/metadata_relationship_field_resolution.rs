//! Regression test for ticket 40's section 6: `EntityDefinition` -- a real,
//! permanent Salesforce standard pseudo-object used to describe objects
//! dynamically -- had no entry at all in `standard_objects.json`, so any
//! `MetadataRelationship`-typed custom-metadata field pointing at it (a
//! real, common shape backing NPSP's Customizable Rollups engine, e.g.
//! `fflib_AppBinding__mdt.BindingObject__c`) resolved the relationship
//! traversal itself but had no schema to resolve a further field off it
//! against: `binding.BindingObject__r.QualifiedApiName` (real NPSP shape,
//! `fflib_AppBindingMetaDataModule.cls:57`) stayed `Unresolved`.
//!
//! `FieldDefinition` -- the sibling pseudo-object describing a *field*
//! rather than an object, real NPSP shape `CRLP_Rollup_SEL.cls`'s
//! `Summary_Field__r.QualifiedApiName`/`.Label` off `Rollup__mdt` -- had the
//! exact same gap and gets the same fix here.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::FieldExpr;
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

const OBJECT_META: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <label>Test Binding</label>
    <pluralLabel>Test Bindings</pluralLabel>
    <visibility>Public</visibility>
</CustomObject>"#;

const FIELD_META: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>BindingObject__c</fullName>
    <label>Binding Object</label>
    <referenceTo>EntityDefinition</referenceTo>
    <relationshipName>BindingObjects</relationshipName>
    <type>MetadataRelationship</type>
</CustomField>"#;

/// `<Field>__r.QualifiedApiName` off a `MetadataRelationship` field
/// resolves as a real `EntityDefinition` schema field, not `Unresolved`.
#[test]
fn metadata_relationship_field_resolves_qualifiedapiname_on_entitydefinition() {
    let dir = write_fixture_dir(
        "entitydefinition-qualifiedapiname",
        &[
            (
                "objects/Test_Binding__mdt/Test_Binding__mdt.object-meta.xml",
                OBJECT_META,
            ),
            (
                "objects/Test_Binding__mdt/fields/BindingObject__c.field-meta.xml",
                FIELD_META,
            ),
            (
                "Foo.cls",
                "public class Foo { public void run(Test_Binding__mdt binding) { String api = binding.BindingObject__r.QualifiedApiName; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "QualifiedApiName"),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "EntityDefinition".into(),
            field: Some("QualifiedApiName".into()),
        }))),
    );
}

/// Apex field-member lookup is case-insensitive, mirroring real NPSP
/// source which spells this same field two ways in different files
/// (`QualifiedApiName` and `QualifiedAPIName`).
#[test]
fn metadata_relationship_field_resolution_is_case_insensitive() {
    let dir = write_fixture_dir(
        "entitydefinition-qualifiedapiname-case",
        &[
            (
                "objects/Test_Binding__mdt/Test_Binding__mdt.object-meta.xml",
                OBJECT_META,
            ),
            (
                "objects/Test_Binding__mdt/fields/BindingObject__c.field-meta.xml",
                FIELD_META,
            ),
            (
                "Foo.cls",
                "public class Foo { public void run(Test_Binding__mdt binding) { String api = binding.BindingObject__r.QualifiedAPIName; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "QualifiedAPIName"),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "EntityDefinition".into(),
            field: Some("QualifiedAPIName".into()),
        }))),
    );
}

const FIELD_DEFINITION_RELATIONSHIP_FIELD_META: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>SummaryField__c</fullName>
    <label>Summary Field</label>
    <referenceTo>FieldDefinition</referenceTo>
    <relationshipName>SummaryFields</relationshipName>
    <type>MetadataRelationship</type>
</CustomField>"#;

/// The sibling pseudo-object `FieldDefinition` (describes a *field*, not an
/// object) gets the same fix: `<Field>__r.QualifiedApiName`/`.Label` off a
/// `MetadataRelationship` field pointing at it resolves as a real schema
/// field, not `Unresolved`.
#[test]
fn metadata_relationship_field_resolves_qualifiedapiname_on_fielddefinition() {
    let dir = write_fixture_dir(
        "fielddefinition-qualifiedapiname",
        &[
            (
                "objects/Test_Rollup__mdt/Test_Rollup__mdt.object-meta.xml",
                OBJECT_META,
            ),
            (
                "objects/Test_Rollup__mdt/fields/SummaryField__c.field-meta.xml",
                FIELD_DEFINITION_RELATIONSHIP_FIELD_META,
            ),
            (
                "Foo.cls",
                "public class Foo { public void run(Test_Rollup__mdt rollup) { String api = rollup.SummaryField__r.QualifiedApiName; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "QualifiedApiName"),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "FieldDefinition".into(),
            field: Some("QualifiedApiName".into()),
        }))),
    );
}
