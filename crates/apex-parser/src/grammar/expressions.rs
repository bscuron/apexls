//! Precedence-climbing expression parser, corrected against the reference
//! grammar's actual `expression` rule (`BaseApexParser.g4`) rather than
//! assumed Java-family precedence -- notably: shift binds *tighter* than
//! relational, `instanceof` is its own level between relational and
//! equality, and `??` is left-associative and distinct from the
//! (right-associative) ternary.
//!
//! Loosest to tightest: assign (right) -> ternary `?:` (right) -> `??`
//! (left) -> `||` -> `&&` -> `|` -> `^` -> `&` -> equality -> `instanceof`
//! -> relational (`<`,`>` + merged `=`) -> shift (merged only) -> additive
//! -> multiplicative -> unary prefix/postfix/primary-chain.
//!
//! Operator merging (`<=`/`>=`, shift) checks the *next significant*
//! token, not raw-byte adjacency -- matching ANTLR, where hidden-channel
//! trivia is invisible to parser rules, so `x > /* c */ = y` is `>=` to
//! the reference parser too.

use crate::parser::{CompletedMarker, Parser};
use apex_syntax::SyntaxKind;

pub(crate) fn expr(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    expr_assign(p)
}

// ---- assignment / ternary / instanceof / relational / shift: each has
// RHS handling too different from the others for the shared macro below ----

fn expr_assign(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let lhs = expr_ternary(p)?;
    if is_assign_op(p.current()) {
        let m = lhs.precede(p);
        p.bump();
        expr_assign(p); // right-assoc
        return Some(m.complete(p, SyntaxKind::BinExpr));
    }
    Some(lhs)
}

fn is_assign_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Assign
            | SyntaxKind::AddAssign
            | SyntaxKind::SubAssign
            | SyntaxKind::MulAssign
            | SyntaxKind::DivAssign
            | SyntaxKind::AndAssign
            | SyntaxKind::OrAssign
            | SyntaxKind::XorAssign
            | SyntaxKind::LShiftAssign
            | SyntaxKind::RShiftAssign
            | SyntaxKind::URShiftAssign
    )
}

fn expr_ternary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let cond = expr_coalesce(p)?;
    if p.at(SyntaxKind::Question) {
        let m = cond.precede(p);
        p.bump();
        expr_assign(p); // then-branch: unrestricted, delimited by ':'
        p.expect(SyntaxKind::Colon);
        expr_assign(p); // else-branch: right-assoc, so nested ternaries chain
        return Some(m.complete(p, SyntaxKind::TernaryExpr));
    }
    Some(cond)
}

fn expr_instanceof(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut e = expr_relational(p)?;
    while p.at(SyntaxKind::Instanceof) {
        let m = e.precede(p);
        p.bump();
        if !super::types::type_ref(p) {
            p.error("expected type after 'instanceof'");
        }
        e = m.complete(p, SyntaxKind::InstanceofExpr);
    }
    Some(e)
}

fn expr_relational(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = expr_shift(p)?;
    while matches!(p.current(), SyntaxKind::Lt | SyntaxKind::Gt) {
        let m = lhs.precede(p);
        p.bump(); // < or >
        if p.at(SyntaxKind::Assign) {
            p.bump(); // merges into <= / >=
        }
        expr_shift(p);
        lhs = m.complete(p, SyntaxKind::BinExpr);
    }
    Some(lhs)
}

fn expr_shift(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = expr_additive(p)?;
    loop {
        // >>> checked before >>, so a run of 3 doesn't get mis-split.
        let n = match p.current() {
            SyntaxKind::Lt if p.nth(1) == SyntaxKind::Lt => 2,
            SyntaxKind::Gt if p.nth(1) == SyntaxKind::Gt && p.nth(2) == SyntaxKind::Gt => 3,
            SyntaxKind::Gt if p.nth(1) == SyntaxKind::Gt => 2,
            _ => break,
        };
        let m = lhs.precede(p);
        for _ in 0..n {
            p.bump();
        }
        expr_additive(p);
        lhs = m.complete(p, SyntaxKind::BinExpr);
    }
    Some(lhs)
}

