//! Apex's implicit-conversion and overload-specificity rules for a small,
//! fixed set of "curated" system types: the numeric family (`Integer`/
//! `Long`/`Double`/`Decimal`), `Boolean`, `String`, `Id`, `Date`/`Datetime`/
//! `Time`, `Blob`, `Object`, `SObject`, and the three built-in generic
//! collections (`List`/`Set`/`Map`).
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
//!
//! A second round of org verification, widening the original curated set,
//! turned up a third surprise even less intuitive than the first two:
//! when both `describe(Id)` and `describe(String)` overloads exist, a
//! real org resolves *every* call to the `describe(String)` overload --
//! including a call whose argument's declared type is `Id` itself, which
//! would normally be the exact-match winner the way `Integer` beats
//! `Object` for an `Integer` argument. `Id`/`String` are still recorded
//! as mutually compatible (an `Id` argument must not eliminate a
//! `String`-only candidate, and vice versa -- confirmed real by both
//! directions of `Id`<->`String` assignment compiling and running
//! successfully against a real org), but deliberately *no* specificity
//! rule is added between them: one org data point isn't enough to trust
//! generalizing "`String` always wins," and guessing wrong here would
//! hand back a confident, wrong `Resolved` answer instead of an honest
//! `Candidates` -- exactly the failure mode [`is_more_specific`]'s own
//! doc comment already calls out as worse than an unresolved tie.
//!
//! `Date` widens to `Datetime` (one-directional, confirmed by both a
//! successful `Date`->`Datetime` assignment and a real
//! `Illegal assignment from Datetime to Date` compile error for the
//! reverse) -- modeled with the exact same rank-table shape
//! [`numeric_rank`] already uses for `Integer`/`Long`/`Double`/`Decimal`,
//! including the same "exact match beats widened match" specificity
//! result confirmed for both argument directions. `Time` has no
//! confirmed relationship with either (`Illegal assignment from Datetime
//! to Time`) and `Blob` has none with `String` in either direction
//! (`Illegal assignment from String to Blob` / `... from Blob to
//! String`) -- both are curated purely so a mismatch against a
//! *different* curated type is a positive, provable elimination instead
//! of the "can't prove wrong" `None` an uncurated type always got before.
//!
//! `SObject` widening needed a real, not heuristic, notion of "is this
//! system type name actually a Salesforce object" -- unlike every other
//! curated type, the universe of real SObject names is dynamic (every
//! standard and custom object in the org/project), not a small fixed
//! list, so this module now also takes a `&SchemaIndex` to ask. A real
//! object type upcasts to `SObject` (confirmed: `Account` assigns to an
//! `SObject`-typed variable, and `List<Account>` satisfies a
//! `List<SObject>`-only parameter the same nested-nesting way numeric/
//! `extends` widening already does), never the reverse (`Illegal
//! assignment from SObject to Account`), and two different concrete
//! object types are never mutually compatible either (`Illegal
//! assignment from Account to Contact`) -- unlike a project-local
//! `extends` chain, Apex has no SObject-to-SObject subtyping at all, so
//! "different real object names" is *always* a definite mismatch, not
//! just an unmodeled one.
//!
//! A real object is also never compatible with a curated *scalar*
//! (confirmed: `Id someId = aContactRecord;` and `String s =
//! aContactRecord;` are both real `Illegal assignment` compile errors),
//! including nested one level inside a collection -- a same-arity
//! `pick(Id)`/`pick(Contact)` pair (and, separately, `pick(List<Id>)`/
//! `pick(List<Contact>)`) called with a real `Contact` value
//! unambiguously resolves to the `Contact` overload in both shapes, i.e.
//! the real compiler never even treats the `Id`/`List<Id>` candidate as
//! viable. Before this was added, that specific pairing fell through
//! `system_type_compatible` all the way to `None` (`Contact` isn't
//! itself one of the curated names `is_curated` recognizes), so a real
//! `List<Id>`/`List<CustomObject__c>` overload pair stayed a
//! `Resolution::Candidates` tie no argument could ever break.
//!
//! [`widen`] answers a related but distinct question on top of the same
//! curated rules -- not "can this value be passed here," but "what's the
//! common type of a ternary's two branches" (`crate::resolve`'s
//! `Expr::Ternary` arm) -- verified against a real org the same way
//! every rule above was, and confirmed to need no *new* rules at all:
//! real Apex ternary widening turned out to be exactly this module's
//! existing directional checks, applied in both directions, not a
//! separate common-ancestor search. See [`widen`]'s own doc comment for
//! the specific org evidence.

