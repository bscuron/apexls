//! Apex's implicit-conversion and overload-specificity rules for a small,
//! fixed set of "curated" system types: the numeric family (`Integer`/
//! `Long`/`Double`/`Decimal`), `Boolean`, `String`, `Object`, and the three
//! built-in generic collections (`List`/`Set`/`Map`).
//!
//! Deliberately bounded, and a different gap from `BACKLOG.md`'s still-open
//! "no standard-library type model" item: this isn't about modeling what
//! *methods* `String`/`List`/... expose (a large, ongoing-maintenance
//! surface that changes as Salesforce ships new APIs several times a
//! year), only whether a *value* of one type can be passed where another
//! is declared. That's a small, specification-level rule set that doesn't
//! change with the API surface, so it's tractable to hand-encode once --
//! verified empirically against a real connected org (`sf apex run`
//! anonymous-Apex probes, this project's established oracle practice for
//! disputed Apex semantics) rather than guessed, since a wrongly-encoded
//! elimination silently breaks a call that would really resolve fine,
//! which is a correctness bug, not just a missed narrowing.
//!
//! Two surprising results the org verification actually caught (both
//! opposite of an initial guess going in, which is exactly why this was
//! verified rather than assumed):
//! - `List<Object>`/`Map<_, Object>` accept *any* element/value type, the
//!   same way a bare `Object` parameter does -- Apex's collection generics
//!   are not invariant the way Java's are.
//! - That permissiveness isn't limited to `Object`: `List<Integer>`
//!   satisfies a `List<Long>`-only overload (numeric widening) and
//!   `List<Dog>` satisfies a `List<Animal>`-only overload (`extends`
//!   upcasting), both nested one level inside the collection. In other
//!   words, a collection's type argument follows *exactly* the same
//!   compatibility rule as a bare argument of that same type would, not a
//!   stricter exact-match-only rule -- which is why [`type_compatible`]
//!   recurses into itself for a collection's type argument instead of
//!   using a separate, narrower comparison.

use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use crate::ty::Ty;
use smol_str::SmolStr;

/// `Integer`(0) < `Long`(1) < `Double`(2) < `Decimal`(3) -- each widens
/// implicitly to any later type in this list, never the reverse. Verified
/// against a real org: an `Integer` widens to `Long` in preference to
/// `Decimal` when both are applicable, a `Double`-only overload is chosen
/// over a `Decimal`-only one for the same `Integer` argument, and a
/// `Decimal` argument does *not* satisfy an `Integer`-only parameter (a
/// real compile error).
fn numeric_rank(name: &str) -> Option<u8> {
    const ORDER: [&str; 4] = ["Integer", "Long", "Double", "Decimal"];
    ORDER
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .map(|i| i as u8)
}

/// `List`/`Set`/`Map`, canonically cased, or `None` for anything else.
fn collection_kind(name: &str) -> Option<&'static str> {
    ["List", "Set", "Map"]
        .into_iter()
        .find(|k| k.eq_ignore_ascii_case(name))
}

/// Every name this module has a rule for -- used only to decide whether
/// two *differing* names are a known, definite mismatch (`Some(false)`) or
/// genuinely uncharted territory (`None`, never eliminate).
fn is_curated(name: &str) -> bool {
    name.eq_ignore_ascii_case("String")
        || name.eq_ignore_ascii_case("Boolean")
        || numeric_rank(name).is_some()
        || collection_kind(name).is_some()
}

/// Can a value of type `arg` be passed where a parameter is declared with
/// type `param_name` (plus `param_args`, its own type arguments if it's a
/// generic collection)? `Some(false)` only for a *positively* known
/// incompatibility within this module's curated rules; `None` for
/// anything outside them (an unmodeled system type on either side, or a
/// project-local type this module has no rule for) -- callers must treat
/// `None` exactly like "can't prove wrong," i.e. never eliminate on it.
///
/// This subsumes the plain project-local-vs-project-local case
/// (`narrow_by_overload`'s original, still-exact rule: identical type, or
/// the argument's type `extends`/`implements` the parameter's) as one
/// branch, so callers no longer need a separate check for it.
pub(crate) fn type_compatible(
    table: &SymbolTable,
    param_name: &str,
    param_args: &[SmolStr],
    arg: &Ty,
) -> Option<bool> {
    if param_name.eq_ignore_ascii_case("Object") {
        return Some(true);
    }
    match arg {
        Ty::Project(arg_id) => project_arg_compatible(table, param_name, *arg_id),
        Ty::System {
            name: arg_name,
            args: arg_args,
        } => {
            if table.top_level(param_name).is_some() {
                // The parameter is project-local (a user class/interface),
                // the argument is a system value -- no defined conversion
                // either direction, and a builtin system type can never be
                // made to implement a user-defined interface either.
                return Some(false);
            }
            system_type_compatible(table, param_name, param_args, arg_name, arg_args)
        }
    }
}