// ---- the plain left-associative binary levels ----

macro_rules! left_assoc_level {
    ($name:ident, $next:ident, $at_op:expr) => {
        fn $name(p: &mut Parser<'_>) -> Option<CompletedMarker> {
            let mut lhs = $next(p)?;
            while $at_op(p.current()) {
                let m = lhs.precede(p);
                p.bump();
                $next(p);
                lhs = m.complete(p, SyntaxKind::BinExpr);
            }
            Some(lhs)
        }
    };
}

left_assoc_level!(expr_coalesce, expr_or, |k| k == SyntaxKind::Coal);
left_assoc_level!(expr_or, expr_and, |k| k == SyntaxKind::Or);
left_assoc_level!(expr_and, expr_bitor, |k| k == SyntaxKind::And);
left_assoc_level!(expr_bitor, expr_bitxor, |k| k == SyntaxKind::BitOr);
left_assoc_level!(expr_bitxor, expr_bitand, |k| k == SyntaxKind::Caret);
left_assoc_level!(expr_bitand, expr_equality, |k| k == SyntaxKind::BitAnd);
left_assoc_level!(expr_equality, expr_instanceof, |k: SyntaxKind| matches!(
    k,
    SyntaxKind::Equal
        | SyntaxKind::NotEqual
        | SyntaxKind::TripleEqual
        | SyntaxKind::TripleNotEqual
        | SyntaxKind::LessAndGreater
));
left_assoc_level!(
    expr_additive,
    expr_multiplicative,
    |k: SyntaxKind| matches!(k, SyntaxKind::Add | SyntaxKind::Sub)
);
left_assoc_level!(expr_multiplicative, expr_unary, |k: SyntaxKind| matches!(
    k,
    SyntaxKind::Mul | SyntaxKind::Div
));

// ---- unary (prefix chain + postfix), primary, and the postfix chain ----

/// Prefix operators (`!`,`~`,`+`,`-`,`++`,`--`) recurse into themselves,
/// matching how ANTLR's automatic left-recursion elimination treats
/// `postOpExpression`/`preOpExpression`/`negExpression` as one combined
/// tier: chains like `-!x` or `--++x` are built by repeated self-calls,
/// not by stepping through separate levels one at a time.
fn expr_unary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if matches!(
        p.current(),
        SyntaxKind::Bang
            | SyntaxKind::Tilde
            | SyntaxKind::Add
            | SyntaxKind::Sub
            | SyntaxKind::Inc
            | SyntaxKind::Dec
    ) {
        let m = p.start();
        p.bump();
        expr_unary(p);
        return Some(m.complete(p, SyntaxKind::UnaryExpr));
    }

    let mut e = expr_primary_chain(p)?;
    while matches!(p.current(), SyntaxKind::Inc | SyntaxKind::Dec) {
        let m = e.precede(p);
        p.bump();
        e = m.complete(p, SyntaxKind::PostfixExpr);
    }
    Some(e)
}

/// `.`/`?.` (field access or, if followed by `(`, a method call) and
/// `[...]` indexing, chained after a primary. Bare calls (`foo(...)`,
/// no leading dot) and `this(...)`/`super(...)` are handled inside
/// `primary` itself, matching the reference grammar's separate
/// `methodCall` vs `dotExpression -> dotMethodCall` alternatives -- so
/// this loop never needs to special-case a bare `(`.
fn expr_primary_chain(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut e = primary(p)?;
    loop {
        e = match p.current() {
            SyntaxKind::Dot | SyntaxKind::QuestionDot => {
                let m = e.precede(p);
                p.bump();
                if at_member_name(p) {
                    p.bump();
                } else {
                    p.error("expected member name");
                }
                if p.at(SyntaxKind::LParen) {
                    arg_list(p);
                    m.complete(p, SyntaxKind::MethodCallExpr)
                } else {
                    m.complete(p, SyntaxKind::FieldExpr)
                }
            }
            SyntaxKind::LBrack => {
                let m = e.precede(p);
                p.bump();
                expr(p);
                p.expect(SyntaxKind::RBrack);
                m.complete(p, SyntaxKind::IndexExpr)
            }
            _ => break,
        };
    }
    Some(e)
}

