//! Phase 4: SOQL/SOSL grammar, reached as an expression `primary` --
//! matching `BaseApexParser.g4`'s `soqlLiteral`/`soslLiteral` alternatives
//! there (`primary: ... | soqlLiteral | soslLiteral | ...`), i.e. a query
//! is syntactically just another kind of value, usable anywhere an
//! expression is (assigned, passed as an argument, returned, ...).
//!
//! `soqlId`/`soslId` are `id` (`soqlId: id;`) or a dotted chain of it
//! (`soslId: id (DOT soslId)*`) per the reference grammar -- both reuse
//! `soql_field_name`'s dotted-`ids::is_id_kind` chain rather than having
//! separate implementations, since the shapes are identical.
//!
//! Some leaf productions (`fieldGroupByList`, `subFieldList`) aren't
//! published verbatim in the reference grammar excerpt this was built
//! from; they're reconstructed as the obvious `fieldName (',' fieldName)*`
//! shape every sibling list production in this file already uses.

use crate::parser::{CompletedMarker, Parser};
use apex_syntax::SyntaxKind;

pub(crate) fn at_soql_start(p: &Parser<'_>) -> bool {
    p.at(SyntaxKind::LBrack) && p.nth(1) == SyntaxKind::Select
}

pub(crate) fn at_sosl_start(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::FindLiteral | SyntaxKind::FindLiteralAlt
    ) || (p.at(SyntaxKind::LBrack) && p.nth(1) == SyntaxKind::Find)
}

/// `soqlLiteral: LBRACK query RBRACK`.
pub(crate) fn soql_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // [
    query(p);
    p.expect(SyntaxKind::RBrack);
    m.complete(p, SyntaxKind::SoqlExpr)
}

/// `soslLiteral: FindLiteral soslClauses RBRACK | LBRACK FIND
/// boundExpression soslClauses RBRACK`. `FindLiteral`/`FindLiteralAlt`
/// are single lexer tokens spanning `[find '...'`/`[find {...}` (see
/// `apex_lexer::TokenKind`'s doc comments) -- only the closing `]` and
/// `soslClauses` remain to parse in that branch.
pub(crate) fn sosl_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    match p.current() {
        SyntaxKind::FindLiteral | SyntaxKind::FindLiteralAlt => p.bump(),
        _ => {
            p.bump(); // [
            p.expect(SyntaxKind::Find);
            bound_expr(p);
        }
    }
    sosl_clauses(p);
    p.expect(SyntaxKind::RBrack);
    m.complete(p, SyntaxKind::SoslExpr)
}

/// Consumes an optional `soqlId` alias, but never one of the clause
/// keywords that can legally follow a `selectEntry`/`fromNameList`
/// element -- all of which are `id`-shaped per the reference grammar
/// (usable as ordinary identifiers *elsewhere*), so a naive `at_id` check
/// would greedily swallow a mandatory `FROM`/`WHERE`/etc as if it were an
/// alias. Real ANTLR resolves this via full-context adaptive lookahead;
/// this is the pragmatic hand-written equivalent -- excluding tokens that
/// would never sanely be used as an alias in practice, the same
/// real-world-frequency tradeoff already used for `instanceof` and the
/// arithmetic cast-operand exclusions in `grammar::expressions`.
fn maybe_alias(p: &mut Parser<'_>) {
    if super::ids::at_id(p) && !at_soql_clause_keyword(p) {
        p.bump();
    }
}

fn at_soql_clause_keyword(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::From
            | SyntaxKind::Where
            | SyntaxKind::With
            | SyntaxKind::Group
            | SyntaxKind::Order
            | SyntaxKind::Limit
            | SyntaxKind::Offset
            | SyntaxKind::For
            | SyntaxKind::Update
            | SyntaxKind::Using
            | SyntaxKind::All
            | SyntaxKind::Having
            | SyntaxKind::When
            | SyntaxKind::Then
            | SyntaxKind::End
            | SyntaxKind::Else
    )
}

