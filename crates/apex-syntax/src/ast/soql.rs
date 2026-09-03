//! Typed AST wrappers for SOQL/SOSL grammar (`grammar::soql`), the
//! previously-opaque part of the AST layer: before this, `Expr::Soql`/
//! `Expr::Sosl` only told you a query existed, not what it selects or
//! queries. Real Apex leans on SOQL constantly, so this closes the
//! single biggest hole in what the AST layer can walk into.
//!
//! A few productions are deliberately left shallow (`SoqlDataCategorySelection`,
//! `SoqlFunction`'s rarer argument shapes like `DISTANCE`/`FIELDS`) --
//! low real-world frequency (Salesforce Knowledge data categories,
//! geolocation functions), not forgotten; `.syntax().text()` still gets
//! the full text for anything not broken out into its own accessor.

use super::{
    ast_node, direct_tokens, dispatch_enum, first_non_trivia_token, last_non_trivia_token, Expr,
};
use crate::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::{support, AstChildren, AstNode};
use smol_str::SmolStr;

// A `GROUP BY`/`ORDER BY` element -- both are `fieldName | soqlFunction`
// per the reference grammar, the same shape, so one dispatch enum
// covers both `SoqlGroupBy::fields()` and `SoqlFieldOrder::target()`.
dispatch_enum! {
    SoqlFieldOrFunction {
        Function(SoqlFunction) => SoqlFunction,
        Field(SoqlFieldName) => SoqlFieldName,
    }
}

// A `SoqlLogicalExpr`'s operand: either a real comparison, or a
// parenthesized nested group -- `conditionalExpression`'s `LPAREN
// logicalExpression RPAREN` alternative has no node of its own (see
// `grammar::soql::conditional_expr`'s doc comment), so a nested
// `SoqlLogicalExpr` appears as a direct child of its parent, not
// wrapped in anything marking it as "the parenthesized case".
dispatch_enum! {
    SoqlCondition {
        Comparison(SoqlComparison) => SoqlComparison,
        Group(SoqlLogicalExpr) => SoqlLogicalExpr,
    }
}

ast_node!(SoqlExpr, SoqlExpr);
ast_node!(SoqlSelectList, SoqlSelectList);
ast_node!(SoqlSelectEntry, SoqlSelectEntry);
ast_node!(SoqlFieldName, SoqlFieldName);
ast_node!(SoqlFromList, SoqlFromList);
ast_node!(SoqlUsingScope, SoqlUsingScope);
ast_node!(SoqlWhereClause, SoqlWhereClause);
ast_node!(SoqlLogicalExpr, SoqlLogicalExpr);
ast_node!(SoqlComparison, SoqlComparison);
ast_node!(SoqlValue, SoqlValue);
ast_node!(SoqlValueList, SoqlValueList);
ast_node!(SoqlWithClause, SoqlWithClause);
ast_node!(SoqlFilteringExpr, SoqlFilteringExpr);
ast_node!(SoqlDataCategorySelection, SoqlDataCategorySelection);
ast_node!(SoqlGroupBy, SoqlGroupBy);
ast_node!(SoqlOrderBy, SoqlOrderBy);
ast_node!(SoqlFieldOrder, SoqlFieldOrder);
ast_node!(SoqlLimit, SoqlLimit);
ast_node!(SoqlOffset, SoqlOffset);
ast_node!(SoqlForClause, SoqlForClause);
ast_node!(SoqlUpdateList, SoqlUpdateList);
ast_node!(SoqlBoundExpr, SoqlBoundExpr);
ast_node!(SoqlFunction, SoqlFunction);
ast_node!(SoqlTypeOf, SoqlTypeOf);
ast_node!(SoqlWhenClause, SoqlWhenClause);
ast_node!(SoqlElseClause, SoqlElseClause);
ast_node!(SoqlFieldNameList, SoqlFieldNameList);
ast_node!(SoqlSubQuery, SoqlSubQuery);