fn project_arg_compatible(table: &SymbolTable, param_name: &str, arg_id: SymbolId) -> Option<bool> {
    if let Some(param_id) = table.top_level(param_name) {
        return Some(arg_id == param_id || table.inherited_chain(arg_id).contains(&param_id));
    }
    // The parameter's declared type isn't project-local. A project class
    // can never *be* one of Apex's sealed builtin leaf types (`String`,
    // `Integer`, ..., `List`, `Set`, `Map` are not open for user
    // extension), so that's a safe, definite elimination -- but an
    // unmodeled system *interface* name (`Comparable`, `Iterable`, ...) is
    // a different story: a project class legitimately *can* implement one
    // of those, and this module has no model of which ones, so that case
    // stays `None` rather than a guessed elimination.
    if is_curated(param_name) {
        Some(false)
    } else {
        None
    }
}

fn system_type_compatible(
    table: &SymbolTable,
    param_name: &str,
    param_args: &[SmolStr],
    arg_name: &str,
    arg_args: &[Ty],
) -> Option<bool> {
    if param_name.eq_ignore_ascii_case(arg_name) {
        return if collection_kind(param_name).is_some() {
            collection_args_compatible(table, param_args, arg_args)
        } else {
            Some(true)
        };
    }
    if let (Some(arg_rank), Some(param_rank)) = (numeric_rank(arg_name), numeric_rank(param_name)) {
        return Some(arg_rank <= param_rank);
    }
    if is_curated(param_name) && is_curated(arg_name) {
        // Different curated families with no widening relationship
        // between them (e.g. `String` vs `Boolean`, `List` vs `Set`, a
        // collection vs a scalar) -- a real, definite mismatch.
        return Some(false);
    }
    None
}

/// A collection parameter's type arguments against the argument's own,
/// verified nested exactly one level deep (Apex has no user-defined
/// generics and no generic-of-generic collections, so this never needs to
/// recurse further than [`type_compatible`]'s own one recursive call
/// already does). `None` (not `Some(true)`) whenever any position can't be
/// confirmed compatible, even if none is positively ruled out either --
/// "some positions unknown" must never look identical to "fully verified
/// compatible" to a caller deciding whether to *select* this candidate.
fn collection_args_compatible(
    table: &SymbolTable,
    param_args: &[SmolStr],
    arg_args: &[Ty],
) -> Option<bool> {
    if param_args.len() != arg_args.len() {
        return None;
    }
    let mut all_confirmed = true;
    for (p, a) in param_args.iter().zip(arg_args.iter()) {
        match type_compatible(table, p, &[], a) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => all_confirmed = false,
        }
    }
    if all_confirmed {
        Some(true)
    } else {
        None
    }
}