// ==== SOQL ====

fn query(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::Select);
    select_list(p);
    p.expect(SyntaxKind::From);
    from_list(p);
    if p.at(SyntaxKind::Using) {
        using_scope(p);
    }
    if p.at(SyntaxKind::Where) {
        where_clause(p);
    }
    while p.at(SyntaxKind::With) {
        with_clause(p);
    }
    if p.at(SyntaxKind::Group) {
        group_by(p);
    }
    if p.at(SyntaxKind::Order) {
        order_by(p);
    }
    if p.at(SyntaxKind::Limit) {
        limit_clause(p);
    }
    if p.at(SyntaxKind::Offset) {
        offset_clause(p);
    }
    if p.at(SyntaxKind::All) && p.nth(1) == SyntaxKind::Rows {
        p.bump();
        p.bump();
    }
    for_clauses(p);
    if p.at(SyntaxKind::Update) {
        p.bump();
        update_list(p);
    }
}

/// `subQuery` -- `query`'s narrower cousin (no `usingScope`/`withClause`/
/// `groupByClause`/`offsetClause`/`allRowsClause`), used for `IN
/// (SELECT ...)` and parenthesized select-entry subqueries.
fn sub_query(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::Select);
    select_list(p);
    p.expect(SyntaxKind::From);
    from_list(p);
    if p.at(SyntaxKind::Where) {
        where_clause(p);
    }
    if p.at(SyntaxKind::Order) {
        order_by(p);
    }
    if p.at(SyntaxKind::Limit) {
        limit_clause(p);
    }
    for_clauses(p);
    if p.at(SyntaxKind::Update) {
        p.bump();
        update_list(p);
    }
    m.complete(p, SyntaxKind::SoqlSubQuery)
}

fn select_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    select_entry(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        select_entry(p);
    }
    m.complete(p, SyntaxKind::SoqlSelectList)
}

/// `selectEntry: fieldName soqlId? | soqlFunction soqlId? | LPAREN
/// subQuery RPAREN soqlId? | typeOf`.
fn select_entry(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(SyntaxKind::Typeof) {
        type_of(p);
    } else if p.at(SyntaxKind::LParen) {
        p.bump();
        sub_query(p);
        p.expect(SyntaxKind::RParen);
        maybe_alias(p);
    } else if at_soql_function_start(p) {
        soql_function(p);
        maybe_alias(p);
    } else {
        field_name(p);
        maybe_alias(p);
    }
    m.complete(p, SyntaxKind::SoqlSelectEntry)
}

/// `fieldName: soqlId (DOT soqlId)*` -- also used verbatim for `soslId`
/// (`id (DOT soslId)*`), the same shape.
fn field_name(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    super::ids::expect_id(p);
    while p.at(SyntaxKind::Dot) && super::ids::is_id_kind(p.nth(1)) {
        p.bump();
        p.bump();
    }
    m.complete(p, SyntaxKind::SoqlFieldName)
}

fn from_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    field_name(p);
    maybe_alias(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        field_name(p);
        maybe_alias(p);
    }
    m.complete(p, SyntaxKind::SoqlFromList)
}

fn using_scope(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // USING
    p.expect(SyntaxKind::Scope);
    super::ids::expect_id(p);
    m.complete(p, SyntaxKind::SoqlUsingScope)
}

fn where_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // WHERE
    logical_expr(p);
    m.complete(p, SyntaxKind::SoqlWhereClause)
}

