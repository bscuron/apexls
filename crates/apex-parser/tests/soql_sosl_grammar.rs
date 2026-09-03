//! Targeted coverage for the SOQL/SOSL grammar's rarer productions
//! (`grammar::soql`) and their AST accessors (`apex_syntax::ast::soql`).
//!
//! The corpus-driven smoke test (`ast_smoke.rs`) only ever walks whatever
//! SOQL/SOSL shapes actually occur in the real NPSP corpus, so anything
//! real-world-rare but still real Apex syntax -- `USING SCOPE`, `TYPEOF`,
//! `FIELDS(ALL)`, `DISTANCE(...)`/`GEOLOCATION(...)`, every `WITH ...`
//! SOSL form, `UPDATE TRACKING`/`VIEWSTAT`, etc -- never got exercised.
//! Each case below is built directly from the exact production the
//! implementing function's own doc comment cites, not guessed.

use apex_parser::parse_expression;
use apex_syntax::ast::expr::Expr;
use apex_syntax::ast::soql::*;
use apex_syntax::AstNode;

fn assert_round_trips(src: &str) {
    let parse = parse_expression(src);
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

fn parse_soql(src: &str) -> SoqlExpr {
    let parse = parse_expression(src);
    assert!(
        parse.errors.is_empty(),
        "{src:?}: unexpected errors: {:?}",
        parse.errors
    );
    parse
        .syntax()
        .descendants()
        .find_map(SoqlExpr::cast)
        .unwrap_or_else(|| panic!("{src:?}: no SoqlExpr in the parsed tree"))
}

fn parse_sosl(src: &str) -> SoslExpr {
    let parse = parse_expression(src);
    assert!(
        parse.errors.is_empty(),
        "{src:?}: unexpected errors: {:?}",
        parse.errors
    );
    parse
        .syntax()
        .descendants()
        .find_map(SoslExpr::cast)
        .unwrap_or_else(|| panic!("{src:?}: no SoslExpr in the parsed tree"))
}

/// Every case here targets a specific previously-uncovered branch in
/// `grammar::soql`; see the comment on each group for which one.
#[test]
fn soql_rare_clauses_round_trip() {
    for src in [
        // `usingScope`.
        "[SELECT Id FROM Account USING SCOPE Mine]",
        // `ALL ROWS`.
        "[SELECT Id FROM Account ALL ROWS]",
        // `FOR VIEW` / `FOR UPDATE` / `FOR REFERENCE`.
        "[SELECT Id FROM Account FOR VIEW]",
        "[SELECT Id FROM Account FOR UPDATE]",
        "[SELECT Id FROM Account FOR REFERENCE]",
        // The query-level `UPDATE updateList` tail (`updateType (COMMA
        // updateList)?`, both `TRACKING`/`VIEWSTAT`).
        "[SELECT Id FROM Account UPDATE TRACKING]",
        "[SELECT Id FROM Account UPDATE TRACKING, VIEWSTAT]",
        // `LIMIT`/`OFFSET` as a bound expression rather than a literal.
        "[SELECT Id FROM Account LIMIT :myLimit]",
        "[SELECT Id FROM Account OFFSET :myOffset]",
        // `FIELDS(ALL|CUSTOM|STANDARD)` select-entry function.
        "[SELECT FIELDS(ALL) FROM Account LIMIT 10]",
        "[SELECT FIELDS(CUSTOM) FROM Account LIMIT 10]",
        "[SELECT FIELDS(STANDARD) FROM Account LIMIT 10]",
        // `DISTANCE(fieldName, GEOLOCATION(lat, long), unit)`, plus
        // `signed_number`'s sign-present branch (`-122.418`).
        "[SELECT Id FROM Account WHERE DISTANCE(Location__c, GEOLOCATION(37.775, -122.418), 'mi') < 20]",
        // `locationValue`'s own `boundExpression` alternative.
        "[SELECT Id FROM Account WHERE DISTANCE(:myLoc, GEOLOCATION(1, 2), 'mi') < 5]",
        // A date-grouping function's `ConvertTimezone(fieldName)` nested
        // form, and `FORMAT(soqlFunction)`'s nested-call form.
        "[SELECT CALENDAR_MONTH(ConvertTimezone(CreatedDate)), FORMAT(SUM(Amount)) FROM Opportunity]",
        // `typeOf: TYPEOF fieldName whenClause+ elseClause? END`, with a
        // multi-field `THEN` list exercising `field_name_list`'s own
        // comma loop.
        "[SELECT TYPEOF What WHEN Account THEN Phone, Name WHEN Opportunity THEN Amount ELSE Name END FROM Event]",
        // `WITH DATA CATEGORY ...`: the parenthesized multi-value
        // `dataCategoryName` form and the plain single-id form, chained
        // via `filteringExpression`'s `AND` loop.
        "[SELECT Id FROM Account WITH DATA CATEGORY Geography__c AT (America, Europe) AND Product__c ABOVE_OR_BELOW Printers]",
        // `IntegralCurrencyLiteral (DOT IntegerLiteral?)?`.
        "[SELECT Id FROM Account WHERE AnnualRevenue > USD25000.50]",
        // `value`'s own signed-literal alternative (distinct from
        // `signed_number`, which only backs `GEOLOCATION`'s coordinates).
        "[SELECT Id FROM Account WHERE AnnualRevenue = -5]",
    ] {
        assert_round_trips(src);
    }
}

#[test]
fn sosl_forms_round_trip() {
    for src in [
        // The `FindLiteral`/`FindLiteralAlt` single-token forms (`[find
        // '...'`/`[find {...}`), as opposed to the `[FIND :boundExpr`
        // form every existing test already used.
        "[find 'test' RETURNING Account]",
        "[find {test} RETURNING Account]",
        // `IN <searchGroup> FIELDS`, and a `RETURNING` list wide enough
        // to hit `fieldSpec`'s every optional tail (`WHERE`, `USING
        // LISTVIEW = ...`, `ORDER BY`, `LIMIT`, `OFFSET`), `sosl_field_list`'s
        // `TOLABEL(...)`/`FORMAT(...)` alternatives, and the query-level
        // `UPDATE updateList` tail.
        "[find 'test' IN ALL FIELDS RETURNING \
            Account(toLabel(Status)), \
            Contact(Name, Phone WHERE Name != null USING LISTVIEW = MyListView ORDER BY Name LIMIT 5 OFFSET 2), \
            Opportunity(FORMAT(Amount)), \
            Lead(FORMAT(SUM(Amount))) \
            LIMIT 10 UPDATE TRACKING, VIEWSTAT]",
        // Every `soslWithClause` alternative, one query each.
        "[find 'test' RETURNING Account WITH DIVISION = 'Global']",
        "[find 'test' RETURNING Account WITH DIVISION = :myDivision]",
        "[find 'test' RETURNING Account WITH DATA CATEGORY Geography__c AT America]",
        "[find 'test' RETURNING Account WITH SNIPPET(TARGET_LENGTH=120)]",
        "[find 'test' RETURNING Account WITH NETWORK IN ('001xx0000000001AAA', '001xx0000000002AAA')]",
        "[find 'test' RETURNING Account WITH NETWORK = '001xx0000000001AAA']",
        "[find 'test' RETURNING Account WITH PricebookId = '01sxx0000000001AAA']",
        "[find 'test' RETURNING Account WITH HIGHLIGHT]",
        "[find 'test' RETURNING Account WITH SPELL_CORRECTION = true]",
        "[find 'test' RETURNING Account WITH SPELL_CORRECTION = :flag]",
    ] {
        assert_round_trips(src);
    }
}

/// Every one of `grammar::soql`'s `p.error(...)` fallback arms, hit with
/// deliberately malformed input -- proves each produces its documented
/// message rather than panicking or silently accepting garbage.
#[test]
fn malformed_soql_sosl_reports_the_expected_error() {
    let cases: &[(&str, &str)] = &[
        (
            "[SELECT Id FROM Account WHERE Name BOGUS 'x']",
            "expected a comparison operator",
        ),
        (
            "[SELECT Id FROM Account WHERE Name = ,]",
            "expected a value",
        ),
        (
            "[SELECT Id FROM Account WITH DATA CATEGORY Geography__c ZZZ America]",
            "expected 'at', 'above', 'below', or an above-or-below operator",
        ),
        (
            "[SELECT Id FROM Account FOR BOGUS]",
            "expected 'view', 'update', or 'reference'",
        ),
        (
            "[SELECT Id FROM Account ORDER BY Name NULLS BOGUS]",
            "expected 'first' or 'last'",
        ),
        (
            "[SELECT Id FROM Account WHERE DISTANCE(Location__c, GEOLOCATION(x, 2), 'mi') < 5]",
            "expected a number",
        ),
        (
            "[SELECT Id FROM Account WHERE AnnualRevenue = -x]",
            "expected a number after sign",
        ),
        (
            "[find 'test' IN BOGUS FIELDS RETURNING Account]",
            "expected a search group",
        ),
        (
            "[find 'test' RETURNING Account WITH DIVISION = 123]",
            "expected a string literal",
        ),
        (
            "[find 'test' RETURNING Account WITH BOGUS]",
            "expected a WITH clause",
        ),
    ];
    for (src, expected_message) in cases {
        let parse = parse_expression(src);
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
fn soql_ast_accessors_cover_the_rare_clauses() {
    let soql = parse_soql("[SELECT Id FROM Account USING SCOPE Mine]");
    let scope = soql.using_scope().expect("using_scope");
    assert_eq!(scope.scope().unwrap().text(), "Mine");

    let soql = parse_soql("[SELECT Id FROM Account ALL ROWS]");
    assert!(soql.is_all_rows());

    let soql = parse_soql("[SELECT Id FROM Account FOR UPDATE]");
    let for_clause = soql.for_clauses().next().expect("for clause");
    assert_eq!(for_clause.kind_token().unwrap().text(), "UPDATE");

    let soql = parse_soql("[SELECT Id FROM Account UPDATE TRACKING, VIEWSTAT]");
    let update_list = soql.update_list().expect("update list");
    let kinds: Vec<_> = update_list
        .update_types()
        .iter()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(kinds, vec!["TRACKING", "VIEWSTAT"]);

    let soql = parse_soql("[SELECT Id FROM Account LIMIT :myLimit]");
    assert!(soql.limit().unwrap().count_token().is_none());
    assert!(soql.limit().unwrap().bound_expr().is_some());

    let soql = parse_soql("[SELECT Id FROM Account OFFSET :myOffset]");
    assert!(soql.offset().unwrap().count_token().is_none());
    assert!(soql.offset().unwrap().bound_expr().is_some());

    let soql = parse_soql(
        "[SELECT Id FROM Account WITH DATA CATEGORY Geography__c AT (America, Europe) AND Product__c ABOVE_OR_BELOW Printers]",
    );
    let with_clause = soql.with_clauses().next().expect("with clause");
    let selections: Vec<_> = with_clause
        .filtering_expr()
        .expect("filtering expr")
        .selections()
        .collect();
    assert_eq!(
        selections.len(),
        2,
        "expected two AND-chained data category selections"
    );

    let soql = parse_soql("[SELECT Id FROM Account ORDER BY Name DESC NULLS LAST]");
    let field_order = soql.order_by().unwrap().field_orders().next().unwrap();
    assert!(field_order.is_descending());
    assert!(!field_order.is_ascending());
    assert!(field_order.nulls_last());
    assert!(!field_order.nulls_first());

    let soql = parse_soql(
        "[SELECT TYPEOF What WHEN Account THEN Phone, Name WHEN Opportunity THEN Amount ELSE Name END FROM Event]",
    );
    let select_entry = soql.select_list().unwrap().entries().next().unwrap();
    let type_of = select_entry.type_of().expect("type_of");
    let when_clauses: Vec<_> = type_of.when_clauses().collect();
    assert_eq!(when_clauses.len(), 2);
    let then_fields: Vec<_> = when_clauses[0]
        .then_fields()
        .expect("then fields")
        .fields()
        .map(|f| f.text().to_string())
        .collect();
    assert_eq!(then_fields, vec!["Phone", "Name"]);
    let else_fields: Vec<_> = type_of
        .else_clause()
        .expect("else clause")
        .fields()
        .expect("else fields")
        .fields()
        .map(|f| f.text().to_string())
        .collect();
    assert_eq!(else_fields, vec!["Name"]);
}

#[test]
fn sosl_ast_accessors_cover_the_rare_clauses() {
    let sosl = parse_sosl("[find 'test' RETURNING Account]");
    assert_eq!(sosl.find_literal().unwrap().text(), "[find 'test'");

    let sosl = parse_sosl("[find 'test' IN ALL FIELDS RETURNING Account]");
    let group = sosl
        .clauses()
        .unwrap()
        .search_group()
        .expect("search group");
    assert_eq!(group.group_token().unwrap().text(), "ALL");

    let sosl = parse_sosl(
        "[find 'test' RETURNING Account(Name, Phone WHERE Name != null USING LISTVIEW = MyListView)]",
    );
    let spec = sosl
        .clauses()
        .unwrap()
        .field_spec_list()
        .unwrap()
        .specs()
        .next()
        .unwrap();
    assert_eq!(spec.object().unwrap().text(), "Account");
    assert!(spec.where_clause().is_some());
    assert_eq!(spec.using_listview().unwrap().text(), "MyListView");
    let fields: Vec<_> = spec
        .field_list()
        .unwrap()
        .fields()
        .map(|f| f.text().to_string())
        .collect();
    assert_eq!(fields, vec!["Name", "Phone"]);

    let sosl = parse_sosl("[find 'test' RETURNING Account(FORMAT(Amount))]");
    let spec = sosl
        .clauses()
        .unwrap()
        .field_spec_list()
        .unwrap()
        .specs()
        .next()
        .unwrap();
    // `FORMAT(<plain field>)` -- no nested function, just the field.
    assert!(spec.field_list().unwrap().function().is_none());
    let fields: Vec<_> = spec
        .field_list()
        .unwrap()
        .fields()
        .map(|f| f.text().to_string())
        .collect();
    assert_eq!(fields, vec!["Amount"]);

    let sosl = parse_sosl("[find 'test' RETURNING Account(FORMAT(SUM(Amount)))]");
    let spec = sosl
        .clauses()
        .unwrap()
        .field_spec_list()
        .unwrap()
        .specs()
        .next()
        .unwrap();
    // `FORMAT(<nested soqlFunction>)` -- the `SUM(...)` call itself.
    assert!(spec.field_list().unwrap().function().is_some());

    let sosl = parse_sosl("[find 'test' RETURNING Account WITH NETWORK IN ('001xx0000000001AAA', '001xx0000000002AAA')]");
    let with_clause = sosl.clauses().unwrap().with_clauses().next().unwrap();
    assert_eq!(with_clause.kind_token().unwrap().text(), "NETWORK");
    let values: Vec<_> = with_clause
        .network_list()
        .expect("network list")
        .values()
        .iter()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(values, vec!["'001xx0000000001AAA'", "'001xx0000000002AAA'"]);

    let sosl = parse_sosl("[find 'test' RETURNING Account WITH DIVISION = :myDivision]");
    let with_clause = sosl.clauses().unwrap().with_clauses().next().unwrap();
    assert!(with_clause.bound_expr().is_some());

    let sosl =
        parse_sosl("[find 'test' RETURNING Account WITH DATA CATEGORY Geography__c AT America]");
    let with_clause = sosl.clauses().unwrap().with_clauses().next().unwrap();
    assert!(with_clause.filtering_expr().is_some());
}

/// Sanity check that `SoqlLogicalExpr::is_negated`/`SoqlWithClause`'s
/// three security-mode booleans -- all previously-uncovered `bool`
/// accessors -- actually distinguish the negated/each-mode case from the
/// plain one, not just "doesn't panic".
#[test]
fn boolean_accessors_distinguish_present_from_absent() {
    let negated = parse_soql("[SELECT Id FROM Account WHERE NOT Name = null]");
    let cond = negated.where_clause().unwrap().condition().unwrap();
    assert!(cond.is_negated());

    let plain = parse_soql("[SELECT Id FROM Account WHERE Name = null]");
    let cond = plain.where_clause().unwrap().condition().unwrap();
    assert!(!cond.is_negated());

    let security_enforced = parse_soql("[SELECT Id FROM Account WITH SECURITY_ENFORCED]");
    let with_clause = security_enforced.with_clauses().next().unwrap();
    assert!(with_clause.is_security_enforced());
    assert!(!with_clause.is_user_mode());
    assert!(!with_clause.is_system_mode());

    let user_mode = parse_soql("[SELECT Id FROM Account WITH USER_MODE]");
    let with_clause = user_mode.with_clauses().next().unwrap();
    assert!(with_clause.is_user_mode());
    assert!(!with_clause.is_security_enforced());

    let system_mode = parse_soql("[SELECT Id FROM Account WITH SYSTEM_MODE]");
    let with_clause = system_mode.with_clauses().next().unwrap();
    assert!(with_clause.is_system_mode());
    assert!(!with_clause.is_user_mode());
}

/// `Expr::Sosl`/`Expr::Soql` -- the dispatch-enum entry points a real
/// caller (e.g. `apex-binder`) actually uses, as opposed to this file's
/// own `SoqlExpr::cast`/`SoslExpr::cast` shortcut.
#[test]
fn soql_and_sosl_are_reachable_through_the_expr_dispatch_enum() {
    let parse = parse_expression("[SELECT Id FROM Account]");
    let expr = Expr::cast(parse.syntax().first_child().unwrap()).unwrap();
    assert!(matches!(expr, Expr::Soql(_)));

    let parse = parse_expression("[find 'test' RETURNING Account]");
    let expr = Expr::cast(parse.syntax().first_child().unwrap()).unwrap();
    assert!(matches!(expr, Expr::Sosl(_)));
}
