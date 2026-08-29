//! Regression tests for `Page.<name>` resolution -- real Apex compiler-
//! magic syntax (`PageReference pr = Page.MyPage;`) with no real "Page"
//! class anywhere in Salesforce's own docs (unlike `Label`, which is a
//! real `System.Label` class -- see `crate::page_index::PageIndex`'s own
//! doc comment). Before this, `Page.<name>` resolved as
//! `Resolution::Unresolved` unconditionally: the bare `Page` identifier
//! had nothing to match (no local/member/type/schema-object/stdlib-class
//! named "Page"), and neither did the `.member` hop off it.

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

#[test]
fn a_visualforce_page_reference_resolves() {
    let dir = write_fixture_dir(
        "vf-page",
        &[
            ("pages/MyPage.page", "<apex:page>Hello</apex:page>"),
            (
                "Foo.cls",
                "public class Foo { public PageReference run() { return Page.MyPage; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "MyPage") {
        Some(Resolution::VisualforcePage(r)) => assert_eq!(r.name, "MyPage"),
        other => panic!("expected Resolution::VisualforcePage, got {other:?}"),
    }
}

#[test]
fn a_visualforce_page_reference_is_case_insensitive() {
    let dir = write_fixture_dir(
        "vf-page-case-insensitive",
        &[
            ("pages/MyPage.page", "<apex:page>Hello</apex:page>"),
            (
                "Foo.cls",
                "public class Foo { public PageReference run() { return Page.mypage; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "mypage") {
        Some(Resolution::VisualforcePage(r)) => assert_eq!(r.name, "MyPage"),
        other => panic!("expected Resolution::VisualforcePage, got {other:?}"),
    }
}

/// A real project type genuinely named `Page` (Apex doesn't reserve the
/// word) shadows the compiler-magic fallback, matching real Apex/Java
/// member-vs-namespace shadowing precedence.
#[test]
fn a_real_project_type_named_page_shadows_the_visualforce_fallback() {
    let dir = write_fixture_dir(
        "vf-page-shadowed",
        &[
            ("pages/MyPage.page", "<apex:page>Hello</apex:page>"),
            (
                "Page.cls",
                "public class Page { public static String MyPage = 'x'; }",
            ),
            (
                "Foo.cls",
                "public class Foo { public String run() { return Page.MyPage; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    match field_expr_resolution(&program, "MyPage") {
        Some(Resolution::Resolved(_)) => {}
        other => panic!("expected the real Page.cls field to win, got {other:?}"),
    }
}

/// An undeclared page name stays honestly `Unresolved`, not a guessed
/// `PageReference`.
#[test]
fn an_undeclared_page_name_stays_unresolved() {
    let dir = write_fixture_dir(
        "vf-page-undeclared",
        &[(
            "Foo.cls",
            "public class Foo { public PageReference run() { return Page.DoesNotExist; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "DoesNotExist"),
        Some(Resolution::Unresolved)
    );
}

#[test]
fn a_page_reference_chains_into_a_further_pagereference_method_call() {
    let dir = write_fixture_dir(
        "vf-page-chained-call",
        &[
            ("pages/MyPage.page", "<apex:page>Hello</apex:page>"),
            (
                "Foo.cls",
                "public class Foo { public String run() { return Page.MyPage.getUrl(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let resolution = program
        .all_resolutions()
        .find_map(|(_, r)| match r {
            Resolution::StdlibMember(m) if m.member.as_deref() == Some("getUrl") => Some(r),
            _ => None,
        })
        .cloned();
    assert!(
        matches!(resolution, Some(Resolution::StdlibMember(_))),
        "expected .getUrl() chained off the page to resolve as a real PageReference method"
    );
}