/// A strict partial order used only to break a tie among candidates that
/// have *already* each individually passed [`type_compatible`] against the
/// real call's arguments -- never used to eliminate. Returning `false` for
/// "no defined ordering" (as opposed to "b is more specific") is the safe
/// default: an unresolved ordering just leaves the tie unresolved
/// (`Resolution::Candidates`), never a guess.
pub(crate) fn is_more_specific(
    table: &SymbolTable,
    a_name: &str,
    a_args: &[SmolStr],
    b_name: &str,
    b_args: &[SmolStr],
) -> bool {
    if a_name.eq_ignore_ascii_case(b_name) {
        return collection_kind(a_name).is_some()
            && a_args.len() == b_args.len()
            && !a_args.is_empty()
            && a_args.iter().zip(b_args.iter()).all(|(x, y)| {
                x.eq_ignore_ascii_case(y) || is_more_specific(table, x, &[], y, &[])
            })
            && a_args
                .iter()
                .zip(b_args.iter())
                .any(|(x, y)| !x.eq_ignore_ascii_case(y) && is_more_specific(table, x, &[], y, &[]));
    }
    if b_name.eq_ignore_ascii_case("Object") {
        // Anything -- curated system type or project-local -- is strictly
        // more specific than the universal top type.
        return true;
    }
    if let (Some(a_rank), Some(b_rank)) = (numeric_rank(a_name), numeric_rank(b_name)) {
        return a_rank < b_rank;
    }
    if let (Some(a_id), Some(b_id)) = (table.top_level(a_name), table.top_level(b_name)) {
        return a_id != b_id && table.inherited_chain(a_id).contains(&b_id);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_id::FileId;

    fn sid(local: u32) -> SymbolId {
        SymbolId::new(FileId(0), local)
    }

    fn empty_table() -> SymbolTable {
        SymbolTable::default()
    }

    fn sys(name: &'static str) -> Ty {
        Ty::system(name)
    }

    fn sys_args(name: &'static str, args: Vec<Ty>) -> Ty {
        Ty::system_with_args(name, args)
    }

    #[test]
    fn object_param_accepts_anything() {
        let table = empty_table();
        assert_eq!(
            type_compatible(&table, "Object", &[], &sys("String")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&table, "Object", &[], &Ty::Project(sid(0))),
            Some(true)
        );
    }

    #[test]
    fn numeric_widening_is_one_directional() {
        let table = empty_table();
        assert_eq!(
            type_compatible(&table, "Long", &[], &sys("Integer")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&table, "Integer", &[], &sys("Long")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&table, "Decimal", &[], &sys("Double")),
            Some(true)
        );
    }

    #[test]
    fn string_and_boolean_are_exact_only() {
        let table = empty_table();
        assert_eq!(
            type_compatible(&table, "String", &[], &sys("Boolean")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&table, "Boolean", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&table, "String", &[], &sys("String")),
            Some(true)
        );
    }

    #[test]
    fn uncurated_system_types_are_never_eliminated() {
        let table = empty_table();
        assert_eq!(
            type_compatible(&table, "Id", &[], &sys("String")),
            None
        );
        assert_eq!(
            type_compatible(&table, "Comparable", &[], &Ty::Project(sid(0))),
            None
        );
    }

    #[test]
    fn collection_element_type_recurses_with_the_same_rules() {
        let table = empty_table();
        // List<Object> accepts any element type, same as a bare `Object`.
        assert_eq!(
            type_compatible(
                &table,
                "List",
                &[SmolStr::new_static("Object")],
                &sys_args("List", vec![sys("Integer")]),
            ),
            Some(true)
        );
        // List<Long> accepts a List<Integer> argument (widening nests).
        assert_eq!(
            type_compatible(
                &table,
                "List",
                &[SmolStr::new_static("Long")],
                &sys_args("List", vec![sys("Integer")]),
            ),
            Some(true)
        );
        // List<Integer> does not accept a List<Long> argument (the reverse).
        assert_eq!(
            type_compatible(
                &table,
                "List",
                &[SmolStr::new_static("Integer")],
                &sys_args("List", vec![sys("Long")]),
            ),
            Some(false)
        );
        // Different collection kinds never convert.
        assert_eq!(
            type_compatible(
                &table,
                "Set",
                &[SmolStr::new_static("Integer")],
                &sys_args("List", vec![sys("Integer")]),
            ),
            Some(false)
        );
    }

    #[test]
    fn specificity_prefers_the_exact_and_narrower_type() {
        let table = empty_table();
        assert!(is_more_specific(&table, "Integer", &[], "Object", &[]));
        assert!(!is_more_specific(&table, "Object", &[], "Integer", &[]));
        assert!(is_more_specific(&table, "Integer", &[], "Long", &[]));
        assert!(!is_more_specific(&table, "Long", &[], "Integer", &[]));
        assert!(is_more_specific(
            &table,
            "List",
            &[SmolStr::new_static("Integer")],
            "List",
            &[SmolStr::new_static("Long")],
        ));
    }
}
