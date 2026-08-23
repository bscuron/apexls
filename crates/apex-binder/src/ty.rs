//! An expression's inferred static type -- what `Pass 2` (`crate::resolve`)
//! chains from one expression to the next as it walks a body, replacing
//! the earlier, narrower `Option<SymbolId>` (which could only ever mean
//! "a project-local class/interface/enum, or nothing at all").
//!
//! `Ty` is purely a walker-internal chaining value: it never appears in
//! `crate::reference_table::Resolution` (what a *reference* resolved to,
//! a separate and unaffected concern) or gets stored on `BoundProgram`.
//! It exists so the walker stops silently forgetting an expression's type
//! the instant that type isn't project-local -- a string literal, an
//! `Integer`, a `List<Account>` -- rather than only being able to
//! represent that once true type inference and generics were added.

use crate::symbol::SymbolId;
use std::borrow::Cow;

/// `System` deliberately covers two distinct "not project-local" cases
/// under one name, both honestly unable to resolve *members* in v1 (see
/// `BACKLOG.md`'s still-separate, still-unmodeled standard-library-type-
/// model gap): a genuinely unmodeled system/library type (`String`,
/// `Integer`, a bare `List`/`Map`/`Set` before `crate::generics` gets a
/// chance to model one of its methods) and a schema SObject type
/// (`Account`) that `crate::schema_index::SchemaIndex` knows exists but
/// has no member model for either. Conflating them loses nothing real:
/// `crate::reference_table::Resolution` (`SchemaObject` vs `Unresolved`)
/// already records the precise distinction separately; `Ty` only needs
/// "not project-local, but at least named."
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ty {
    /// A project-local class/interface/enum -- exactly what
    /// `Option<SymbolId>` used to be the whole story for.
    Project(SymbolId),
    /// `Cow`, not a plain `String`: a *literal*'s type (`Ty::for_literal`)
    /// and every hand-written name in `crate::generics` are always one of
    /// a handful of `&'static str`s, and literals in particular are by
    /// far the most common thing `bind_expr` types in a real project --
    /// `Cow::Borrowed` there costs nothing, keeping `String`'s real
    /// allocation only for names captured from actual dynamic source
    /// text (a declared type's own spelling, which isn't known until
    /// parse time and must be owned). A measured, not guessed, tradeoff:
    /// the first version of this type used a plain `String` throughout
    /// and cost a real, consistent ~15-17% slowdown across every
    /// `apex-binder` benchmark -- every literal in the whole project
    /// walks through `Ty::for_literal`, so that allocation is squarely
    /// on the hottest path in the crate.
    ///
    /// `args`: `name`'s type arguments, if any (`List<Account>` ->
    /// `args: [Ty::Project(Account)]`). Empty for a non-generic system
    /// type or when the argument itself couldn't be resolved to anything
    /// more specific than a bare name.
    System {
        name: Cow<'static, str>,
        args: Vec<Ty>,
    },
}

impl Ty {
    /// For a compile-time-known system type name -- a literal's type, or
    /// one of `crate::generics`'s hand-written method names -- allocation-
    /// free, see [`Self::System`]'s doc comment for why that matters.
    pub(crate) fn system(name: &'static str) -> Ty {
        Ty::System {
            name: Cow::Borrowed(name),
            args: Vec::new(),
        }
    }

    /// Like [`Self::system`], but with type arguments already known (a
    /// hand-modeled `crate::generics` result like `Map.keySet()` ->
    /// `Set<K>`).
    pub(crate) fn system_with_args(name: &'static str, args: Vec<Ty>) -> Ty {
        Ty::System {
            name: Cow::Borrowed(name),
            args,
        }
    }

    /// For a type name captured from real, dynamic source text (a
    /// declared type's own spelling) -- necessarily owned, since it
    /// isn't known until parse time.
    pub(crate) fn system_owned(name: String, args: Vec<Ty>) -> Ty {
        Ty::System {
            name: Cow::Owned(name),
            args,
        }
    }

    pub(crate) fn boolean() -> Ty {
        Ty::system("Boolean")
    }

