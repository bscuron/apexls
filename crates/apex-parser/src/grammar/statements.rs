//! Statement grammar, matching the reference grammar's actual 19-way
//! `statement` alternative (not a simplification of it): besides the
//! familiar control-flow forms, this includes Apex's own `switch on`
//! (distinct from Java's `switch`), the six DML statements
//! (`insert`/`update`/`delete`/`undelete`/`upsert`/`merge`), and
//! `System.runAs(...) { }`. There is deliberately no general bare-`;`
//! empty statement -- the reference grammar only allows that as the body
//! of `for`/`while` specifically, not as a statement in general.
//!
//! The one genuine backtracking case: statement-start `Identifier ...` is
//! ambiguous between a local variable declaration (`Foo x = ...;`) and an
//! expression statement (`foo.bar();`) without parsing past the whole
//! leading type. `List`/`Map`/`Set`-led statements have no such
//! ambiguity (those tokens can never start a bare expression), so they
//! dispatch directly.

use crate::parser::{CompletedMarker, Parser};
use apex_syntax::SyntaxKind;

pub(crate) fn statement(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let s = match p.current() {
        SyntaxKind::LBrace => block(p),
        SyntaxKind::If => if_stmt(p),
        SyntaxKind::Switch => switch_stmt(p),
        SyntaxKind::For => for_stmt(p),
        SyntaxKind::While => while_stmt(p),
        SyntaxKind::Do => do_while_stmt(p),
        SyntaxKind::Try => try_stmt(p),
        SyntaxKind::Return => return_stmt(p),
        SyntaxKind::Throw => throw_stmt(p),
        SyntaxKind::Break => break_stmt(p),
        SyntaxKind::Continue => continue_stmt(p),
        SyntaxKind::Insert => dml_stmt(p, SyntaxKind::InsertStmt),
        SyntaxKind::Update => dml_stmt(p, SyntaxKind::UpdateStmt),
        SyntaxKind::Delete => dml_stmt(p, SyntaxKind::DeleteStmt),
        SyntaxKind::Undelete => dml_stmt(p, SyntaxKind::UndeleteStmt),
        SyntaxKind::Upsert => upsert_stmt(p),
        SyntaxKind::Merge => merge_stmt(p),
        SyntaxKind::SystemRunAs => runas_stmt(p),
        SyntaxKind::List | SyntaxKind::Map | SyntaxKind::Set => local_var_decl_stmt(p),
        SyntaxKind::Final | SyntaxKind::Transient => local_var_decl_stmt(p),
        // Exact keyword arms above always win ties (e.g. `Insert`/`Try`/
        // `For` are also `id`-shaped per the reference grammar, but Rust
        // match already resolved those via the earlier exact arms), so
        // this only ever catches identifiers plus the many SOQL/SOSL/DML
        // keywords that double as ordinary names (`System`, `Name`, ...).
        k if super::ids::is_id_kind(k) => local_var_decl_or_expr_stmt(p),
        _ => expr_stmt(p),
    };
    Some(s)
}

pub(crate) fn block(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    p.parse_list(
        |p| p.at(SyntaxKind::RBrace),
        |p| {
            statement(p);
        },
    );
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::Block)
}

fn par_expr(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::LParen);
    super::expressions::expr(p);
    p.expect(SyntaxKind::RParen);
}

fn if_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // if
    par_expr(p);
    statement(p);
    if p.at(SyntaxKind::Else) {
        p.bump();
        statement(p);
    }
    m.complete(p, SyntaxKind::IfStmt)
}

// ---- switch on ... { when ... } ----

fn switch_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // switch
    p.expect(SyntaxKind::On);
    super::expressions::expr(p);
    p.expect(SyntaxKind::LBrace);
    p.parse_list(
        |p| p.at(SyntaxKind::RBrace),
        |p| {
            when_control(p);
        },
    );
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::SwitchStmt)
}

fn when_control(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::When);
    when_value(p);
    block(p);
    m.complete(p, SyntaxKind::WhenClause)
}

/// `ELSE | whenLiteral (',' whenLiteral)* | typeRef id`. The type-pattern
/// alternative is recognized via a one-token-lookahead heuristic (a type
/// name immediately followed by another identifier) rather than full
/// backtracking: this correctly handles the common single-segment case
/// (`when Account a`) but not a *dotted* type pattern (`when Outer.Inner
/// x`), which falls through to the `qualifiedName` branch instead and
/// mis-parses. Accepted as a known Phase 2 gap -- dotted switch-on-type
/// patterns are rare in practice, and Phase 2's fragment round-trip test
/// will surface any real instance as an expected-fail rather than silent
/// corruption.
fn when_value(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(SyntaxKind::Else) {
        p.bump();
    } else if super::types::at_type_start(p) && super::ids::is_id_kind(p.nth(1)) {
        super::types::type_ref(p);
        super::ids::expect_id(p);
    } else {
        when_literal(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            when_literal(p);
        }
    }
    m.complete(p, SyntaxKind::WhenValue)
}

