//! Targeted coverage for `grammar::declarations` productions no other
//! test exercises: the comma-separated and bare-literal annotation-
//! argument forms, and a handful of `p.error(...)` fallback arms hit
//! with deliberately malformed declarations -- proving each produces its
//! documented message and recovers cleanly rather than panicking.

use apex_parser::{parse_compilation_unit, parse_trigger_unit};
use apex_syntax::ast::decl::{MethodDecl, PropertyAccessor, TriggerUnit};
use apex_syntax::AstNode;

fn assert_round_trips(src: &str) {
    let parse = parse_compilation_unit(src);
    assert!(
        parse.errors.is_empty(),
        "{src:?}: unexpected errors: {:?}",
        parse.errors
    );
    assert_eq!(
        parse.syntax().text().to_string(),
        src,
        "{src:?} did not round-trip"
    );
}

#[test]
fn declaration_forms_round_trip() {
    for src in [
        // `elementValuePairs`'s comma between pairs is optional (per its
        // own doc comment) -- only the space-separated form had ever
        // been exercised before.
        "public class Foo { @SuppressWarnings(a=1, b=2) public void run() { } }",
        // The bare-`elementValue` annotation-argument form (a single
        // literal, no `name=` prefix).
        "@SuppressWarnings('PMD') public class Foo { }",
        // A stray `;` directly in a class body (`classBodyDeclaration: ';' | ...`).
        "public class Foo { ; }",
    ] {
        assert_round_trips(src);
    }
}

#[test]
fn malformed_declarations_report_the_expected_error() {
    let cases: &[(&str, &str)] = &[
        (
            "@Foo(x=bogus) public class Bar { }",
            "expected a literal value",
        ),
        (
            "public class Foo extends { }",
            "expected a type after 'extends'",
        ),
        ("public class Foo implements { }", "expected a type"),
        ("public class Foo implements Bar, { }", "expected a type"),
        (
            "public class Foo { public 5; }",
            "expected a member declaration",
        ),
        (
            "public class Foo { public Integer 5; }",
            "expected a member name",
        ),
        (
            "public class Foo { public void run(5) { } }",
            "expected a parameter type",
        ),
        (
            "public class Foo { public Integer X { bogus; } }",
            "expected 'get' or 'set'",
        ),
        (
            "public interface Foo { 5 bar(); }",
            "expected a return type",
        ),
    ];
    for (src, expected_message) in cases {
        let parse = parse_compilation_unit(src);
        assert!(
            parse
                .errors
                .iter()
                .any(|e| e.message.contains(expected_message)),
            "{src:?}: expected an error containing {expected_message:?}, got {:?}",
            parse.errors
        );
    }
}

#[test]
fn trigger_exposes_every_before_after_case() {
    let src = "trigger Foo on Account (before insert, after update) { }";
    let parse = parse_trigger_unit(src);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let tu = TriggerUnit::cast(parse.syntax()).expect("expected a TriggerUnit");
    let cases: Vec<_> = tu
        .cases()
        .map(|c| c.syntax().text().to_string().trim().to_string())
        .collect();
    assert_eq!(cases, vec!["before insert", "after update"]);
}

#[test]
fn method_decl_is_void_distinguishes_void_from_a_real_return_type() {
    let parse = parse_compilation_unit(
        "public class Foo { public void run() { } public Integer get() { return 1; } }",
    );
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let methods: Vec<MethodDecl> = parse
        .syntax()
        .descendants()
        .filter_map(MethodDecl::cast)
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(methods[0].is_void());
    assert!(!methods[1].is_void());
}

#[test]
fn property_accessor_is_getter_distinguishes_get_from_set() {
    let parse = parse_compilation_unit("public class Foo { public Integer X { get; set; } }");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let accessors: Vec<PropertyAccessor> = parse
        .syntax()
        .descendants()
        .filter_map(PropertyAccessor::cast)
        .collect();
    assert_eq!(accessors.len(), 2);
    assert!(accessors[0].is_getter());
    assert!(!accessors[1].is_getter());
}