/// Phase 2's minimal member-name set: a plain `Identifier`. The reference
/// grammar's `anyId` accepts a much wider set of contextual keywords here
/// (`obj.when`, `obj.get`, ...); real-world uses of that are expected to
/// show up as filtered-out, not-yet-supported fragments in the corpus
/// round-trip test rather than as a regression.
fn at_member_name(p: &Parser<'_>) -> bool {
    p.at(SyntaxKind::Identifier)
}

fn is_literal_kind(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::IntegerLiteral
            | SyntaxKind::LongLiteral
            | SyntaxKind::NumberLiteral
            | SyntaxKind::StringLiteral
            | SyntaxKind::MultilineStringLiteral
            | SyntaxKind::BooleanLiteral
            | SyntaxKind::Null
    )
}

fn primary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    match p.current() {
        SyntaxKind::This if p.nth(1) == SyntaxKind::LParen => {
            let m = p.start();
            p.bump();
            arg_list(p);
            Some(m.complete(p, SyntaxKind::CallExpr))
        }
        SyntaxKind::This => {
            let m = p.start();
            p.bump();
            Some(m.complete(p, SyntaxKind::ThisExpr))
        }
        SyntaxKind::Super if p.nth(1) == SyntaxKind::LParen => {
            let m = p.start();
            p.bump();
            arg_list(p);
            Some(m.complete(p, SyntaxKind::CallExpr))
        }
        SyntaxKind::Super => {
            let m = p.start();
            p.bump();
            Some(m.complete(p, SyntaxKind::SuperExpr))
        }
        SyntaxKind::New => Some(new_expr(p)),
        SyntaxKind::LParen => Some(paren_or_cast_expr(p)),
        SyntaxKind::Identifier if p.nth(1) == SyntaxKind::LParen => {
            let m = p.start();
            p.bump();
            arg_list(p);
            Some(m.complete(p, SyntaxKind::CallExpr))
        }
        SyntaxKind::Identifier => {
            let m = p.start();
            p.bump();
            Some(m.complete(p, SyntaxKind::NameExpr))
        }
        k if is_literal_kind(k) => {
            let m = p.start();
            p.bump();
            Some(m.complete(p, SyntaxKind::LiteralExpr))
        }
        _ => {
            p.error(format!("expected expression, found {:?}", p.current()));
            None
        }
    }
}

fn arg_list(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::LParen);
    if !p.at(SyntaxKind::RParen) {
        expr(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            expr(p);
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::ArgList)
}

/// `NEW creator`, i.e. `new` followed by a (possibly dotted, possibly
/// per-segment-generic) type name and exactly one of: constructor args
/// `(...)`, an array size/initializer `[...]`/`[]{...}`, or a brace
/// initializer (`{}`, a map `{k=>v,...}`, or a set/list `{a,b,...}`).
fn new_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // new
    if !super::types::type_ref(p) {
        p.error("expected a type after 'new'");
    }
    match p.current() {
        SyntaxKind::LParen => {
            arg_list(p);
        }
        SyntaxKind::LBrack => {
            p.bump();
            if p.at(SyntaxKind::RBrack) {
                p.bump();
                if p.at(SyntaxKind::LBrace) {
                    array_initializer(p);
                }
            } else {
                expr(p);
                p.expect(SyntaxKind::RBrack);
            }
        }
        SyntaxKind::LBrace => brace_init(p),
        _ => p.error("expected constructor arguments, array size, or initializer after 'new' type"),
    }
    m.complete(p, SyntaxKind::NewExpr)
}

fn array_initializer(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // {
    if !p.at(SyntaxKind::RBrace) {
        expr(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            if p.at(SyntaxKind::RBrace) {
                break; // trailing comma
            }
            expr(p);
        }
    }
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::ArrayInitializer)
}