fn when_literal(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    match p.current() {
        SyntaxKind::LParen => {
            p.bump();
            when_literal(p);
            p.expect(SyntaxKind::RParen);
        }
        SyntaxKind::Add | SyntaxKind::Sub => {
            while matches!(p.current(), SyntaxKind::Add | SyntaxKind::Sub) {
                p.bump();
            }
            p.expect(SyntaxKind::IntegerLiteral);
        }
        SyntaxKind::IntegerLiteral
        | SyntaxKind::LongLiteral
        | SyntaxKind::StringLiteral
        | SyntaxKind::MultilineStringLiteral
        | SyntaxKind::Null => {
            p.bump();
        }
        k if super::ids::is_id_kind(k) => {
            super::types::qualified_name(p);
        }
        _ => p.error(format!("expected when-literal, found {:?}", p.current())),
    }
    m.complete(p, SyntaxKind::WhenLiteral)
}

// ---- for / while / do-while ----

fn for_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // for
    p.expect(SyntaxKind::LParen);

    let checkpoint = p.checkpoint();
    let is_each = try_enhanced_for_control(p);
    if !is_each {
        p.rollback(checkpoint);
        classic_for_control(p);
    }

    p.expect(SyntaxKind::RParen);
    if p.at(SyntaxKind::Semi) {
        p.bump();
    } else {
        statement(p);
    }
    m.complete(
        p,
        if is_each {
            SyntaxKind::ForEachStmt
        } else {
            SyntaxKind::ForStmt
        },
    )
}

/// `typeRef id ':' expression`.
fn try_enhanced_for_control(p: &mut Parser<'_>) -> bool {
    if !super::types::type_ref(p) {
        return false;
    }
    if !super::ids::at_id(p) {
        return false;
    }
    p.bump(); // id
    if !p.at(SyntaxKind::Colon) {
        return false;
    }
    p.bump(); // :
    super::expressions::expr(p);
    true
}

/// `forInit? ';' expression? ';' forUpdate?`.
fn classic_for_control(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::Semi) {
        for_init(p);
    }
    p.expect(SyntaxKind::Semi);
    if !p.at(SyntaxKind::Semi) {
        super::expressions::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    if !p.at(SyntaxKind::RParen) {
        for_update(p);
    }
}

/// `localVariableDeclaration | expressionList` -- the same
/// type-vs-expression ambiguity as top-level statement dispatch, resolved
/// the same way (speculative parse, roll back on mismatch).
fn for_init(p: &mut Parser<'_>) {
    let checkpoint = p.checkpoint();
    if try_for_init_decl(p).is_some() {
        return;
    }
    p.rollback(checkpoint);
    let m = p.start();
    expr_list(p);
    m.complete(p, SyntaxKind::ForInit);
}

fn try_for_init_decl(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let m = p.start();
    if !try_local_var_decl_core(p) {
        return None;
    }
    Some(m.complete(p, SyntaxKind::ForInit))
}

fn for_update(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    expr_list(p);
    m.complete(p, SyntaxKind::ForUpdate)
}

fn expr_list(p: &mut Parser<'_>) {
    super::expressions::expr(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        super::expressions::expr(p);
    }
}

fn while_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // while
    par_expr(p);
    if p.at(SyntaxKind::Semi) {
        p.bump();
    } else {
        statement(p);
    }
    m.complete(p, SyntaxKind::WhileStmt)
}

fn do_while_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // do
    block(p);
    p.expect(SyntaxKind::While);
    par_expr(p);
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::DoWhileStmt)
}

// ---- try / catch / finally ----

fn try_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // try
    block(p);
    let mut has_catch = false;
    while p.at(SyntaxKind::Catch) {
        has_catch = true;
        catch_clause(p);
    }
    if p.at(SyntaxKind::Finally) {
        finally_block(p);
    } else if !has_catch {
        p.error("expected 'catch' or 'finally' after 'try' block");
    }
    m.complete(p, SyntaxKind::TryStmt)
}

/// `catchClause: CATCH LPAREN modifier* qualifiedName id RPAREN block`.
/// The `modifier*` (e.g. `catch (final MyException e)`) is genuinely
/// valid Apex -- confirmed by compiling it against a real org via `sf
/// apex run`, not just present in the reference grammar -- even though
/// it's rare enough that it never appears anywhere in the NPSP corpus.
fn catch_clause(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // catch
    p.expect(SyntaxKind::LParen);
    super::declarations::modifiers(p);
    if !super::types::qualified_name(p) {
        p.error("expected exception type");
    }
    super::ids::expect_id(p);
    p.expect(SyntaxKind::RParen);
    block(p);
    m.complete(p, SyntaxKind::CatchClause)
}

fn finally_block(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // finally
    block(p);
    m.complete(p, SyntaxKind::FinallyClause)
}

// ---- return / throw / break / continue ----

