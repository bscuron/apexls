//! Regression test for ticket 39's 2b: `Schema.SObjectTypeFields`/
//! `Schema.SObjectTypeFieldSets` (`.fields`/`.fieldSets`'s own real result
//! types) had no entry anywhere in the stdlib snapshot, so even once the
//! `.fields.<FieldName>`/`.fieldSets.<FieldSetName>` token shorthand
//! resolved correctly, a real *method* call chained directly off `.fields`/
//! `.fieldSets` instead (`.getMap()`) had no stdlib class to look the
//! method up against and stayed `Unresolved`. Real, common NPSP idiom
//! (`fflib_SObjectDescribe.cls:54,62`):
//! `describe.fields.getMap()`/`describe.fieldSets.getMap()`.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::MethodCallExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Foo.cls"), src).unwrap();
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

/// `object.fields.getMap()` (bare-object-type receiver mode) resolves as a
/// real `SObjectTypeFields.getMap()` method call.
#[test]
fn fields_getmap_resolves_on_the_bare_object_type_receiver() {
    let dir = write_fixture_dir(
        "fields-getmap-token",
        "public class Foo { public void run() { Map<String, Schema.SObjectField> m = Account.fields.getMap(); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getMap") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectTypeFields"),
        other => panic!("expected .fields.getMap() to resolve as a real SObjectTypeFields method, got {other:?}"),
    }
}

/// The already-described receiver mode (`Schema.SObjectType.<Object>.fields`)
/// resolves the same real method.
#[test]
fn fields_getmap_resolves_on_the_already_described_receiver() {
    let dir = write_fixture_dir(
        "fields-getmap-describe",
        "public class Foo { public void run() { Map<String, Schema.SObjectField> m = Schema.SObjectType.Account.fields.getMap(); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getMap") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectTypeFields"),
        other => panic!("expected .fields.getMap() to resolve as a real SObjectTypeFields method, got {other:?}"),
    }
}

/// `object.fieldSets.getMap()` resolves as a real `SObjectTypeFieldSets.getMap()`
/// method call, the same fall-through mirrored for `fieldSets`.
#[test]
fn fieldsets_getmap_resolves() {
    let dir = write_fixture_dir(
        "fieldsets-getmap",
        "public class Foo { public void run() { Map<String, Schema.FieldSet> m = Account.fieldSets.getMap(); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getMap") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectTypeFieldSets"),
        other => panic!("expected .fieldSets.getMap() to resolve as a real SObjectTypeFieldSets method, got {other:?}"),
    }
}

/// A real NPSP shape chaining a further call off the result:
/// `.fields.getMap().keySet().contains(...)` -- confirms `getMap()`'s own
/// return type propagates correctly (`Map<String, Schema.SObjectField>`),
/// not just that the call itself resolves.
#[test]
fn fields_getmap_return_type_supports_a_further_chained_call() {
    let dir = write_fixture_dir(
        "fields-getmap-chained",
        "public class Foo { public void run() { Boolean has = Account.fields.getMap().keySet().contains('name'); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "contains") {
        Some(Resolution::StdlibMember(_)) => {}
        other => panic!("expected the chained .contains() to resolve as a real Set method, got {other:?}"),
    }
}

/// A real, common NPSP shape (`fflib_SObjectDescribe.cls:43-54`): the
/// `Schema.DescribeSObjectResult` receiver is a plain declared field/
/// property, not derived from an `<ObjectName>.SObjectType`/
/// `SObjectType.<ObjectName>` chain in the same expression, so there's no
/// owning-object name to recover at all. `.getMap()` still resolves --
/// unlike the bare-field-name shorthand (`.fields.<FieldName>`), it needs
/// no owner to look up a real method against.
#[test]
fn fields_getmap_resolves_even_when_the_owning_object_is_unrecoverable() {
    let dir = write_fixture_dir(
        "fields-getmap-unknown-owner",
        "public class Foo { Schema.DescribeSObjectResult describe; public void run() { Map<String, Schema.SObjectField> m = describe.fields.getMap(); } }",
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match method_call_resolution(&program, "getMap") {
        Some(Resolution::StdlibMember(m)) => assert_eq!(m.class_name, "SObjectTypeFields"),
        other => panic!("expected .fields.getMap() to resolve as a real SObjectTypeFields method even with no known owner, got {other:?}"),
    }
}
