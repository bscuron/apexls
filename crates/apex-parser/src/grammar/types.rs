//! Expression/statement-scoped `Type` grammar, matching the reference
//! grammar's `typeRef`/`typeName`/`typeArguments` shape (not a
//! simplification of it): each dotted segment can carry its own type
//! arguments (`Outer<T>.Inner<U>` is legal, matching Java-derived nesting),
//! and array suffixes trail the whole qualified name.
//!
//! ```text
//! Type         := TypeName ('.' TypeName)* ('[' ']')*
//! TypeName     := (Identifier | List | Map | Set) TypeArgList?
//! TypeArgList  := '<' Type (',' Type)* '>'
//! ```
//!
//! Declaration-level types (class-level generics, bounds) are Phase 3.
//! Also reused as-is for `new` targets / `creator` names (Phase 2's
//! `grammar::expressions`), since `createdName` has the same dotted,
//! per-segment-generic shape as `typeRef`.

use crate::parser::Parser;
use apex_syntax::SyntaxKind;

/// Does the current token start a `Type`? Callers (statement dispatch,
/// cast-vs-paren disambiguation) use this before committing to a
/// speculative `type_ref` attempt.
pub(crate) fn at_type_start(p: &Parser<'_>) -> bool {
    at_type_name_start(p, 0)
}

fn at_type_name_start(p: &Parser<'_>, n: usize) -> bool {
    matches!(
        p.nth(n),
        SyntaxKind::Identifier | SyntaxKind::List | SyntaxKind::Map | SyntaxKind::Set
    )
}

/// Parses a `Type` if one starts here; leaves the cursor untouched and
/// returns `false` otherwise (nothing consumed, so callers can fall back
/// to a different interpretation without a rollback).
pub(crate) fn type_ref(p: &mut Parser<'_>) -> bool {
    if !at_type_start(p) {
        return false;
    }

    let m = p.start();
    type_name(p);
    while p.at(SyntaxKind::Dot) && at_type_name_start(p, 1) {
        p.bump(); // .
        type_name(p);
    }
    while p.at(SyntaxKind::LBrack) && p.nth(1) == SyntaxKind::RBrack {
        p.bump(); // [
        p.bump(); // ]
    }
    m.complete(p, SyntaxKind::Type);
    true
}

/// `(Identifier | List | Map | Set) TypeArgList?` -- no node of its own;
/// its tokens are direct children of the enclosing `Type`.
fn type_name(p: &mut Parser<'_>) {
    p.bump(); // caller (type_ref's loop condition) already verified this
    if p.at(SyntaxKind::Lt) {
        type_arg_list(p);
    }
}

fn type_arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(); // <
    if type_ref(p) {
        while p.at(SyntaxKind::Comma) {
            p.bump();
            type_ref(p);
        }
    }
    expect_close_angle(p);
    m.complete(p, SyntaxKind::TypeArgList);
}

/// Closes a `TypeArgList`. Always consumes exactly one `Gt` token --
/// nested generics like `List<List<Integer>>` close correctly because the
/// lexer already emits two independent `Gt` tokens, never a glued `>>`.
/// This must never attempt the shift-operator merge that
/// `grammar::expressions` does when parsing at expression precedence;
/// that ambiguity is simply unreachable from here.
fn expect_close_angle(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::Gt);
}

/// `QualifiedName := Identifier ('.' Identifier)*` -- simpler than `Type`
/// (no type arguments, no array suffixes). Used where the reference
/// grammar calls for a `qualifiedName` rather than a `typeRef`: catch
/// clause exception types, `whenValue`'s bare-enum-constant form, and
/// `upsert`'s optional external-ID field reference.
pub(crate) fn qualified_name(p: &mut Parser<'_>) -> bool {
    if !p.at(SyntaxKind::Identifier) {
        return false;
    }
    let m = p.start();
    p.bump();
    while p.at(SyntaxKind::Dot) && p.nth(1) == SyntaxKind::Identifier {
        p.bump();
        p.bump();
    }
    m.complete(p, SyntaxKind::QualifiedName);
    true
}