/// `logicalExpression: conditionalExpression (SOQLAND conditionalExpression)*
/// | conditionalExpression (SOQLOR conditionalExpression)* | NOT
/// conditionalExpression`. Simplified to a single flat left-associative
/// AND/OR chain rather than the reference grammar's single-operator-kind
/// restriction: real-world queries needing to mix `AND`/`OR` always
/// parenthesize anyway (`conditionalExpression`'s own `LPAREN
/// logicalExpression RPAREN` alternative), so this never has to choose
/// between the two readings in practice.
fn logical_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(SyntaxKind::Not) {
        p.bump();
    }
    conditional_expr(p);
    while matches!(p.current(), SyntaxKind::SoqlAnd | SyntaxKind::SoqlOr) {
        p.bump();
        conditional_expr(p);
    }
    m.complete(p, SyntaxKind::SoqlLogicalExpr)
}

/// `conditionalExpression: LPAREN logicalExpression RPAREN |
/// fieldExpression`. No node of its own -- an `LPAREN`-wrapped group's
/// tokens become direct children of the enclosing `SoqlLogicalExpr`/
/// `SoqlComparison`.
fn conditional_expr(p: &mut Parser<'_>) {
    if p.at(SyntaxKind::LParen) {
        p.bump();
        logical_expr(p);
        p.expect(SyntaxKind::RParen);
    } else {
        field_expression(p);
    }
}

/// `fieldExpression: fieldName comparisonOperator value | soqlFunction
/// comparisonOperator value`.
fn field_expression(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if at_soql_function_start(p) {
        soql_function(p);
    } else {
        field_name(p);
    }
    comparison_operator(p);
    value(p);
    m.complete(p, SyntaxKind::SoqlComparison)
}

/// `comparisonOperator: ASSIGN | NOTEQUAL | LT | GT | LT ASSIGN | GT
/// ASSIGN | LESSANDGREATER | LIKE | IN | NOT IN | INCLUDES | EXCLUDES`.
/// `<=`/`>=` are two lexer tokens (the lexer never merges them, matching
/// `grammar::expressions`' own relational-operator handling), so `Lt`/`Gt`
/// speculatively swallow a following `Assign`.
fn comparison_operator(p: &mut Parser<'_>) {
    match p.current() {
        SyntaxKind::Assign
        | SyntaxKind::NotEqual
        | SyntaxKind::LessAndGreater
        | SyntaxKind::Like
        | SyntaxKind::Includes
        | SyntaxKind::Excludes
        | SyntaxKind::In => {
            p.bump();
        }
        SyntaxKind::Lt | SyntaxKind::Gt => {
            p.bump();
            if p.at(SyntaxKind::Assign) {
                p.bump();
            }
        }
        SyntaxKind::Not if p.nth(1) == SyntaxKind::In => {
            p.bump();
            p.bump();
        }
        _ => p.error(format!(
            "expected a comparison operator, found {:?}",
            p.current()
        )),
    }
}

/// `value: NULL | BooleanLiteral | signedNumber | StringLiteral |
/// MultilineStringLiteral | DateLiteral | TimeLiteral | DateTimeLiteral |
/// dateFormula | IntegralCurrencyLiteral (DOT IntegerLiteral?)? | LPAREN
/// subQuery RPAREN | valueList | boundExpression`.
fn value(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    match p.current() {
        SyntaxKind::Null
        | SyntaxKind::BooleanLiteral
        | SyntaxKind::StringLiteral
        | SyntaxKind::MultilineStringLiteral
        | SyntaxKind::DateLiteral
        | SyntaxKind::TimeLiteral
        | SyntaxKind::DateTimeLiteral
        | SyntaxKind::IntegerLiteral
        | SyntaxKind::NumberLiteral => {
            p.bump();
        }
        SyntaxKind::Add | SyntaxKind::Sub => {
            p.bump();
            if matches!(
                p.current(),
                SyntaxKind::IntegerLiteral | SyntaxKind::NumberLiteral
            ) {
                p.bump();
            } else {
                p.error("expected a number after sign");
            }
        }
        SyntaxKind::IntegralCurrencyLiteral => {
            p.bump();
            if p.at(SyntaxKind::Dot) {
                p.bump();
                if p.at(SyntaxKind::IntegerLiteral) {
                    p.bump();
                }
            }
        }
        SyntaxKind::Colon => {
            bound_expr(p);
        }
        SyntaxKind::LParen if p.nth(1) == SyntaxKind::Select => {
            p.bump();
            sub_query(p);
            p.expect(SyntaxKind::RParen);
        }
        SyntaxKind::LParen => {
            let vl = p.start();
            p.bump();
            if !p.at(SyntaxKind::RParen) {
                value(p);
                while p.at(SyntaxKind::Comma) {
                    p.bump();
                    value(p);
                }
            }
            p.expect(SyntaxKind::RParen);
            vl.complete(p, SyntaxKind::SoqlValueList);
        }
        k if is_date_formula_kind(k) => {
            p.bump();
            if p.at(SyntaxKind::Colon) {
                p.bump();
                if matches!(p.current(), SyntaxKind::Add | SyntaxKind::Sub) {
                    p.bump();
                }
                p.expect(SyntaxKind::IntegerLiteral);
            }
        }
        _ => p.error(format!("expected a value, found {:?}", p.current())),
    }
    m.complete(p, SyntaxKind::SoqlValue)
}

