//! `Trigger.new`/`.old`/`.newMap`/`.oldMap` narrow to the trigger's own
//! declared `ON <Object>` type instead of the stdlib's generic
//! `List<SObject>`/`Map<Id,SObject>` -- see ticket 09's Answer
//! (`.scratch/apex-lsp-gaps/issues/09-trigger-narrowing-decision.md`) for
//! the real-org verification this is built against
//! (`Trigger.new[0].Email` -- a real field, just not on `Account` -- fails
//! to compile on an `on Account` trigger, proving the element type is
//! concretely the trigger's own object, not generic `SObject`).

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef, StdlibMemberRef};
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

fn field_expr_resolution(program: &BoundProgram, member: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(fe) = FieldExpr::cast(node) else {
                continue;
            };
            let Some(name) = fe.member_token() else {
                continue;
            };
            if name.text() == member {
                let ptr = apex_binder::SyntaxPtr::new(file, fe.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

#[test]
fn trigger_new_indexed_narrows_to_the_declared_object() {
    const SRC: &str = "trigger AccTrigger on Account (before insert) { \
        for (Account a : Trigger.new) { \
            String n = a.Name; \
        } \
        String n2 = Trigger.new[0].Name; \
    }";
    let dir = write_fixture_dir("trigger-new-narrow", &[("AccTrigger.trigger", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
        "Trigger.new[0].Name should resolve against Account's own schema, \
         proving the list element narrowed to Account instead of staying \
         a generic, memberless SObject"
    );
}

#[test]
fn trigger_newmap_values_indexed_narrows_to_the_declared_object() {
    const SRC: &str = "trigger AccTrigger on Account (before insert) { \
        String n = Trigger.newMap.values()[0].Name; \
    }";
    let dir = write_fixture_dir("trigger-newmap-narrow", &[("AccTrigger.trigger", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Account".into(),
            field: Some("Name".into()),
        }))),
        "Trigger.newMap.values()[0].Name should narrow through Map<Id,Account> \
         -> List<Account> -> Account the same way Trigger.new does"
    );
}

#[test]
fn trigger_narrowing_follows_the_triggers_own_declared_object_not_a_hardcoded_one() {
    const SRC: &str = "trigger ConTrigger on Contact (before insert) { \
        String n = Trigger.old[0].LastName; \
    }";
    let dir = write_fixture_dir("trigger-contact-narrow", &[("ConTrigger.trigger", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "LastName"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Contact".into(),
            field: Some("LastName".into()),
        }))),
        "a trigger declared `on Contact` should narrow Trigger.old to \
         Contact, not Account -- the object comes from this trigger's own \
         declaration, never a fixed default"
    );
}

#[test]
fn trigger_new_itself_still_resolves_as_the_ordinary_stdlib_member() {
    const SRC: &str =
        "trigger AccTrigger on Account (before insert) { List<Account> l = Trigger.new; }";
    let dir = write_fixture_dir("trigger-new-stdlib-member", &[("AccTrigger.trigger", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "new"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Trigger".into(),
            member: Some("new".into()),
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "narrowing the propagated type must not change what Trigger.new \
         itself resolves to -- hover/goto-definition on `new` still shows \
         the real Trigger.new stdlib member"
    );
}