ast_node!(SoslExpr, SoslExpr);
ast_node!(SoslClauses, SoslClauses);
ast_node!(SoslSearchGroup, SoslSearchGroup);
ast_node!(SoslFieldSpecList, SoslFieldSpecList);
ast_node!(SoslFieldSpec, SoslFieldSpec);
ast_node!(SoslWithClause, SoslWithClause);
ast_node!(SoslFieldList, SoslFieldList);
ast_node!(SoslNetworkList, SoslNetworkList);

/// The last direct token, unless it's a bare `)` -- for the several
/// productions shaped `... '(' subthing ')' soqlId?`, where a naive
/// "last direct token" would return the closing paren itself on the
/// (common) no-alias case instead of `None`.
fn alias_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let last = last_non_trivia_token(node)?;
    (last.kind() != SyntaxKind::RParen).then_some(last)
}

impl SoqlExpr {
    pub fn select_list(&self) -> Option<SoqlSelectList> {
        support::child(self.syntax())
    }

    pub fn from_list(&self) -> Option<SoqlFromList> {
        support::child(self.syntax())
    }

    pub fn using_scope(&self) -> Option<SoqlUsingScope> {
        support::child(self.syntax())
    }

    pub fn where_clause(&self) -> Option<SoqlWhereClause> {
        support::child(self.syntax())
    }

    pub fn with_clauses(&self) -> AstChildren<SoqlWithClause> {
        support::children(self.syntax())
    }

    pub fn group_by(&self) -> Option<SoqlGroupBy> {
        support::child(self.syntax())
    }

    pub fn order_by(&self) -> Option<SoqlOrderBy> {
        support::child(self.syntax())
    }

    pub fn limit(&self) -> Option<SoqlLimit> {
        support::child(self.syntax())
    }

    pub fn offset(&self) -> Option<SoqlOffset> {
        support::child(self.syntax())
    }

    /// `ALL ROWS`.
    pub fn is_all_rows(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Rows).is_some()
    }

    pub fn for_clauses(&self) -> AstChildren<SoqlForClause> {
        support::children(self.syntax())
    }

    pub fn update_list(&self) -> Option<SoqlUpdateList> {
        support::child(self.syntax())
    }
}

impl SoqlSubQuery {
    pub fn select_list(&self) -> Option<SoqlSelectList> {
        support::child(self.syntax())
    }

    pub fn from_list(&self) -> Option<SoqlFromList> {
        support::child(self.syntax())
    }

    pub fn where_clause(&self) -> Option<SoqlWhereClause> {
        support::child(self.syntax())
    }

    pub fn order_by(&self) -> Option<SoqlOrderBy> {
        support::child(self.syntax())
    }

    pub fn limit(&self) -> Option<SoqlLimit> {
        support::child(self.syntax())
    }

    pub fn for_clauses(&self) -> AstChildren<SoqlForClause> {
        support::children(self.syntax())
    }

    pub fn update_list(&self) -> Option<SoqlUpdateList> {
        support::child(self.syntax())
    }
}

impl SoqlSelectList {
    pub fn entries(&self) -> AstChildren<SoqlSelectEntry> {
        support::children(self.syntax())
    }
}

impl SoqlSelectEntry {
    pub fn type_of(&self) -> Option<SoqlTypeOf> {
        support::child(self.syntax())
    }

    pub fn sub_query(&self) -> Option<SoqlSubQuery> {
        support::child(self.syntax())
    }

    pub fn function(&self) -> Option<SoqlFunction> {
        support::child(self.syntax())
    }

    pub fn field_name(&self) -> Option<SoqlFieldName> {
        support::child(self.syntax())
    }

    /// `Some` only for the `fieldName`/`soqlFunction`/subquery forms
    /// (`typeOf` never takes one).
    pub fn alias(&self) -> Option<SyntaxToken> {
        alias_token(self.syntax())
    }
}