fn bound_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::Colon);
    super::expressions::expr(p);
    m.complete(p, SyntaxKind::SoqlBoundExpr)
}

fn with_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // WITH
    match p.current() {
        SyntaxKind::Data => {
            p.bump();
            p.expect(SyntaxKind::Category);
            filtering_expr(p);
        }
        SyntaxKind::SecurityEnforced | SyntaxKind::SystemMode | SyntaxKind::UserMode => {
            p.bump();
        }
        _ => {
            logical_expr(p);
        }
    }
    m.complete(p, SyntaxKind::SoqlWithClause)
}

/// `filteringExpression: dataCategorySelection (SOQLAND
/// dataCategorySelection)*`.
fn filtering_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    data_category_selection(p);
    while p.at(SyntaxKind::SoqlAnd) {
        p.bump();
        data_category_selection(p);
    }
    m.complete(p, SyntaxKind::SoqlFilteringExpr)
}

/// `dataCategorySelection: soqlId filteringSelector dataCategoryName`;
/// `filteringSelector: AT | ABOVE | BELOW | ABOVE_OR_BELOW`;
/// `dataCategoryName: soqlId | LPAREN soqlId (COMMA soqlId)* RPAREN`.
fn data_category_selection(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    super::ids::expect_id(p);
    if matches!(
        p.current(),
        SyntaxKind::At | SyntaxKind::Above | SyntaxKind::Below | SyntaxKind::AboveOrBelow
    ) {
        p.bump();
    } else {
        p.error("expected 'at', 'above', 'below', or an above-or-below operator");
    }
    if p.at(SyntaxKind::LParen) {
        p.bump();
        super::ids::expect_id(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            super::ids::expect_id(p);
        }
        p.expect(SyntaxKind::RParen);
    } else {
        super::ids::expect_id(p);
    }
    m.complete(p, SyntaxKind::SoqlDataCategorySelection)
}

fn group_by(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // GROUP
    p.expect(SyntaxKind::By);
    match p.current() {
        SyntaxKind::Rollup | SyntaxKind::Cube => {
            p.bump();
            p.expect(SyntaxKind::LParen);
            field_group_by_list(p);
            p.expect(SyntaxKind::RParen);
        }
        _ => field_group_by_list(p),
    }
    if p.at(SyntaxKind::Having) {
        p.bump();
        logical_expr(p);
    }
    m.complete(p, SyntaxKind::SoqlGroupBy)
}

/// `fieldGroupByList` -- reconstructed (not published verbatim in the
/// reference excerpt this was built from) as `groupByField (COMMA
/// groupByField)*` where each element is a `fieldName` *or* a
/// `soqlFunction`, matching real usage like `GROUP BY
/// CALENDAR_YEAR(CloseDate)` (date-grouping functions are the norm, not
/// the exception, for `GROUP BY` in practice).
fn field_group_by_list(p: &mut Parser<'_>) {
    group_by_field(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        group_by_field(p);
    }
}