    /// The system type name a literal token's `SyntaxKind` denotes, if
    /// it's one of the kinds a general expression `LiteralExpr` can
    /// actually wrap (see `apex_parser::grammar::expressions::is_literal_kind`)
    /// -- `null` (`SyntaxKind::Null`, not a `*Literal` token at all)
    /// carries no useful type of its own (compatible with everything),
    /// so it stays `None` rather than inventing one.
    pub(crate) fn for_literal(kind: apex_syntax::SyntaxKind) -> Option<Ty> {
        use apex_syntax::SyntaxKind;
        match kind {
            SyntaxKind::IntegerLiteral => Some(Ty::system("Integer")),
            SyntaxKind::LongLiteral => Some(Ty::system("Long")),
            // Apex's own default for a plain numeric literal with a
            // decimal point is `Decimal`, not `Double` -- a real
            // simplification (a literal written in scientific notation,
            // or explicitly cast, can still genuinely be a `Double`) but
            // the right default absent any further context.
            SyntaxKind::NumberLiteral => Some(Ty::system("Decimal")),
            SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral => {
                Some(Ty::system("String"))
            }
            SyntaxKind::BooleanLiteral => Some(Ty::boolean()),
            _ => None,
        }
    }

    /// The result type of a binary expression whose operator spells out
    /// to `op_text` (`BinExpr::operator_tokens()`'s concatenated
    /// `.text()`, since some operators -- merged relational-with-equals,
    /// shifts -- are more than one token). Comparison/logical operators
    /// always produce `Boolean`, regardless of operand types. Arithmetic/
    /// bitwise/shift operators and plain/compound assignment propagate an
    /// operand's own type -- exactly correct for numeric arithmetic and
    /// assignment, a reasonable best-effort guess for anything else
    /// (string concatenation via `+` propagates `String`, correctly) --
    /// never fabricated when neither operand's type is known.
    pub(crate) fn for_bin_op(op_text: &str, lhs: Option<Ty>, rhs: Option<Ty>) -> Option<Ty> {
        match op_text {
            "<" | ">" | "<=" | ">=" | "==" | "!=" | "===" | "!==" | "<>" | "&&" | "||" => {
                Some(Ty::boolean())
            }
            "+" | "-" | "*" | "/" | "&" | "|" | "^" | "<<" | ">>" | ">>>" | "??" => lhs.or(rhs),
            "=" | "+=" | "-=" | "*=" | "/=" | "&=" | "|=" | "^=" | "<<=" | ">>=" | ">>>=" => lhs,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_kinds_get_their_real_system_types() {
        assert_eq!(
            Ty::for_literal(apex_syntax::SyntaxKind::IntegerLiteral),
            Some(Ty::system("Integer"))
        );
        assert_eq!(
            Ty::for_literal(apex_syntax::SyntaxKind::StringLiteral),
            Some(Ty::system("String"))
        );
        assert_eq!(
            Ty::for_literal(apex_syntax::SyntaxKind::BooleanLiteral),
            Some(Ty::boolean())
        );
        // `null` has no informative type of its own -- compatible with
        // everything, so inventing one would be worse than `None`.
        assert_eq!(Ty::for_literal(apex_syntax::SyntaxKind::Null), None);
    }

    #[test]
    fn comparison_and_logical_operators_always_produce_boolean() {
        let int_ty = Some(Ty::system("Integer"));
        assert_eq!(
            Ty::for_bin_op("<", int_ty.clone(), int_ty.clone()),
            Some(Ty::boolean())
        );
        assert_eq!(
            Ty::for_bin_op("&&", Some(Ty::boolean()), Some(Ty::boolean())),
            Some(Ty::boolean())
        );
    }

    #[test]
    fn arithmetic_operators_propagate_an_operand_type() {
        let string_ty = Some(Ty::system("String"));
        assert_eq!(Ty::for_bin_op("+", string_ty.clone(), None), string_ty);
        assert_eq!(Ty::for_bin_op("+", None, string_ty.clone()), string_ty);
        assert_eq!(Ty::for_bin_op("+", None, None), None);
    }

    #[test]
    fn shift_and_merged_relational_operators_are_recognized_by_full_text() {
        // `<<`/`>>=`/etc. are multiple tokens in the tree (see
        // `BinExpr::operator_tokens`'s doc comment) -- this only ever
        // sees the already-concatenated text, so this just checks the
        // classification itself, not the concatenation.
        let int_ty = Some(Ty::system("Integer"));
        assert_eq!(
            Ty::for_bin_op("<<", int_ty.clone(), int_ty.clone()),
            int_ty.clone()
        );
        assert_eq!(
            Ty::for_bin_op(">=", int_ty.clone(), int_ty),
            Some(Ty::boolean())
        );
    }
}
