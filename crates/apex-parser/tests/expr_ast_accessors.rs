//! Targeted coverage for `apex_syntax::ast::expr` accessors no other
//! test calls: `ThisExpr`/`SuperExpr::keyword()`, and
//! `FieldExpr`/`MethodCallExpr::is_null_safe()` (the `?.` operator).

use apex_parser::parse_expression;
use apex_syntax::ast::expr::{FieldExpr, MethodCallExpr, SuperExpr, ThisExpr};
use apex_syntax::AstNode;

fn parse<T: AstNode<Language = apex_syntax::ApexLanguage>>(src: &str) -> T {
    let parse = parse_expression(src);
    assert!(
        parse.errors.is_empty(),
        "{src:?}: unexpected errors: {:?}",
        parse.errors
    );
    parse
        .syntax()
        .descendants()
        .find_map(T::cast)
        .unwrap_or_else(|| panic!("{src:?}: no matching node in the parsed tree"))
}

#[test]
fn this_and_super_expose_their_own_keyword_token() {
    let this_expr: ThisExpr = parse("this");
    assert_eq!(this_expr.keyword().unwrap().text(), "this");

    let super_expr: SuperExpr = parse("super");
    assert_eq!(super_expr.keyword().unwrap().text(), "super");
}

#[test]
fn field_access_null_safety_is_distinguished_from_plain_dot() {
    let plain: FieldExpr = parse("a.b");
    assert!(!plain.is_null_safe());

    let null_safe: FieldExpr = parse("a?.b");
    assert!(null_safe.is_null_safe());
}

#[test]
fn method_call_null_safety_is_distinguished_from_plain_dot() {
    let plain: MethodCallExpr = parse("a.b()");
    assert!(!plain.is_null_safe());

    let null_safe: MethodCallExpr = parse("a?.b()");
    assert!(null_safe.is_null_safe());
}