fn group_by_field(p: &mut Parser<'_>) {
    if at_soql_function_start(p) {
        soql_function(p);
    } else {
        field_name(p);
    }
}

fn order_by(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // ORDER
    p.expect(SyntaxKind::By);
    field_order(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        field_order(p);
    }
    m.complete(p, SyntaxKind::SoqlOrderBy)
}

fn field_order(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if at_soql_function_start(p) {
        soql_function(p);
    } else {
        field_name(p);
    }
    if matches!(p.current(), SyntaxKind::Asc | SyntaxKind::Desc) {
        p.bump();
    }
    if p.at(SyntaxKind::Nulls) {
        p.bump();
        if matches!(p.current(), SyntaxKind::First | SyntaxKind::Last) {
            p.bump();
        } else {
            p.error("expected 'first' or 'last'");
        }
    }
    m.complete(p, SyntaxKind::SoqlFieldOrder)
}

fn limit_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // LIMIT
    if p.at(SyntaxKind::Colon) {
        bound_expr(p);
    } else {
        p.expect(SyntaxKind::IntegerLiteral);
    }
    m.complete(p, SyntaxKind::SoqlLimit)
}

fn offset_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // OFFSET
    if p.at(SyntaxKind::Colon) {
        bound_expr(p);
    } else {
        p.expect(SyntaxKind::IntegerLiteral);
    }
    m.complete(p, SyntaxKind::SoqlOffset)
}

fn for_clauses(p: &mut Parser<'_>) {
    while p.at(SyntaxKind::For) {
        let m = p.start();
        p.bump();
        if matches!(
            p.current(),
            SyntaxKind::View | SyntaxKind::Update | SyntaxKind::Reference
        ) {
            p.bump();
        } else {
            p.error("expected 'view', 'update', or 'reference'");
        }
        m.complete(p, SyntaxKind::SoqlForClause);
    }
}

/// `updateList: updateType (COMMA updateList)?`; `updateType: TRACKING |
/// VIEWSTAT`. Shared verbatim between SOQL's and SOSL's `(UPDATE
/// updateList)?` tails.
fn update_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    update_type(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        update_type(p);
    }
    m.complete(p, SyntaxKind::SoqlUpdateList)
}

fn update_type(p: &mut Parser<'_>) {
    if matches!(p.current(), SyntaxKind::Tracking | SyntaxKind::Viewstat) {
        p.bump();
    } else {
        p.error("expected 'tracking' or 'viewstat'");
    }
}

fn at_soql_function_start(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::Avg
            | SyntaxKind::Count
            | SyntaxKind::CountDistinct
            | SyntaxKind::Min
            | SyntaxKind::Max
            | SyntaxKind::Sum
            | SyntaxKind::ToLabel
            | SyntaxKind::Format
            | SyntaxKind::CalendarMonth
            | SyntaxKind::CalendarQuarter
            | SyntaxKind::CalendarYear
            | SyntaxKind::DayInMonth
            | SyntaxKind::DayInWeek
            | SyntaxKind::DayInYear
            | SyntaxKind::DayOnly
            | SyntaxKind::FiscalMonth
            | SyntaxKind::FiscalQuarter
            | SyntaxKind::FiscalYear
            | SyntaxKind::HourInDay
            | SyntaxKind::WeekInMonth
            | SyntaxKind::WeekInYear
            | SyntaxKind::Fields
            | SyntaxKind::Distance
            | SyntaxKind::Grouping
            | SyntaxKind::ConvertCurrency
    ) && p.nth(1) == SyntaxKind::LParen
}