use crate::schema_index::SchemaIndex;
use crate::stdlib_index::StdlibIndex;
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

/// `Date`(0) < `Datetime`(1) -- `Date` widens implicitly to `Datetime`,
/// never the reverse. Verified against a real org exactly like
/// [`numeric_rank`]: a `Date` argument assigns to a `Datetime`-typed
/// variable and a `Date`/`Datetime` overload pair called with a `Date`
/// argument picks the exact-match `Date` overload, while the reverse
/// assignment (`Datetime` -> `Date`) is a real `Illegal assignment`
/// compile error. `Time` is deliberately not part of this table -- a
/// `Datetime` argument does not assign to a `Time`-typed variable either
/// (also a confirmed real compile error), so it has no widening
/// relationship with either of these, only with itself.
fn date_rank(name: &str) -> Option<u8> {
    const ORDER: [&str; 2] = ["Date", "Datetime"];
    ORDER
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .map(|i| i as u8)
}

/// Every name this module has a rule for -- used only to decide whether
/// two *differing* names are a known, definite mismatch (`Some(false)`) or
/// genuinely uncharted territory (`None`, never eliminate).
fn is_curated(name: &str) -> bool {
    name.eq_ignore_ascii_case("String")
        || name.eq_ignore_ascii_case("Boolean")
        || name.eq_ignore_ascii_case("Id")
        || name.eq_ignore_ascii_case("Time")
        || name.eq_ignore_ascii_case("Blob")
        || name.eq_ignore_ascii_case("SObject")
        || numeric_rank(name).is_some()
        || date_rank(name).is_some()
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
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
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
            if table.resolve_dotted_name(param_name).is_some() {
                // The parameter is project-local (a user class/interface),
                // the argument is a system value -- no defined conversion
                // either direction, and a builtin system type can never be
                // made to implement a user-defined interface either.
                return Some(false);
            }
            system_type_compatible(schema, stdlib, table, param_name, param_args, arg_name, arg_args)
        }
    }
}