impl SoqlFieldName {
    /// The dotted path's individual identifier tokens, `Dot` tokens
    /// excluded.
    pub fn segments(&self) -> Vec<SyntaxToken> {
        direct_tokens(self.syntax())
            .filter(|t| t.kind() != SyntaxKind::Dot)
            .collect()
    }

    /// The dotted path as plain text (`Account.Owner.Name`), built by
    /// walking tokens directly (not via `segments()`, and not
    /// `self.syntax().text()`, so it can never pick up stray trivia) so
    /// the common single-segment case (`Name`, `Amount`, no dots)
    /// converts straight from the one token's `&str` into a `SmolStr`
    /// with no intermediate `Vec`/`String`/`join` allocation -- only a
    /// genuinely dotted relationship path (`Account.Owner.Name`) pays for
    /// building one.
    pub fn text(&self) -> SmolStr {
        let mut tokens = direct_tokens(self.syntax()).filter(|t| t.kind() != SyntaxKind::Dot);
        let Some(first) = tokens.next() else {
            return SmolStr::default();
        };
        match tokens.next() {
            None => SmolStr::new(first.text()),
            Some(second) => {
                let mut joined = String::from(first.text());
                joined.push('.');
                joined.push_str(second.text());
                for t in tokens {
                    joined.push('.');
                    joined.push_str(t.text());
                }
                SmolStr::from(joined)
            }
        }
    }
}

impl SoqlFromList {
    /// The queried object(s) -- more than one only for the rare
    /// multi-object form. Per-entry aliases aren't exposed here (they're
    /// bare tokens with no wrapper node of their own to hang an accessor
    /// off, unlike `SoqlSelectEntry::alias`); walk `.syntax()` directly
    /// if an entry's alias is needed.
    pub fn entries(&self) -> AstChildren<SoqlFieldName> {
        support::children(self.syntax())
    }
}

impl SoqlUsingScope {
    pub fn scope(&self) -> Option<SyntaxToken> {
        last_non_trivia_token(self.syntax())
    }
}

impl SoqlWhereClause {
    pub fn condition(&self) -> Option<SoqlLogicalExpr> {
        support::child(self.syntax())
    }
}

impl SoqlLogicalExpr {
    pub fn is_negated(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Not).is_some()
    }

    pub fn conditions(&self) -> AstChildren<SoqlCondition> {
        support::children(self.syntax())
    }
}

impl SoqlComparison {
    pub fn field_name(&self) -> Option<SoqlFieldName> {
        support::child(self.syntax())
    }

    pub fn function(&self) -> Option<SoqlFunction> {
        support::child(self.syntax())
    }

    /// 1-2 tokens (`=`, or `<`/`>` plus a merged `=`, `NOT`+`IN`, ...).
    pub fn operator_tokens(&self) -> Vec<SyntaxToken> {
        direct_tokens(self.syntax()).collect()
    }

    pub fn value(&self) -> Option<SoqlValue> {
        support::child(self.syntax())
    }
}

impl SoqlValue {
    /// The leading token for any of the single-token/token-prefixed
    /// shapes (a literal, a date formula, the sign of a signed number,
    /// an `IntegralCurrencyLiteral`, ...) -- `None` for the
    /// `boundExpression`/subquery/`valueList` shapes, which are child
    /// nodes instead (see [`Self::bound_expr`]/[`Self::sub_query`]/
    /// [`Self::value_list`]).
    pub fn literal_token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    pub fn bound_expr(&self) -> Option<SoqlBoundExpr> {
        support::child(self.syntax())
    }

    pub fn sub_query(&self) -> Option<SoqlSubQuery> {
        support::child(self.syntax())
    }

    pub fn value_list(&self) -> Option<SoqlValueList> {
        support::child(self.syntax())
    }
}

impl SoqlValueList {
    pub fn values(&self) -> AstChildren<SoqlValue> {
        support::children(self.syntax())
    }
}