/// `soqlFunction` -- one function-name keyword, `(`, a shape that varies
/// per function, `)`. Argument shapes are collapsed to whichever of
/// `fieldName` / `dateFieldName` / `locationValue` / a literal-list they
/// reduce to (see the function's own doc line in the reference grammar
/// for the exact alternative); this covers every function actually seen
/// in the NPSP corpus.
fn soql_function(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    let kind = p.current();
    p.bump(); // function name
    p.expect(SyntaxKind::LParen);
    match kind {
        SyntaxKind::Count if p.at(SyntaxKind::RParen) => {}
        SyntaxKind::Fields => {
            if matches!(
                p.current(),
                SyntaxKind::All | SyntaxKind::Custom | SyntaxKind::Standard
            ) {
                p.bump();
            } else {
                p.error("expected ALL, CUSTOM, or STANDARD");
            }
        }
        SyntaxKind::Distance => {
            location_value(p);
            p.expect(SyntaxKind::Comma);
            location_value(p);
            p.expect(SyntaxKind::Comma);
            if matches!(
                p.current(),
                SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral
            ) {
                p.bump();
            } else {
                p.error("expected a unit string literal");
            }
        }
        SyntaxKind::Format if at_soql_function_start(p) => {
            soql_function(p);
        }
        SyntaxKind::CalendarMonth
        | SyntaxKind::CalendarQuarter
        | SyntaxKind::CalendarYear
        | SyntaxKind::DayInMonth
        | SyntaxKind::DayInWeek
        | SyntaxKind::DayInYear
        | SyntaxKind::DayOnly
        | SyntaxKind::FiscalMonth
        | SyntaxKind::FiscalQuarter
        | SyntaxKind::FiscalYear
        | SyntaxKind::HourInDay
        | SyntaxKind::WeekInMonth
        | SyntaxKind::WeekInYear
            if p.at(SyntaxKind::ConvertTimezone) && p.nth(1) == SyntaxKind::LParen =>
        {
            p.bump();
            p.bump();
            field_name(p);
            p.expect(SyntaxKind::RParen);
        }
        _ => {
            field_name(p);
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::SoqlFunction)
}

/// `locationValue: fieldName | boundExpression | GEOLOCATION LPAREN
/// coordinateValue COMMA coordinateValue RPAREN`.
fn location_value(p: &mut Parser<'_>) {
    if p.at(SyntaxKind::Colon) {
        bound_expr(p);
    } else if p.at(SyntaxKind::Geolocation) && p.nth(1) == SyntaxKind::LParen {
        p.bump();
        p.bump();
        signed_number(p);
        p.expect(SyntaxKind::Comma);
        signed_number(p);
        p.expect(SyntaxKind::RParen);
    } else {
        field_name(p);
    }
}

fn signed_number(p: &mut Parser<'_>) {
    if matches!(p.current(), SyntaxKind::Add | SyntaxKind::Sub) {
        p.bump();
    }
    if matches!(
        p.current(),
        SyntaxKind::IntegerLiteral | SyntaxKind::NumberLiteral
    ) {
        p.bump();
    } else {
        p.error("expected a number");
    }
}

fn is_date_formula_kind(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Yesterday
            | SyntaxKind::Today
            | SyntaxKind::Tomorrow
            | SyntaxKind::LastWeek
            | SyntaxKind::ThisWeek
            | SyntaxKind::NextWeek
            | SyntaxKind::LastMonth
            | SyntaxKind::ThisMonth
            | SyntaxKind::NextMonth
            | SyntaxKind::Last90Days
            | SyntaxKind::Next90Days
            | SyntaxKind::LastNDaysN
            | SyntaxKind::NextNDaysN
            | SyntaxKind::NDaysAgoN
            | SyntaxKind::NextNWeeksN
            | SyntaxKind::LastNWeeksN
            | SyntaxKind::NWeeksAgoN
            | SyntaxKind::NextNMonthsN
            | SyntaxKind::LastNMonthsN
            | SyntaxKind::NMonthsAgoN
            | SyntaxKind::ThisQuarter
            | SyntaxKind::LastQuarter
            | SyntaxKind::NextQuarter
            | SyntaxKind::NextNQuartersN
            | SyntaxKind::LastNQuartersN
            | SyntaxKind::NQuartersAgoN
            | SyntaxKind::ThisYear
            | SyntaxKind::LastYear
            | SyntaxKind::NextYear
            | SyntaxKind::NextNYearsN
            | SyntaxKind::LastNYearsN
            | SyntaxKind::NYearsAgoN
            | SyntaxKind::ThisFiscalQuarter
            | SyntaxKind::LastFiscalQuarter
            | SyntaxKind::NextFiscalQuarter
            | SyntaxKind::NextNFiscalQuartersN
            | SyntaxKind::LastNFiscalQuartersN
            | SyntaxKind::NFiscalQuartersAgoN
            | SyntaxKind::ThisFiscalYear
            | SyntaxKind::LastFiscalYear
            | SyntaxKind::NextFiscalYear
            | SyntaxKind::NextNFiscalYearsN
            | SyntaxKind::LastNFiscalYearsN
            | SyntaxKind::NFiscalYearsAgoN
    )
}