/// `{}` (empty), `{k=>v, ...}` (map), or `{a, b, ...}` (set/list) --
/// disambiguated without backtracking: the first element is always a
/// plain expression, and whichever separator follows it (`=>` or not)
/// decides which of the two continuation grammars applies.
fn brace_init(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(); // {
    if p.at(SyntaxKind::RBrace) {
        p.bump();
        m.complete(p, SyntaxKind::ArrayInitializer); // empty: shape doesn't matter yet
        return;
    }

    let first_key = expr(p);
    if p.at(SyntaxKind::MapTo) {
        map_pair_continue(p, first_key);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            let key = expr(p);
            map_pair_continue(p, key);
        }
        p.expect(SyntaxKind::RBrace);
        m.complete(p, SyntaxKind::MapInitializer);
    } else {
        while p.at(SyntaxKind::Comma) {
            p.bump();
            expr(p);
        }
        p.expect(SyntaxKind::RBrace);
        m.complete(p, SyntaxKind::SetInitializer);
    }
}

/// Wraps an already-parsed key expression (or, if it failed, just parses
/// fresh) plus a `=> value` into a `MapEntry`.
fn map_pair_continue(p: &mut Parser<'_>, key: Option<CompletedMarker>) {
    match key {
        Some(key) => {
            let m = key.precede(p);
            p.expect(SyntaxKind::MapTo);
            expr(p);
            m.complete(p, SyntaxKind::MapEntry);
        }
        None => {
            p.expect(SyntaxKind::MapTo);
            expr(p);
        }
    }
}

/// `(Foo) x` cast vs `(x)` parenthesized value -- genuinely ambiguous at
/// parse-start (the classic C-family problem), resolved by speculatively
/// parsing a `Type`, checking whether a `)` immediately followed by a
/// plausible expression-start comes next, and rolling back to plain
/// parenthesized parsing if not.
fn paren_or_cast_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let checkpoint = p.checkpoint();
    if let Some(cast) = try_cast(p) {
        return cast;
    }
    p.rollback(checkpoint);
    paren_expr_body(p)
}

fn try_cast(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let m = p.start();
    p.bump(); // (
    if !super::types::at_type_start(p) {
        return None;
    }
    super::types::type_ref(p);
    if !(p.at(SyntaxKind::RParen) && at_cast_operand_start(p, 1)) {
        return None;
    }
    p.bump(); // )
    if expr_unary(p).is_none() {
        p.error("expected expression after cast");
    }
    Some(m.complete(p, SyntaxKind::CastExpr))
}

fn paren_expr_body(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.expect(SyntaxKind::LParen);
    expr(p);
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::ParenExpr)
}

/// Tokens that unambiguously start a cast's operand. Deliberately
/// excludes `Add`/`Sub`/`Inc`/`Dec`: `(Foo) + x` is treated as a
/// parenthesized-value continuation (addition), not `cast(+x) to Foo`,
/// and likewise `(Foo)++` is treated as a parenthesized value followed by
/// *postfix* `++` (matching `expr_unary`'s own postfix-after-primary
/// handling), not a cast applied to a prefix-incremented operand --
/// caught by this exact confusion in `metamorphic_parens.rs`: wrapping
/// the `a` in `-a++ + !b` as `-(a)++ + !b` was mis-parsed as `-CastExpr(
/// (a), ++ + !b)` before `Inc`/`Dec` were excluded here too. Both are
/// documented simplifications versus ANTLR's full adaptive lookahead,
/// matching how this construct is actually used in practice -- a cast of
/// a pre-incremented value is vanishingly rare real-world Apex.
fn at_cast_operand_start(p: &Parser<'_>, n: usize) -> bool {
    let k = p.nth(n);
    is_literal_kind(k)
        || matches!(
            k,
            SyntaxKind::Bang
                | SyntaxKind::Tilde
                | SyntaxKind::This
                | SyntaxKind::Super
                | SyntaxKind::New
                | SyntaxKind::LParen
                | SyntaxKind::Identifier
                | SyntaxKind::List
                | SyntaxKind::Map
                | SyntaxKind::Set
        )
}