impl SoqlBoundExpr {
    pub fn expr(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl SoqlWithClause {
    pub fn filtering_expr(&self) -> Option<SoqlFilteringExpr> {
        support::child(self.syntax())
    }

    pub fn condition(&self) -> Option<SoqlLogicalExpr> {
        support::child(self.syntax())
    }

    pub fn is_security_enforced(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::SecurityEnforced).is_some()
    }

    pub fn is_system_mode(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::SystemMode).is_some()
    }

    pub fn is_user_mode(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::UserMode).is_some()
    }
}

impl SoqlFilteringExpr {
    pub fn selections(&self) -> AstChildren<SoqlDataCategorySelection> {
        support::children(self.syntax())
    }
}

impl SoqlGroupBy {
    pub fn is_rollup(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Rollup).is_some()
    }

    pub fn is_cube(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Cube).is_some()
    }

    pub fn fields(&self) -> AstChildren<SoqlFieldOrFunction> {
        support::children(self.syntax())
    }

    pub fn having(&self) -> Option<SoqlLogicalExpr> {
        support::child(self.syntax())
    }
}

impl SoqlOrderBy {
    pub fn field_orders(&self) -> AstChildren<SoqlFieldOrder> {
        support::children(self.syntax())
    }
}

impl SoqlFieldOrder {
    pub fn target(&self) -> Option<SoqlFieldOrFunction> {
        support::child(self.syntax())
    }

    pub fn is_ascending(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Asc).is_some()
    }

    pub fn is_descending(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Desc).is_some()
    }

    pub fn nulls_first(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::First).is_some()
    }

    pub fn nulls_last(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Last).is_some()
    }
}

impl SoqlLimit {
    /// `None` when the limit is a [`SoqlBoundExpr`] (`LIMIT :n`) instead
    /// of a literal.
    pub fn count_token(&self) -> Option<SyntaxToken> {
        direct_tokens(self.syntax()).nth(1)
    }

    pub fn bound_expr(&self) -> Option<SoqlBoundExpr> {
        support::child(self.syntax())
    }
}

impl SoqlOffset {
    pub fn count_token(&self) -> Option<SyntaxToken> {
        direct_tokens(self.syntax()).nth(1)
    }

    pub fn bound_expr(&self) -> Option<SoqlBoundExpr> {
        support::child(self.syntax())
    }
}

impl SoqlForClause {
    /// `VIEW`, `UPDATE`, or `REFERENCE`.
    pub fn kind_token(&self) -> Option<SyntaxToken> {
        direct_tokens(self.syntax()).nth(1)
    }
}

impl SoqlUpdateList {
    /// `TRACKING`/`VIEWSTAT`, one or both.
    pub fn update_types(&self) -> Vec<SyntaxToken> {
        direct_tokens(self.syntax())
            .filter(|t| t.kind() != SyntaxKind::Comma)
            .collect()
    }
}

impl SoqlFunction {
    pub fn name_token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }

    pub fn field_name(&self) -> Option<SoqlFieldName> {
        support::child(self.syntax())
    }

    /// The `FORMAT(soqlFunction)` nested-call form.
    pub fn nested_function(&self) -> Option<SoqlFunction> {
        support::child(self.syntax())
    }
}

impl SoqlTypeOf {
    pub fn field_name(&self) -> Option<SoqlFieldName> {
        support::child(self.syntax())
    }

    pub fn when_clauses(&self) -> AstChildren<SoqlWhenClause> {
        support::children(self.syntax())
    }

    pub fn else_clause(&self) -> Option<SoqlElseClause> {
        support::child(self.syntax())
    }
}

impl SoqlWhenClause {
    pub fn field_name(&self) -> Option<SoqlFieldName> {
        support::child(self.syntax())
    }

    pub fn then_fields(&self) -> Option<SoqlFieldNameList> {
        support::child(self.syntax())
    }
}

impl SoqlElseClause {
    pub fn fields(&self) -> Option<SoqlFieldNameList> {
        support::child(self.syntax())
    }
}

impl SoqlFieldNameList {
    pub fn fields(&self) -> AstChildren<SoqlFieldName> {
        support::children(self.syntax())
    }
}