/// `typeOf: TYPEOF fieldName whenClause+ elseClause? END`.
fn type_of(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // TYPEOF
    field_name(p);
    while p.at(SyntaxKind::When) {
        let wm = p.start();
        p.bump();
        field_name(p);
        p.expect(SyntaxKind::Then);
        field_name_list(p);
        wm.complete(p, SyntaxKind::SoqlWhenClause);
    }
    if p.at(SyntaxKind::Else) {
        let em = p.start();
        p.bump();
        field_name_list(p);
        em.complete(p, SyntaxKind::SoqlElseClause);
    }
    p.expect(SyntaxKind::End);
    m.complete(p, SyntaxKind::SoqlTypeOf)
}

fn field_name_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    field_name(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        field_name(p);
    }
    m.complete(p, SyntaxKind::SoqlFieldNameList)
}

// ==== SOSL ====

/// `soslClauses: (IN searchGroup)? (RETURNING fieldSpecList)?
/// soslWithClause* limitClause? (UPDATE updateList)?`.
fn sosl_clauses(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(SyntaxKind::In) {
        p.bump();
        search_group(p);
    }
    if p.at(SyntaxKind::Returning) {
        p.bump();
        field_spec_list(p);
    }
    while p.at(SyntaxKind::With) {
        sosl_with_clause(p);
    }
    if p.at(SyntaxKind::Limit) {
        limit_clause(p);
    }
    if p.at(SyntaxKind::Update) {
        p.bump();
        update_list(p);
    }
    m.complete(p, SyntaxKind::SoslClauses)
}

fn search_group(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if matches!(
        p.current(),
        SyntaxKind::All
            | SyntaxKind::Email
            | SyntaxKind::Name
            | SyntaxKind::Phone
            | SyntaxKind::Sidebar
    ) {
        p.bump();
    } else {
        p.error("expected a search group (ALL|EMAIL|NAME|PHONE|SIDEBAR)");
    }
    p.expect(SyntaxKind::Fields);
    m.complete(p, SyntaxKind::SoslSearchGroup)
}

fn field_spec_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    field_spec(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        field_spec(p);
    }
    m.complete(p, SyntaxKind::SoslFieldSpecList)
}

/// `fieldSpec: soslId (LPAREN fieldList (WHERE logicalExpression)?
/// (USING LISTVIEW ASSIGN soslId)? (ORDER BY fieldOrderList)?
/// limitClause? offsetClause? RPAREN)?`.
fn field_spec(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    field_name(p); // soslId, same dotted-id shape
    if p.at(SyntaxKind::LParen) {
        p.bump();
        sosl_field_list(p);
        if p.at(SyntaxKind::Where) {
            where_clause(p);
        }
        if p.at(SyntaxKind::Using) {
            p.bump();
            p.expect(SyntaxKind::Listview);
            p.expect(SyntaxKind::Assign);
            field_name(p);
        }
        if p.at(SyntaxKind::Order) {
            order_by(p);
        }
        if p.at(SyntaxKind::Limit) {
            limit_clause(p);
        }
        if p.at(SyntaxKind::Offset) {
            offset_clause(p);
        }
        p.expect(SyntaxKind::RParen);
    }
    m.complete(p, SyntaxKind::SoslFieldSpec)
}