fn return_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // return
    if !p.at(SyntaxKind::Semi) {
        super::expressions::expr(p);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ReturnStmt)
}

fn throw_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // throw
    super::expressions::expr(p);
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ThrowStmt)
}

fn break_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump();
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::BreakStmt)
}

fn continue_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump();
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ContinueStmt)
}

// ---- DML: insert / update / delete / undelete / upsert / merge, and
// System.runAs(...) { } ----

fn access_level(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::As) {
        return;
    }
    let m = p.start();
    p.bump(); // as
    if matches!(p.current(), SyntaxKind::System | SyntaxKind::User) {
        p.bump();
    } else {
        p.error("expected 'system' or 'user' after 'as'");
    }
    m.complete(p, SyntaxKind::AccessLevelClause);
}

fn dml_stmt(p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
    let m = p.start();
    p.bump(); // insert / update / delete / undelete
    access_level(p);
    super::expressions::expr(p);
    p.expect(SyntaxKind::Semi);
    m.complete(p, kind)
}

fn upsert_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // upsert
    access_level(p);
    super::expressions::expr(p);
    super::types::qualified_name(p); // optional external ID field
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::UpsertStmt)
}

fn merge_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // merge
    access_level(p);
    super::expressions::expr(p);
    super::expressions::expr(p);
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::MergeStmt)
}

fn runas_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // System.runAs (the single SystemRunAs token)
    p.expect(SyntaxKind::LParen);
    if !p.at(SyntaxKind::RParen) {
        expr_list(p);
    }
    p.expect(SyntaxKind::RParen);
    block(p);
    m.complete(p, SyntaxKind::RunAsStmt)
}

// ---- local variable declaration / expression statement ----

fn local_var_decl_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if !try_local_var_decl_core(p) {
        p.error("expected a local variable declaration");
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::LocalVarDeclStmt)
}

fn local_var_decl_or_expr_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let checkpoint = p.checkpoint();
    if let Some(decl) = try_local_var_decl_stmt(p) {
        return decl;
    }
    p.rollback(checkpoint);
    expr_stmt(p)
}

fn try_local_var_decl_stmt(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let m = p.start();
    if !try_local_var_decl_core(p) {
        return None;
    }
    p.expect(SyntaxKind::Semi);
    Some(m.complete(p, SyntaxKind::LocalVarDeclStmt))
}

/// `modifier* typeRef variableDeclarators`, no trailing `;` (shared by
/// the statement form and `forInit`). Returns `false`, having consumed
/// only the type (and any leading modifiers), if what follows the type
/// doesn't look like a declarator name -- the signal callers use to roll
/// back to an expression-statement interpretation instead.
fn try_local_var_decl_core(p: &mut Parser<'_>) -> bool {
    while matches!(p.current(), SyntaxKind::Final | SyntaxKind::Transient) {
        p.bump();
    }
    if !super::types::type_ref(p) {
        return false;
    }
    if !super::ids::at_id(p) {
        return false;
    }
    var_declarators(p);
    true
}

/// Shared with `grammar::declarations`' field declarations, which have
/// the exact same `id (',' id ('=' expr)?)*` shape.
pub(crate) fn var_declarators(p: &mut Parser<'_>) {
    var_declarator(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        var_declarator(p);
    }
}

fn var_declarator(p: &mut Parser<'_>) {
    let m = p.start();
    super::ids::expect_id(p);
    if p.at(SyntaxKind::Assign) {
        p.bump();
        super::expressions::expr(p);
    }
    m.complete(p, SyntaxKind::VarDeclarator);
}

fn expr_stmt(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if super::expressions::expr(p).is_none() {
        // No expression at all here (e.g. an unrecognized token): skip
        // forward to the next plausible statement boundary -- a `;`
        // (consumed, since it plausibly ends the bad statement), a `}`
        // (left for the enclosing `block` to handle, so recovery doesn't
        // eat the closing brace of an outer scope), or another
        // statement-leading keyword (an anchor that also covers a
        // missing-semicolon case) -- rather than discarding just one
        // token and likely re-erroring on the next.
        p.recover_until(is_stmt_boundary);
        if p.at(SyntaxKind::Semi) {
            p.bump();
        }
        return m.complete(p, SyntaxKind::ExprStmt);
    }
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::ExprStmt)
}

fn is_stmt_boundary(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Semi
            | SyntaxKind::RBrace
            | SyntaxKind::If
            | SyntaxKind::Switch
            | SyntaxKind::For
            | SyntaxKind::While
            | SyntaxKind::Do
            | SyntaxKind::Try
            | SyntaxKind::Return
            | SyntaxKind::Throw
            | SyntaxKind::Break
            | SyntaxKind::Continue
            | SyntaxKind::Insert
            | SyntaxKind::Update
            | SyntaxKind::Delete
            | SyntaxKind::Undelete
            | SyntaxKind::Upsert
            | SyntaxKind::Merge
            | SyntaxKind::SystemRunAs
    )
}
