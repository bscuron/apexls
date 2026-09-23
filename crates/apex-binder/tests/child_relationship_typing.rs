//! A parent's child-relationship collection (`opportunity.Payments__r`)
//! is a `List` of the child object, typed from the `<relationshipName>`
//! the child's own lookup field declares -- so a method called on it
//! (`.isEmpty()`, `.size()`) resolves instead of dead-ending. Only local
//! metadata names relationships: the bundled standard-object snapshot
//! carries none (checked in `standard_objects.json` itself), so a standard
//! child relationship such as `account.Contacts` stays unknown.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::MethodCallExpr;
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

const CHILD_OBJECT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <label>Payment</label>
    <pluralLabel>Payments</pluralLabel>
    <nameField><type>Text</type></nameField>
    <visibility>Public</visibility>
</CustomObject>"#;

const LOOKUP_FIELD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>Opportunity__c</fullName>
    <label>Opportunity</label>
    <referenceTo>Opportunity</referenceTo>
    <relationshipName>Payments__r</relationshipName>
    <type>Lookup</type>
</CustomField>"#;

/// The method call `<receiver>.<member>()` anywhere in the project.
fn call_resolution(program: &BoundProgram, member: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(call) = MethodCallExpr::cast(node) else {
                continue;
            };
            let Some(name) = call.method_name_token() else {
                continue;
            };
            if name.text() == member {
                let ptr = apex_binder::SyntaxPtr::new(file, call.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

#[test]
fn a_child_relationship_is_a_list_of_the_child_object() {
    let dir = write_fixture_dir(
        "child-relationship-typing",
        &[
            (
                "objects/Payment__c/Payment__c.object-meta.xml",
                CHILD_OBJECT,
            ),
            (
                "objects/Payment__c/fields/Opportunity__c.field-meta.xml",
                LOOKUP_FIELD,
            ),
            (
                "classes/Uses.cls",
                "public class Uses {\n    void go(Opportunity opp) {\n        Boolean none = opp.Payments__r.isEmpty();\n        String first = opp.Payments__r[0].Name;\n    }\n}\n",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    let isempty = call_resolution(&program, "isEmpty");
    std::fs::remove_dir_all(&dir).ok();

    // `List.isEmpty()` resolves only if the collection itself was typed.
    assert!(
        matches!(isempty, Some(Resolution::StdlibMember(_))),
        "expected a stdlib List member, got {isempty:?}"
    );
}

/// A *standard* relationship needs no local metadata at all: it comes
/// from the bundled describe-derived data (`account.Contacts`).
#[test]
fn a_standard_child_relationship_is_a_list_of_the_child_object() {
    let dir = write_fixture_dir(
        "standard-child-relationship",
        &[(
            "classes/Standard.cls",
            "public class Standard {
    void go(Account acc) {
        Boolean none = acc.Contacts.isEmpty();
    }
}
",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    let isempty = call_resolution(&program, "isEmpty");
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        matches!(isempty, Some(Resolution::StdlibMember(_))),
        "expected a stdlib List member, got {isempty:?}"
    );
}