/// `fieldList: soslId (COMMA fieldList)* | TOLABEL LPAREN soslId RPAREN
/// soslId? | CONVERT_CURRENCY LPAREN soslId RPAREN soslId? | FORMAT
/// LPAREN (soslId | soqlFunction) RPAREN soslId?`.
fn sosl_field_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    match p.current() {
        SyntaxKind::ToLabel | SyntaxKind::ConvertCurrency => {
            p.bump();
            p.expect(SyntaxKind::LParen);
            field_name(p);
            p.expect(SyntaxKind::RParen);
            maybe_alias(p);
        }
        SyntaxKind::Format => {
            p.bump();
            p.expect(SyntaxKind::LParen);
            if at_soql_function_start(p) {
                soql_function(p);
            } else {
                field_name(p);
            }
            p.expect(SyntaxKind::RParen);
            maybe_alias(p);
        }
        _ => {
            field_name(p);
            while p.at(SyntaxKind::Comma) {
                p.bump();
                field_name(p);
            }
        }
    }
    m.complete(p, SyntaxKind::SoslFieldList)
}

/// `soslWithClause` -- one of ten `WITH ...` forms; see the reference
/// grammar for the exact alternatives this mirrors.
fn sosl_with_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // WITH
    match p.current() {
        SyntaxKind::Division => {
            p.bump();
            p.expect(SyntaxKind::Assign);
            if p.at(SyntaxKind::Colon) {
                bound_expr(p);
            } else {
                sosl_string_literal(p);
            }
        }
        SyntaxKind::Data => {
            p.bump();
            p.expect(SyntaxKind::Category);
            filtering_expr(p);
        }
        SyntaxKind::Snippet => {
            p.bump();
            if p.at(SyntaxKind::LParen) {
                p.bump();
                p.expect(SyntaxKind::TargetLength);
                p.expect(SyntaxKind::Assign);
                p.expect(SyntaxKind::IntegerLiteral);
                p.expect(SyntaxKind::RParen);
            }
        }
        SyntaxKind::Network => {
            p.bump();
            if p.at(SyntaxKind::In) {
                p.bump();
                p.expect(SyntaxKind::LParen);
                network_list(p);
                p.expect(SyntaxKind::RParen);
            } else {
                p.expect(SyntaxKind::Assign);
                sosl_string_literal(p);
            }
        }
        SyntaxKind::PricebookId | SyntaxKind::Metadata => {
            p.bump();
            p.expect(SyntaxKind::Assign);
            sosl_string_literal(p);
        }
        SyntaxKind::Highlight | SyntaxKind::UserMode | SyntaxKind::SystemMode => {
            p.bump();
        }
        SyntaxKind::SpellCorrection => {
            p.bump();
            p.expect(SyntaxKind::Assign);
            if p.at(SyntaxKind::BooleanLiteral) {
                p.bump();
            } else {
                bound_expr(p);
            }
        }
        _ => p.error("expected a WITH clause"),
    }
    m.complete(p, SyntaxKind::SoslWithClause)
}

fn sosl_string_literal(p: &mut Parser<'_>) {
    if matches!(
        p.current(),
        SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral
    ) {
        p.bump();
    } else {
        p.error("expected a string literal");
    }
}

fn network_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    sosl_string_literal(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        sosl_string_literal(p);
    }
    m.complete(p, SyntaxKind::SoslNetworkList)
}
