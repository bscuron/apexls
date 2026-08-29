//! Regression tests for custom-label resolution (`.labels-meta.xml`
//! discovery via `apex_discover`/`apex_metadata`, wired into
//! `crate::resolve::bind_field_expr` via `crate::label_index::LabelIndex`).
//! Before this, `Label.<name>`/`System.Label.<name>` -- real, common Apex
//! syntax for reading a custom label's declared value -- resolved as
//! `Resolution::Unresolved` unconditionally: `Label` itself is a real
//! stdlib class, but no project-specific label *names* were ever
//! discovered or indexed at all.

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

const CUSTOM_LABELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">
    <labels>
        <fullName>greeting</fullName>
        <language>en_US</language>
        <protected>false</protected>
        <shortDescription>greeting</shortDescription>
        <value>Hello there</value>
    </labels>
</CustomLabels>"#;

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
fn a_bare_label_reference_resolves() {
    let dir = write_fixture_dir(
        "label-bare",
        &[
            ("labels/CustomLabels.labels-meta.xml", CUSTOM_LABELS_XML),
            (
                "Foo.cls",
                "public class Foo { public String run() { return Label.greeting; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "greeting") {
        Some(Resolution::Label(r)) => assert_eq!(r.full_name, "greeting"),
        other => panic!("expected Resolution::Label, got {other:?}"),
    }
}

#[test]
fn a_system_qualified_label_reference_resolves_the_same_way() {
    let dir = write_fixture_dir(
        "label-system-qualified",
        &[
            ("labels/CustomLabels.labels-meta.xml", CUSTOM_LABELS_XML),
            (
                "Foo.cls",
                "public class Foo { public String run() { return System.Label.greeting; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "greeting") {
        Some(Resolution::Label(r)) => assert_eq!(r.full_name, "greeting"),
        other => panic!("expected Resolution::Label, got {other:?}"),
    }
}

#[test]
fn a_label_reference_is_case_insensitive() {
    let dir = write_fixture_dir(
        "label-case-insensitive",
        &[
            ("labels/CustomLabels.labels-meta.xml", CUSTOM_LABELS_XML),
            (
                "Foo.cls",
                "public class Foo { public String run() { return Label.GREETING; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "GREETING") {
        Some(Resolution::Label(r)) => assert_eq!(r.full_name, "greeting"),
        other => panic!("expected Resolution::Label, got {other:?}"),
    }
}

/// A name that isn't a declared label at all -- most often, in real NPSP
/// code, one segment of the `Label.<namespace>.<name>` cross-package form
/// (`System.Label.npo02.Foo`) -- stays honestly `Unresolved`, not a guessed
/// `String`, so a further chained reference off it doesn't silently
/// resolve against the wrong type either.
#[test]
fn an_undeclared_label_name_stays_unresolved() {
    let dir = write_fixture_dir(
        "label-undeclared",
        &[
            ("labels/CustomLabels.labels-meta.xml", CUSTOM_LABELS_XML),
            (
                "Foo.cls",
                "public class Foo { public String run() { return Label.doesNotExist; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "doesNotExist"),
        Some(Resolution::Unresolved)
    );
}

#[test]
fn a_label_reference_chains_into_a_further_string_method_call() {
    let dir = write_fixture_dir(
        "label-chained-call",
        &[
            ("labels/CustomLabels.labels-meta.xml", CUSTOM_LABELS_XML),
            (
                "Foo.cls",
                "public class Foo { public Boolean run() { return Label.greeting.isEmpty(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let resolution = program
        .all_resolutions()
        .find_map(|(_, r)| match r {
            Resolution::StdlibMember(m) if m.member.as_deref() == Some("isEmpty") => Some(r),
            _ => None,
        })
        .cloned();
    assert!(
        matches!(resolution, Some(Resolution::StdlibMember(_))),
        "expected .isEmpty() chained off the label to resolve as a real String method"
    );
}
