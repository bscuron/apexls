//! Targeted coverage for `apex_syntax::ast::stmt` accessors that no
//! existing test (corpus-driven or otherwise) ever exercised, even
//! though the underlying syntax round-trips fine in `apex-parser`'s own
//! `lib.rs` test suite: `switch`'s `when else`/`when <literal-list>`
//! forms, DML's `AS SYSTEM`/`AS USER` access-level clause, `upsert`'s
//! optional external-ID field, and a local declaration's `final`/
//! `transient` prefixes. Parsing these was already proven correct;
//! nothing had ever called the AST layer built on top.

use apex_parser::parse_statement;
use apex_syntax::ast::stmt::{
    AccessLevelClause, InsertStmt, LocalVarDeclStmt, MergeStmt, SwitchStmt, UpsertStmt, WhenValue,
};
use apex_syntax::AstNode;

fn parse<T: AstNode<Language = apex_syntax::ApexLanguage>>(src: &str) -> T {
    let parse = parse_statement(src);
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
fn when_else_is_recognized_as_the_else_form() {
    let switch: SwitchStmt = parse("switch on x { when 1 { a(); } when else { b(); } }");
    let whens: Vec<_> = switch.when_clauses().collect();
    assert_eq!(whens.len(), 2);

    let literal_value = whens[0].value().expect("first when has a value");
    assert!(!literal_value.is_else());

    let else_value = whens[1].value().expect("second when has a value");
    assert!(else_value.is_else());
}

#[test]
fn a_literal_list_when_value_exposes_every_literal() {
    let switch: SwitchStmt = parse("switch on x { when 1, 2, 3 { a(); } }");
    let value: WhenValue = switch
        .when_clauses()
        .next()
        .and_then(|w| w.value())
        .expect("expected a when value");
    assert!(!value.is_else());
    assert!(
        value.type_ref().is_none(),
        "a literal list is not a type-pattern"
    );
    let texts: Vec<_> = value
        .literals()
        .map(|l| l.syntax().text().to_string().trim().to_string())
        .collect();
    assert_eq!(texts, vec!["1", "2", "3"]);
}

#[test]
fn dml_access_level_clause_distinguishes_system_from_user() {
    let insert: InsertStmt = parse("insert as system records;");
    let clause: AccessLevelClause = insert
        .access_level()
        .expect("expected an access-level clause");
    assert!(clause.is_system());
    assert!(!clause.is_user());

    let upsert: UpsertStmt = parse("upsert as user records;");
    let clause = upsert
        .access_level()
        .expect("expected an access-level clause");
    assert!(clause.is_user());
    assert!(!clause.is_system());

    let merge: MergeStmt = parse("merge as system a b;");
    let clause = merge
        .access_level()
        .expect("expected an access-level clause");
    assert!(clause.is_system());
}

#[test]
fn a_plain_dml_statement_has_no_access_level_clause() {
    let insert: InsertStmt = parse("insert records;");
    assert!(insert.access_level().is_none());
}

#[test]
fn upsert_exposes_its_optional_external_id_field() {
    let with_field: UpsertStmt = parse("upsert records MyField__c;");
    let field = with_field
        .external_id_field()
        .expect("expected an external ID field");
    assert_eq!(field.syntax().text().to_string(), "MyField__c");

    let without_field: UpsertStmt = parse("upsert records;");
    assert!(without_field.external_id_field().is_none());
}

#[test]
fn merge_exposes_master_and_duplicate_as_distinct_expressions() {
    let merge: MergeStmt = parse("merge masterRecord duplicateRecord;");
    let master = merge.master().expect("expected a master expression");
    let duplicate = merge.duplicate().expect("expected a duplicate expression");
    assert_eq!(master.syntax().text().to_string(), "masterRecord ");
    assert_eq!(duplicate.syntax().text().to_string(), "duplicateRecord");
}

#[test]
fn local_var_decl_final_and_transient_prefixes_are_independently_detected() {
    let plain: LocalVarDeclStmt = parse("Integer x = 5;");
    assert!(!plain.is_final());
    assert!(!plain.is_transient());

    let final_decl: LocalVarDeclStmt = parse("final Integer x = 5;");
    assert!(final_decl.is_final());
    assert!(!final_decl.is_transient());

    let transient_decl: LocalVarDeclStmt = parse("transient Integer x;");
    assert!(transient_decl.is_transient());
    assert!(!transient_decl.is_final());
}