fn project_arg_compatible(table: &SymbolTable, param_name: &str, arg_id: SymbolId) -> Option<bool> {
    if let Some(param_id) = table.resolve_dotted_name(param_name) {
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
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    table: &SymbolTable,
    param_name: &str,
    param_args: &[SmolStr],
    arg_name: &str,
    arg_args: &[Ty],
) -> Option<bool> {
    if param_name.eq_ignore_ascii_case(arg_name) {
        return if collection_kind(param_name).is_some() {
            collection_args_compatible(schema, stdlib, table, param_args, arg_args)
        } else {
            Some(true)
        };
    }
    if let (Some(arg_rank), Some(param_rank)) = (numeric_rank(arg_name), numeric_rank(param_name)) {
        return Some(arg_rank <= param_rank);
    }
    if let (Some(arg_rank), Some(param_rank)) = (date_rank(arg_name), date_rank(param_name)) {
        return Some(arg_rank <= param_rank);
    }
    // `Id`/`String` are bidirectionally compatible (confirmed: both
    // assignment directions compile and run successfully against a real
    // org) -- deliberately no specificity claim between them, see this
    // module's own doc comment on the org's surprising `String`-always-
    // wins overload result.
    if (param_name.eq_ignore_ascii_case("Id") && arg_name.eq_ignore_ascii_case("String"))
        || (param_name.eq_ignore_ascii_case("String") && arg_name.eq_ignore_ascii_case("Id"))
    {
        return Some(true);
    }
    // `SObject` accepts any real object type -- confirmed via
    // `schema.object`, not a name guess, since the universe of real
    // object names is dynamic (unlike every other curated type here).
    // Never the reverse (an `SObject`-typed value doesn't implicitly
    // downcast to a concrete object type), and two *different* concrete
    // object types are never mutually compatible either -- unlike a
    // project-local `extends` chain, Apex has no SObject-to-SObject
    // subtyping at all, both confirmed by real `Illegal assignment`
    // compile errors.
    if param_name.eq_ignore_ascii_case("SObject") && schema.object(arg_name).is_some() {
        return Some(true);
    }
    if schema.object(param_name).is_some()
        && (arg_name.eq_ignore_ascii_case("SObject") || schema.object(arg_name).is_some())
    {
        return Some(false);
    }
    // A concrete SObject value is never compatible with a curated
    // *scalar* (`Id`/`String`/`Boolean`/numeric/`Date`/`Datetime`/`Time`/
    // `Blob`) -- confirmed against a real org: `Id someId =
    // aContactRecord;` and `String s = aContactRecord;` are both real
    // `Illegal assignment` compile errors, and a same-arity
    // `pick(Id)`/`pick(Contact)` (also confirmed nested one level, via
    // `pick(List<Id>)`/`pick(List<Contact>)`) overload pair called with a
    // real `Contact` value unambiguously resolves to the `Contact`
    // overload -- the real compiler never even treats the `Id`/`List<Id>`
    // candidate as viable, matching this returning a definite `Some(false)`
    // rather than the `None` that would leave both candidates alive.
    // `param_name` here is never `SObject` itself or another real object
    // (both already handled above), so this only ever fires for a
    // genuine scalar-vs-object mismatch.
    if is_curated(param_name) && schema.object(arg_name).is_some() {
        return Some(false);
    }
    if is_curated(param_name) && is_curated(arg_name) {
        // Different curated families with no widening relationship
        // between them (e.g. `String` vs `Boolean`, `List` vs `Set`, a
        // collection vs a scalar) -- a real, definite mismatch.
        return Some(false);
    }
    // A curated *scalar* (every curated name except `List`/`Set`/`Map`,
    // via `collection_kind`) on either side is a real, definite mismatch
    // against *any* other differing type name, not just another curated
    // one -- Apex has no user-definable implicit conversions at all, so
    // this holds regardless of what the other (uncurated, real but
    // unmodeled) name turns out to be, the same way it already holds
    // between two curated names above. Verified against a real org in
    // both directions with names this module has no other rule for:
    // `Schema.SObjectField`/`Schema.DescribeFieldResult`/`System.Comparable`
    // are all real `Illegal assignment` compile errors against `String`,
    // and so is the reverse (`String s = aSObjectFieldValue;`). Real bug
    // this fixes: `checkFieldIsUpdateable(SObjectType, String)` /
    // `(SObjectType, SObjectField)` / `(SObjectType, DescribeFieldResult)`
    // overloads called with a `String` argument used to stay a
    // three-way `Candidates` tie forever, since neither `SObjectField`
    // nor `DescribeFieldResult` is curated and this function returned
    // `None` (can't prove wrong) for both instead of ruling them out.
    // Deliberately excludes `List`/`Set`/`Map`: those aren't sealed the
    // same way -- confirmed a collection legitimately satisfies an
    // uncurated *interface* name this module doesn't otherwise model
    // (`List<String>` assigns to `Iterable<String>`, a real success, not
    // a compile error) -- so a collection on either side must stay
    // `None` here, not a guessed elimination.
    if (is_curated(param_name) && collection_kind(param_name).is_none())
        || (is_curated(arg_name) && collection_kind(arg_name).is_none())
    {
        return Some(false);
    }
    // Two *different*, both real -- each independently confirmed to be a
    // genuine, documented stdlib class via `StdlibIndex`, not just a name
    // guess -- and neither a curated collection: also a definite mismatch,
    // for the same "Apex has no user-definable implicit conversions"
    // reason the curated-scalar rule above already relies on. Verified
    // against a real org: `Schema.SObjectField` does not assign to
    // `Schema.DescribeFieldResult` (a real `Illegal assignment` compile
    // error) even though *neither* name is in this module's curated set.
    // Real bug this fixes: fflib's `checkFieldIsUpdateable(SObjectType,
    // SObjectField)` / `(SObjectType, DescribeFieldResult)` overload pair,
    // called with a `fflib_SObjectDescribe.getField(...)` argument (a
    // real, resolved `SObjectField`), used to stay a `Candidates` tie
    // forever -- goto-definition showed both -- since this function had
    // no rule at all for two differing *uncurated* system names, only for
    // a curated one against anything. Deliberately requires *both* names
    // to resolve via `stdlib.class` (not just one): an unresolvable name
    // on either side (a typo, or a genuinely unmodeled system type this
    // crate's stdlib snapshot doesn't carry) must keep the honest
    // "can't prove wrong" `None` below, never a guessed elimination.
    // `Iterable`/`Iterator` aren't in the bundled snapshot at all (no
    // scraped page with real method/property content), so `List`/`Set`
    // satisfying them, as already covered by the collection exclusion
    // above, is unaffected either way.
    if collection_kind(param_name).is_none()
        && collection_kind(arg_name).is_none()
        && stdlib.class(param_name).is_some()
        && stdlib.class(arg_name).is_some()
    {
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
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    table: &SymbolTable,
    param_args: &[SmolStr],
    arg_args: &[Ty],
) -> Option<bool> {
    if param_args.len() != arg_args.len() {
        return None;
    }
    let mut all_confirmed = true;
    for (p, a) in param_args.iter().zip(arg_args.iter()) {
        match type_compatible(schema, stdlib, table, p, &[], a) {
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

/// `ty`'s own type name, one level deep (no nested type arguments) --
/// bridges [`widen`]'s two already-*inferred* `Ty` values into
/// [`type_compatible`]'s "declared parameter" shape (`&str` name + a
/// separate `&[SmolStr]` for *its* type arguments), which is the only
/// asymmetry between the two: `type_compatible`'s `arg` side is a real
/// `Ty`, but its `param_name`/`param_args` side is always a name captured
/// from declared-type source text, since a parameter's own type was never
/// itself inferred. A project-local type's name resolves the same way
/// `type_compatible`/`is_more_specific` already look one up by string
/// (`table.resolve_dotted_name`), so handing back its plain `name` here
/// (not a dotted/qualified path) is exactly what those call sites expect.
fn ty_arg_name(table: &SymbolTable, ty: &Ty) -> SmolStr {
    match ty {
        Ty::Project(id) => table.get(*id).name.clone(),
        Ty::System { name, .. } => name.clone(),
    }
}

/// The common type of a ternary's two branches (`crate::resolve`'s
/// `Expr::Ternary` arm), verified against a real connected org before
/// being trusted (`sf apex run` anonymous-Apex probes, this module's own
/// established practice) rather than assumed from first principles --
/// real Apex ternaries turned out to use exactly the same *directional*
/// assignability every other conversion context here already models, not
/// a general "find the nearest common ancestor" join the way Java's
/// conditional operator does: `Integer i; Long l; Long ok = flag ? i : l;`
/// compiles (`Integer` widens into `Long`, confirmed the same direction
/// [`numeric_rank`] already uses) but `Integer bad = flag ? i : l;` is a
/// real `Illegal assignment from Long to Integer` compile error -- and,
/// more surprisingly, two sibling classes with no *direct* relationship
/// to each other (`Dog`/`Cat`, both merely `extends Animal`) is *also* a
/// real compile error, `Incompatible types in ternary operator: Cat,
/// Dog`, even though they share a common ancestor -- while two classes in
/// a direct `extends` relationship (`Animal`/`Dog`) widen exactly the way
/// [`project_arg_compatible`]'s existing `inherited_chain` check already
/// says they should. So this function needs no new common-ancestor
/// machinery at all: every case reuses an existing directional rule,
/// applied in both directions, and returns `None` (not a guess) whenever
/// neither direction succeeds -- which the `String`/`Boolean` probe
/// confirmed is a real Apex compile error in its own right
/// (`Incompatible types in ternary operator: Boolean, String`), not just
/// this module being conservative.
pub(crate) fn widen(schema: &SchemaIndex, stdlib: &StdlibIndex, table: &SymbolTable, a: &Ty, b: &Ty) -> Option<Ty> {
    if a == b {
        return Some(a.clone());
    }
    match (a, b) {
        (Ty::Project(a_id), Ty::Project(b_id)) => {
            if table.inherited_chain(*a_id).contains(b_id) {
                Some(Ty::Project(*b_id))
            } else if table.inherited_chain(*b_id).contains(a_id) {
                Some(Ty::Project(*a_id))
            } else {
                None
            }
        }
        (
            Ty::System {
                name: a_name,
                args: a_args,
            },
            Ty::System {
                name: b_name,
                args: b_args,
            },
        ) => {
            let a_arg_names: Vec<SmolStr> = a_args.iter().map(|t| ty_arg_name(table, t)).collect();
            let b_arg_names: Vec<SmolStr> = b_args.iter().map(|t| ty_arg_name(table, t)).collect();
            let a_into_b = type_compatible(schema, stdlib, table, b_name, &b_arg_names, a);
            let b_into_a = type_compatible(schema, stdlib, table, a_name, &a_arg_names, b);
            match (a_into_b, b_into_a) {
                // Bidirectionally compatible with no established
                // specificity (`Id`/`String` -- see this module's own
                // doc comment on the org's surprising overload-preference
                // result) -- no principled winner, so this stays honestly
                // unresolved rather than an arbitrary pick.
                (Some(true), Some(true)) => None,
                (Some(true), _) => Some(b.clone()),
                (_, Some(true)) => Some(a.clone()),
                _ => None,
            }
        }
        // A project-local type and a system/schema type have no defined
        // relationship either direction -- mirrors `project_arg_compatible`'s
        // own "a project class is never one of Apex's sealed builtin/
        // system types" reasoning.
        _ => None,
    }
}

/// A strict partial order used only to break a tie among candidates that
/// have *already* each individually passed [`type_compatible`] against the
/// real call's arguments -- never used to eliminate. Returning `false` for
/// "no defined ordering" (as opposed to "b is more specific") is the safe
/// default: an unresolved ordering just leaves the tie unresolved
/// (`Resolution::Candidates`), never a guess.
pub(crate) fn is_more_specific(
    schema: &SchemaIndex,
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
                x.eq_ignore_ascii_case(y) || is_more_specific(schema, table, x, &[], y, &[])
            })
            && a_args.iter().zip(b_args.iter()).any(|(x, y)| {
                !x.eq_ignore_ascii_case(y) && is_more_specific(schema, table, x, &[], y, &[])
            });
    }
    if b_name.eq_ignore_ascii_case("Object") {
        // Anything -- curated system type or project-local -- is strictly
        // more specific than the universal top type.
        return true;
    }
    if let (Some(a_rank), Some(b_rank)) = (numeric_rank(a_name), numeric_rank(b_name)) {
        return a_rank < b_rank;
    }
    if let (Some(a_rank), Some(b_rank)) = (date_rank(a_name), date_rank(b_name)) {
        return a_rank < b_rank;
    }
    // A real object type is strictly more specific than the universal
    // `SObject`, the same "narrower wins" shape `Object`'s own case
    // above already uses -- confirmed by the same org evidence backing
    // `system_type_compatible`'s `SObject` rule.
    if b_name.eq_ignore_ascii_case("SObject") && schema.object(a_name).is_some() {
        return true;
    }
    if let (Some(a_id), Some(b_id)) = (table.resolve_dotted_name(a_name), table.resolve_dotted_name(b_name)) {
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

    fn empty_schema() -> SchemaIndex {
        SchemaIndex::from_sobjects(Vec::new())
    }

    /// The real bundled standard-object snapshot -- `Account`/`Contact`/...
    /// actually present, for the `SObject`-widening tests, which need a
    /// real object name `schema.object` can find.
    fn standard_schema() -> SchemaIndex {
        SchemaIndex::from_sobjects(apex_stdlib::standard_sobjects().to_vec())
    }

    /// The real bundled standard-class snapshot -- needed by every test
    /// here, not just the ones specifically about the new "two real,
    /// distinct stdlib classes" elimination rule, since `stdlib.class`
    /// is now consulted (and must find nothing) even for names outside
    /// that rule's own tests.
    fn real_stdlib() -> StdlibIndex {
        StdlibIndex::new()
    }

    fn sys(name: &'static str) -> Ty {
        Ty::system(name)
    }

    fn sys_args(name: &'static str, args: Vec<Ty>) -> Ty {
        Ty::system_with_args(name, args)
    }

    #[test]
    fn object_param_accepts_anything() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Object", &[], &sys("String")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Object", &[], &Ty::Project(sid(0))),
            Some(true)
        );
    }

    #[test]
    fn numeric_widening_is_one_directional() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Long", &[], &sys("Integer")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Integer", &[], &sys("Long")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Decimal", &[], &sys("Double")),
            Some(true)
        );
    }

    #[test]
    fn string_and_boolean_are_exact_only() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "String", &[], &sys("Boolean")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Boolean", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "String", &[], &sys("String")),
            Some(true)
        );
    }

    #[test]
    fn uncurated_system_types_are_never_eliminated() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Comparable", &[], &Ty::Project(sid(0))),
            None
        );
        // Both names uncurated (`Iterable`/`SObjectField`, neither this
        // module has a rule for) -- still an honest `None`, not the
        // curated-scalar elimination `a_curated_scalar_is_never_compatible_with_an_uncurated_system_type`
        // covers below, since neither side is a sealed scalar this
        // module can reason about.
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Iterable", &[], &sys("SObjectField")),
            None
        );
        // `List` is curated but, unlike a scalar, isn't sealed against an
        // uncurated *interface* name -- confirmed via a real org:
        // `List<String>` assigns to an `Iterable<String>`-typed variable
        // (a real success, not a compile error). Must stay `None`
        // (`collection_kind` exempts it from the new scalar-elimination
        // rule below), not a wrongly-guessed `Some(false)`.
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Iterable", &[], &sys("List")),
            None
        );
    }

    /// A curated *scalar* (every curated name except `List`/`Set`/`Map`)
    /// is never compatible with a differently-named, uncurated system
    /// type on either side -- Apex has no user-definable implicit
    /// conversions, so a sealed scalar's incompatibility with anything
    /// outside its own curated relationships holds regardless of whether
    /// this module has a rule for the *other* name too. Real bug this
    /// fixes: fflib's `checkFieldIsUpdateable(SObjectType, String)` /
    /// `(SObjectType, SObjectField)` / `(SObjectType, DescribeFieldResult)`
    /// overload trio, called with a `String` argument, used to stay a
    /// three-way `Candidates` tie forever (goto-definition showing all
    /// three) since neither `SObjectField` nor `DescribeFieldResult` is
    /// curated. Verified against a real org: `Schema.SObjectField`/
    /// `Schema.DescribeFieldResult`/`System.Comparable` are all real
    /// `Illegal assignment` compile errors against `String`, and so is
    /// the reverse (`String s = aSObjectFieldValue;`).
    #[test]
    fn a_curated_scalar_is_never_compatible_with_an_uncurated_system_type() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "SObjectField", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "DescribeFieldResult", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Comparable", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "String", &[], &sys("SObjectField")),
            Some(false)
        );
    }

    /// Two *different*, both real (each independently confirmed present
    /// in `StdlibIndex`, i.e. neither is a name guess or typo) system
    /// types with neither curated at all are also a definite mismatch --
    /// a further generalization beyond the curated-scalar rule above,
    /// for the case *neither* side is curated. Real bug this fixes:
    /// fflib's `checkFieldIsUpdateable(SObjectType, SObjectField)` /
    /// `(SObjectType, DescribeFieldResult)` overload pair, called with a
    /// real `fflib_SObjectDescribe.getField(...)` argument (resolved to
    /// `SObjectField`), used to stay a `Candidates` tie forever --
    /// goto-definition showed both -- since this function had no rule at
    /// all for two differing uncurated names. Verified against a real
    /// org: `Schema.DescribeFieldResult d = aSObjectFieldValue;` is a
    /// real `Illegal assignment from Schema.SObjectField to
    /// Schema.DescribeFieldResult` compile error. `Iterable` is
    /// deliberately not in this test (see `uncurated_system_types_are_never_eliminated`
    /// above): it isn't in the bundled snapshot at all, so it can never
    /// trigger this rule regardless of the other side.
    #[test]
    fn two_different_real_uncurated_stdlib_classes_are_never_compatible() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "DescribeFieldResult", &[], &sys("SObjectField")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "SObjectField", &[], &sys("DescribeFieldResult")),
            Some(false)
        );
    }

    /// `Id`/`String` are bidirectionally compatible -- confirmed real
    /// against a real org (both assignment directions compile and run),
    /// unlike the surprising overload-preference result this module's
    /// own doc comment covers, which is deliberately *not* encoded here.
    #[test]
    fn id_and_string_are_mutually_compatible_with_no_specificity_order() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Id", &[], &sys("String")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "String", &[], &sys("Id")),
            Some(true)
        );
        assert!(!is_more_specific(&schema, &table, "Id", &[], "String", &[]));
        assert!(!is_more_specific(&schema, &table, "String", &[], "Id", &[]));
    }

    /// `Date` widens to `Datetime`, never the reverse -- confirmed real
    /// (a real `Illegal assignment from Datetime to Date` compile error
    /// for the reverse direction). `Time` has no relationship with
    /// either.
    #[test]
    fn date_widens_to_datetime_but_not_the_reverse() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Datetime", &[], &sys("Date")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Date", &[], &sys("Datetime")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Time", &[], &sys("Datetime")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Time", &[], &sys("Date")),
            Some(false)
        );
        assert!(is_more_specific(&schema, &table, "Date", &[], "Datetime", &[]));
        assert!(!is_more_specific(&schema, &table, "Datetime", &[], "Date", &[]));
    }

    /// `Blob` has no implicit conversion with `String` in either
    /// direction -- confirmed real (`Illegal assignment` compile errors
    /// both ways).
    #[test]
    fn blob_has_no_relationship_with_string() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Blob", &[], &sys("String")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "String", &[], &sys("Blob")),
            Some(false)
        );
    }

    /// A real object type widens to `SObject` (upcast), never the
    /// reverse, and two *different* concrete object types are never
    /// mutually compatible -- all three confirmed real against a real
    /// org (`Account` assigns to `SObject`; `Illegal assignment from
    /// SObject to Account`; `Illegal assignment from Account to
    /// Contact`).
    #[test]
    fn a_real_object_type_widens_to_sobject_but_not_across_object_types() {
        let schema = standard_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "SObject", &[], &sys("Account")),
            Some(true)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Account", &[], &sys("SObject")),
            Some(false)
        );
        assert_eq!(
            type_compatible(&schema, &stdlib, &table, "Contact", &[], &sys("Account")),
            Some(false)
        );
        assert!(is_more_specific(&schema, &table, "Account", &[], "SObject", &[]));
        assert!(!is_more_specific(&schema, &table, "SObject", &[], "Account", &[]));
    }

    /// `List<Account>` satisfies a `List<SObject>`-only parameter, the
    /// same nested-widening shape numeric/`extends` widening already
    /// have -- confirmed real against a real org.
    #[test]
    fn list_of_a_real_object_type_widens_to_list_of_sobject() {
        let schema = standard_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(
            type_compatible(
                &schema,
                &stdlib,
                &table,
                "List",
                &[SmolStr::new_static("SObject")],
                &sys_args("List", vec![sys("Account")]),
            ),
            Some(true)
        );
    }

    /// A real object type is never compatible with a curated *scalar*,
    /// nested one level inside a collection or not -- confirmed real
    /// against a real org (`Id someId = aContactRecord;`/`String s =
    /// aContactRecord;` are both `Illegal assignment` compile errors, and
    /// a same-arity `pick(Id)`/`pick(Contact)` -- also confirmed nested,
    /// `pick(List<Id>)`/`pick(List<Contact>)` -- called with a real
    /// `Contact` value unambiguously resolves to the `Contact` overload
    /// in both shapes). Before this rule existed, `Contact` (a name
    /// `is_curated` doesn't recognize) made this fall through to `None`,
    /// leaving a real `List<Id>`/`List<CustomObject__c>` overload pair an
    /// unbreakable `Resolution::Candidates` tie -- the exact user-reported
    /// bug (`BDI_DataImport_API.processDataImportRecords`'s
    /// `List<DataImport__c>`/`List<Id>` overloads in real NPSP).
    #[test]
    fn a_real_object_type_is_never_compatible_with_a_curated_scalar() {
        let schema = standard_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(type_compatible(&schema, &stdlib, &table, "Id", &[], &sys("Contact")), Some(false));
        assert_eq!(type_compatible(&schema, &stdlib, &table, "String", &[], &sys("Contact")), Some(false));
        assert_eq!(
            type_compatible(
                &schema,
                &stdlib,
                &table,
                "List",
                &[SmolStr::new_static("Id")],
                &sys_args("List", vec![sys("Contact")]),
            ),
            Some(false)
        );
    }

    #[test]
    fn collection_element_type_recurses_with_the_same_rules() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        // List<Object> accepts any element type, same as a bare `Object`.
        assert_eq!(
            type_compatible(
                &schema,
                &stdlib,
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
                &schema,
                &stdlib,
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
                &schema,
                &stdlib,
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
                &schema,
                &stdlib,
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
        let schema = empty_schema();
        let table = empty_table();
        assert!(is_more_specific(&schema, &table, "Integer", &[], "Object", &[]));
        assert!(!is_more_specific(&schema, &table, "Object", &[], "Integer", &[]));
        assert!(is_more_specific(&schema, &table, "Integer", &[], "Long", &[]));
        assert!(!is_more_specific(&schema, &table, "Long", &[], "Integer", &[]));
        assert!(is_more_specific(
            &schema,
            &table,
            "List",
            &[SmolStr::new_static("Integer")],
            "List",
            &[SmolStr::new_static("Long")],
        ));
    }

    // `widen`'s `(Ty::Project, Ty::Project)` branch (the `inherited_chain`
    // case) is covered end-to-end by
    // `crates/apex-binder/tests/ternary_type_widening.rs` against real
    // parsed/bound source instead of here, matching this file's existing
    // convention: no other test in this module hand-builds a `SymbolTable`
    // with real project-local declarations/inheritance either (see e.g.
    // `is_more_specific`'s own `inherited_chain` branch above, also
    // untested at this level) since that setup is more naturally exercised
    // through the real binder pipeline.

    #[test]
    fn widen_returns_the_shared_type_unchanged_when_branches_already_match() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Integer"), &sys("Integer")), Some(sys("Integer")));
    }

    #[test]
    fn widen_numeric_branches_to_the_wider_type() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Integer"), &sys("Long")), Some(sys("Long")));
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Long"), &sys("Integer")), Some(sys("Long")));
    }

    #[test]
    fn widen_date_branches_to_datetime() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Date"), &sys("Datetime")), Some(sys("Datetime")));
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Datetime"), &sys("Date")), Some(sys("Datetime")));
    }

    /// `Id`/`String` are bidirectionally compatible with no established
    /// specificity (see `type_compatible`'s own doc comment) -- `widen`
    /// must not arbitrarily pick a winner between them.
    #[test]
    fn widen_returns_none_for_bidirectionally_compatible_branches_with_no_specificity() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Id"), &sys("String")), None);
    }

    #[test]
    fn widen_a_real_object_type_and_sobject_widens_to_sobject() {
        let schema = standard_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Account"), &sys("SObject")), Some(sys("SObject")));
        assert_eq!(widen(&schema, &stdlib, &table, &sys("SObject"), &sys("Account")), Some(sys("SObject")));
    }

    #[test]
    fn widen_returns_none_for_two_different_object_types() {
        let schema = standard_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("Account"), &sys("Contact")), None);
    }

    #[test]
    fn widen_returns_none_for_genuinely_incompatible_branches() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &sys("String"), &sys("Boolean")), None);
    }

    #[test]
    fn widen_returns_none_for_a_project_and_system_type_mismatch() {
        let schema = empty_schema();
        let table = empty_table();
        let stdlib = real_stdlib();
        assert_eq!(widen(&schema, &stdlib, &table, &Ty::Project(sid(0)), &sys("String")), None);
    }
}