// ==== SOSL ====

impl SoslExpr {
    /// The `FindLiteral`/`FindLiteralAlt` form (`[find '...' ...]`).
    /// `None` for the `[FIND :boundExpr ...]` form -- see
    /// [`Self::bound_expr`].
    pub fn find_literal(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax()).filter(|t| {
            matches!(
                t.kind(),
                SyntaxKind::FindLiteral | SyntaxKind::FindLiteralAlt
            )
        })
    }

    pub fn bound_expr(&self) -> Option<SoqlBoundExpr> {
        support::child(self.syntax())
    }

    pub fn clauses(&self) -> Option<SoslClauses> {
        support::child(self.syntax())
    }
}

impl SoslClauses {
    pub fn search_group(&self) -> Option<SoslSearchGroup> {
        support::child(self.syntax())
    }

    pub fn field_spec_list(&self) -> Option<SoslFieldSpecList> {
        support::child(self.syntax())
    }

    pub fn with_clauses(&self) -> AstChildren<SoslWithClause> {
        support::children(self.syntax())
    }

    pub fn limit(&self) -> Option<SoqlLimit> {
        support::child(self.syntax())
    }

    pub fn update_list(&self) -> Option<SoqlUpdateList> {
        support::child(self.syntax())
    }
}

impl SoslSearchGroup {
    /// `ALL`/`EMAIL`/`NAME`/`PHONE`/`SIDEBAR`.
    pub fn group_token(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }
}

impl SoslFieldSpecList {
    pub fn specs(&self) -> AstChildren<SoslFieldSpec> {
        support::children(self.syntax())
    }
}

impl SoslFieldSpec {
    pub fn object(&self) -> Option<SoqlFieldName> {
        support::children(self.syntax()).next()
    }

    pub fn field_list(&self) -> Option<SoslFieldList> {
        support::child(self.syntax())
    }

    pub fn where_clause(&self) -> Option<SoqlWhereClause> {
        support::child(self.syntax())
    }

    /// The `USING LISTVIEW = <name>` target, if present.
    pub fn using_listview(&self) -> Option<SoqlFieldName> {
        support::children::<SoqlFieldName>(self.syntax()).nth(1)
    }

    pub fn order_by(&self) -> Option<SoqlOrderBy> {
        support::child(self.syntax())
    }

    pub fn limit(&self) -> Option<SoqlLimit> {
        support::child(self.syntax())
    }

    pub fn offset(&self) -> Option<SoqlOffset> {
        support::child(self.syntax())
    }
}

impl SoslFieldList {
    pub fn fields(&self) -> AstChildren<SoqlFieldName> {
        support::children(self.syntax())
    }

    /// The `FORMAT(soqlFunction)` sub-case.
    pub fn function(&self) -> Option<SoqlFunction> {
        support::child(self.syntax())
    }
}

impl SoslWithClause {
    /// `DIVISION`/`DATA`/`SNIPPET`/`NETWORK`/`PRICEBOOKID`/`METADATA`/
    /// `HIGHLIGHT`/`USER_MODE`/`SYSTEM_MODE`/`SPELL_CORRECTION`. Skips the
    /// leading `WITH` token itself (index 0), same as the sibling
    /// `SoqlForClause::kind_token`'s `nth(1)`.
    pub fn kind_token(&self) -> Option<SyntaxToken> {
        direct_tokens(self.syntax()).nth(1)
    }

    pub fn bound_expr(&self) -> Option<SoqlBoundExpr> {
        support::child(self.syntax())
    }

    pub fn filtering_expr(&self) -> Option<SoqlFilteringExpr> {
        support::child(self.syntax())
    }

    pub fn network_list(&self) -> Option<SoslNetworkList> {
        support::child(self.syntax())
    }
}

impl SoslNetworkList {
    pub fn values(&self) -> Vec<SyntaxToken> {
        direct_tokens(self.syntax())
            .filter(|t| t.kind() != SyntaxKind::Comma)
            .collect()
    }
}
