//! A "golden ratchet" regression guard, complementing (not replacing)
//! `scope_resolution_smoke.rs`'s loose sanity floors/ceiling: pins the
//! exact whole-corpus `Resolved`/`Unresolved` counts as of the commit
//! that added this test, and fails if either one regresses --
//! `unresolved` rising, or `resolved` falling, both mean *something*
//! that used to connect a real reference to its declaration no longer
//! does, project-wide, not just in whichever one file a human happened
//! to be reading when they noticed.
//!
//! This exists because three real, independent resolver bugs
//! (self-nested qualified type names never resolving outside
//! `extends`/`implements`, dead-code detection blind to interface/
//! virtual dynamic dispatch, and an ambiguous overload silently
//! discarding a chained call's type) were each found only because a
//! user happened to notice one specific false positive in one specific
//! real file and reported it -- there was nothing that would have
//! caught any of them automatically, or would notice if a *future*
//! change silently reintroduced a similar gap somewhere else in the
//! corpus. `scope_resolution_smoke.rs`'s existing checks (a 0.70
//! `Unresolved`-ratio *ceiling*, a 100,000 `Resolved` *floor*) are
//! deliberately loose -- generous enough to tolerate normal grammar/AST
//! evolution without becoming flaky -- which is exactly why they never
//! caught any of these three: each individual bug was many orders of
//! magnitude too small to move a ratio that coarse.
//!
//! This test intentionally does the opposite: pin the *exact* current
//! numbers as tightly as possible, so even a small handful of newly-
//! broken references trips it. The tradeoff is exactly the one that
//! implies: it *will* need its constants bumped for entirely legitimate
//! reasons (a grammar fix changes how many reference nodes exist at
//! all, a new `crate::generics`/`crate::conversions` rule correctly
//! resolves something that used to be `Unresolved`, `crate::resolve`'s
//! dynamic-dispatch widening correctly turns more `Resolved` calls into
//! `Candidates`, ...). That's expected maintenance, not a sign this test
//! is too strict -- when a change legitimately moves these numbers,
//! update the constants below in the same commit, with a one-line note
//! on why, the same way any other golden-snapshot test's baseline is
//! maintained. What this test guards against is a change that moves
//! them with *no* accompanying explanation at all.
//!
//! Deliberately does **not** try to distinguish "a real resolver bug"
//! from "a known, still-unmodeled standard-library gap" in what counts
//! toward `Unresolved` -- every `Unresolved` reference counts, stdlib
//! gaps included. That's a feature, not an oversight: when standard-
//! library method modeling is eventually added, that work will lower
//! this exact baseline for a documented reason, giving a precise,
//! accurate record of how much it actually improved -- rather than the
//! count being pre-filtered down to "only the parts that were already
//! believed to be bugs," which would silently hide that improvement.

use apex_binder::{BoundProgram, Resolution};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// Exact whole-corpus counts as of the commit that added this test
/// (after the dotted-nested-type, dynamic-dispatch, and ambiguous-
/// overload-chaining fixes). Update alongside any change that
/// legitimately moves them -- see this module's own doc comment.
///
/// `BASELINE_UNRESOLVED` was tightened from `152_539` to `149_289` when
/// `apex_stdlib`'s bundled standard SObject/field schema was wired into
/// `SchemaIndex` (`crates/apex-binder/src/schema_index.rs::merge_sobjects`):
/// 3,250 real standard-field accesses (`Account.Name` and the like) that
/// used to fall all the way through to `Resolution::Unresolved` now
/// correctly land in `Resolution::SchemaObject` instead -- which this
/// test deliberately doesn't tally either way (see this module's own
/// doc comment on why `SchemaObject`/`UnknownSchema`/`Candidates` are
/// excluded from both counts), so `BASELINE_RESOLVED` itself was
/// unaffected *then*.
///
/// Both constants moved again when `apex_stdlib`'s bundled standard-
/// library class/method/property schema was wired into
/// `crate::resolve`'s `Ty::System` method-call/field-access arms, in two
/// steps:
///
/// 1. Wiring `StdlibIndex` into the existing `Ty::System` arms alone
///    moved `BASELINE_UNRESOLVED` by exactly -15,171 (`149_289` ->
///    `134_118`) -- the same 15,171 references that now land in the
///    (also untallied) `Resolution::StdlibMember` instead -- plus
///    `BASELINE_RESOLVED` +1 (`204_984` -> `204_985`), confirmed via
///    direct bisection (reverted just this change, re-ran, got exactly
///    the old `204_984` back) to be a real, legitimate side effect: a
///    project-local overloaded call somewhere in NPSP takes a stdlib
///    method call's result as one of its own arguments, and that
///    argument's type used to be unknown (`None`), so
///    `narrow_by_overload`'s type-compatibility elimination couldn't use
///    it to disambiguate and the call stayed `Resolution::Candidates`
///    (untallied). Knowing the argument's real type now lets that same
///    existing overload logic eliminate every candidate but one.
/// 2. `bind_name_expr` turned out to have no fallback at all for a bare
///    class name used as a *static-call receiver* (`String` in
///    `String.isBlank(...)`, `Database` in `Database.query(...)`) --
///    only a project-local-type check and an SObject check, so a name
///    that was neither (every stdlib class) bound straight to `None`/
///    `Unresolved` without ever reaching the `Ty::System` arms above at
///    all. Adding the equivalent `StdlibIndex::class` fallback there
///    (mirroring `SchemaObjectRef`'s existing bare-object-name shape,
///    hence `StdlibMemberRef::member` becoming `Option<SmolStr>`) is
///    what actually makes `String.isBlank(...)`/`Database.query(...)`-
///    style calls resolve at all -- and since static stdlib calls are
///    extremely common in real Apex, this moved the numbers far more
///    than step 1 alone: `BASELINE_UNRESOLVED` -49,909 more (`134_118`
///    -> `84_209`), `BASELINE_RESOLVED` +2 more (`204_985` -> `204_987`,
///    same further-disambiguation mechanism as step 1).
/// 3. A real stdlib enum (`LoggingLevel`, `TriggerOperation`, ...) had
///    no way to model its *values* at all until `tools/salesforce-doc-scraper`'s
///    `apex_reference::parse_enum_values` was added -- an `Enum` page has
///    neither `nested2` leaves nor a `Signature` section, so every one
///    of the 104 real stdlib enums silently came out with zero
///    properties beforehand. Each value is modeled as a static property
///    of the enum's own type (`LoggingLevel.INFO`'s value `INFO` has
///    `type_name: "LoggingLevel"`), reusing the exact property-lookup
///    path a real stdlib property already has -- no new `Resolution`
///    variant or `crate::resolve` code needed for this step at all.
///    `BASELINE_UNRESOLVED` -250 more (`84_209` -> `83_959`);
///    `BASELINE_RESOLVED` unaffected (enum constant access never
///    produces a `SymbolId`-backed outcome either way).
/// 4. Widening `crate::conversions`'s curated type set (`Id`, `Date`/
///    `Datetime`/`Time`, `Blob`, and schema-verified `SObject`
///    widening, each confirmed against a real org the same way the
///    original numeric/`Object`/collection rules were) moved
///    `BASELINE_RESOLVED` +318 (`204_987` -> `205_305`) -- the same
///    "argument type newly known well enough to disambiguate" mechanism
///    documented in step 1 above, just against a much larger curated
///    surface. `BASELINE_UNRESOLVED` also dropped, by -5 (`83_959` ->
///    `83_954`): a stdlib method call chained onto a further reference
///    (`someCall(x).next()`) that previously couldn't narrow its own
///    overloaded return type (every overload survived elimination, so
///    `narrow_stdlib_overload_type` gave up with `None`) now narrows to
///    exactly one, giving the chained `.next()` a real base type to
///    resolve against instead of hitting `bind_field_expr`'s "target
///    type entirely unknown" `Unresolved` fallback.
/// 5. Ternary common-supertype widening (`crate::conversions::widen`,
///    verified against a real org before implementation -- see its own
///    doc comment) plus expanding `crate::generics`'s `List`/`Map`/`Set`
///    table (`remove`/`put`/`iterator`, plus a data-driven fallback for
///    `clone`/`deepClone` sourced from `apex_stdlib`'s scraped return
///    types) left `BASELINE_RESOLVED` unchanged (`205_305`, real NPSP
///    apparently has no chain shaped like `.remove(...).field`/
///    `.clone().get(...).field`/a ternary-widened project type that
///    previously failed) but moved `BASELINE_UNRESOLVED` -4
///    (`83_954` -> `83_950`): a handful of real ternaries whose branches
///    now widen to a real common type let a further chained reference
///    (most likely a stdlib call, since `Resolution::StdlibMember` isn't
///    tallied by either counter here) resolve instead of hitting the
///    "target type entirely unknown" fallback.
/// 6. **A real misresolution bug fix, not just a gap closed.** Apex's
///    SObject constructor field-init sugar (`new Contact(LastName =
///    'foo', Primary_Affiliation__c = acc.id)`) parses each `field =
///    value` pair as a perfectly ordinary `Expr::Bin` -- `arg_list`'s
///    grammar has no special case for it -- so `bind_new_expr` never
///    consulted the target SObject's schema at all for its arguments; the
///    LHS just fell through `bind_name_expr`'s ordinary local/member/type
///    lookups like any other identifier *read*. `BASELINE_RESOLVED`
///    dropped -80 (`205_305` -> `205_225`) as a direct result of fixing
///    this, not a regression: confirmed via a real-corpus diagnostic that
///    exactly 81 such field names (`CloseDate`, `AccountId`, `LastName`,
///    `Amount`, `Id`, `Name`, ...) were coincidentally resolving as
///    `Resolution::Resolved` against an unrelated same-named local/
///    parameter/field in the *enclosing* class purely because that name
///    happened to also exist in scope -- a real, silent misresolution
///    (goto-definition on `CloseDate` would jump to some unrelated local
///    variable, not `Opportunity.CloseDate`), not a correct result this
///    change took away. `bind_new_expr` now recognizes this shape (only
///    for a real schema-object constructor target, via
///    `crate::resolve::sobject_field_init`) and resolves the LHS against
///    `SchemaIndex` instead, the same way `bind_field_expr` already does
///    for `object.field` access -- of those 81, 80 now correctly land in
///    `Resolution::SchemaObject`/`UnknownSchema` (untallied) and 1 was a
///    genuine project-local constructor call incidentally sharing the
///    same LHS-name shape, confirmed still `Resolved` correctly.
///    `BASELINE_UNRESOLVED` dropped far more dramatically, -8,545
///    (`83_950` -> `75_405`): the overwhelming majority of real
///    `new SObject(field = value, ...)` field names in NPSP had no
///    coincidentally-matching local/field to misresolve against, so they
///    were landing in the honest but wrong `Resolution::Unresolved`
///    instead -- this is the exact user-reported bug ("goto-definition on
///    a field set via `new Contact(...)` does nothing").
/// 7. Dynamic-SOQL bind-variable resolution (`crate::resolve::bind_dynamic_soql_binds`):
///    `Database.query`/`countQuery`/`getQueryLocator`'s string argument
///    is scanned for `:identifier` bind variables (tracing a variable
///    argument one hop back to its own literal/concatenation source, in
///    the same enclosing-block chain as the call site), each resolved
///    against local/parameter scope. Moved `BASELINE_RESOLVED` +37
///    (`205_225` -> `205_262`) -- real bind-variable references in NPSP
///    that previously had no reference recorded at all (string *content*
///    was never tokenized into anything the binder walked, so these
///    weren't even `Unresolved` -- they simply didn't exist as
///    references). `BASELINE_UNRESOLVED` unaffected, consistent with
///    that: nothing moved *out* of `Unresolved` here, since nothing was
///    ever recorded there for these in the first place.
/// 8. **Two real resolver bugs fixed, found by a new automated
///    consistency guard** (`crates/apex-binder/tests/resolution_consistency.rs`,
///    which independently re-derives a call-shaped `Resolved` reference's
///    actual arity/callee-name from the AST and checks them against the
///    resolved symbol's own declaration -- distinct from this file's own
///    "how many resolve" counts, this one asks "did the ones that
///    resolved resolve to something real"):
///    a. `narrow_by_overload`'s `pool.len() == 1` fast path used to fire
///       whenever exactly one same-named candidate existed at all, even
///       when that candidate's own arity didn't match the call -- e.g. a
///       class extending `Exception` (which implicitly gets four
///       synthesized constructors this binder doesn't model) declaring
///       its own single explicit 2-arg constructor confidently
///       "resolved" a 0-arg or 1-arg `new` call to that unrelated 2-arg
///       one. Fixed by only taking the fast path when the single
///       candidate came from the *arity-filtered* pool, not the
///       unfiltered fallback; an arity-empty pool now honestly reports
///       `Candidates` instead. Moved `BASELINE_RESOLVED` -3 (three real
///       `fflib_QueryFactory.InvalidFieldException` call sites in NPSP
///       were confidently wrong before this).
///    b. `bind_call_expr`'s outward lexical-nesting climb (for an
///       unqualified call from a nested class to a method on its outer
///       class) stopped at the first level with *any* same-named method,
///       never considering arity -- confirmed via a live deploy that
///       real Apex does *not* work this way (a nested class's own
///       single-arity overload does not shadow an unrelated-arity
///       same-name method on its outer class; the real compiler still
///       finds the outer one). Fixed to keep climbing past a level
///       unless that level has an arity-*matching* same-named method.
///       This one didn't move either baseline count (NPSP's one real
///       occurrence, `UTIL_Where.cls`'s `meetsCriteria`, was already
///       counted as `Resolved` before the fix -- just resolved to the
///       wrong, arity-mismatched symbol -- and stays `Resolved`,
///       correctly, after it).
/// 9. **The single largest `Unresolved` reduction in this table's whole
///    history, found by the `examples/unresolved_clusters.rs` diagnostic's
///    own top-ranked cluster.** `resolve_type_ref` checked
///    `SchemaIndex::object` for a bare declared-type reference (a field/
///    property/parameter/method-return type, a generic type argument)
///    but never consulted the stdlib index at all -- so `String`, `List`,
///    `Boolean`, `Database`, and every other real stdlib class used as a
///    *type* (`String s;`, `List<Contact>`, a parameter's own type)
///    stayed `Unresolved`, even though the identical name in *expression*
///    position (`String.isBlank(...)`) already resolved as
///    `StdlibMember` since the standard-library type-model work earlier
///    in this project's history. Fixed by threading `&StdlibIndex`
///    through `resolve_type_ref`/`bind_type_ref` and adding a
///    `stdlib.class(&name)` check mirroring the existing schema check,
///    right before the final `Unresolved` fallback -- the exact same
///    `Resolution::StdlibMember`-with-`member: None` shape
///    `bind_name_expr`'s bare-class-as-value case already used.
///    `BASELINE_UNRESOLVED` dropped **-46,429** (`75_405` -> `28_976`,
///    over 60% of the entire prior `Unresolved` count) with
///    `BASELINE_RESOLVED` unaffected (`StdlibMember` isn't tallied by
///    either counter). Deliberately does not (and cannot yet) resolve an
///    `Exception` subtype (`DmlException`, ...) used as a type: the
///    scraped stdlib snapshot has no entry for `Exception` or its
///    subtypes at all, since the real Apex Reference Guide only
///    documents them on grouped, empty-methods "Built-In Exceptions"-
///    style pages `apex_stdlib::standard_classes` already filters out --
///    a real, separate, pre-existing gap this fix doesn't touch.
/// 10. `crate::conversions::system_type_compatible` never checked "param
///     is a curated *scalar* (`Id`/`String`/...), argument is a real
///     schema object" -- only the reverse direction (param is an
///     object) and the `SObject`-accepts-any-object case were modeled,
///     so that specific pairing fell all the way through to `None`.
///     Confirmed against a real org before fixing: `Id someId =
///     aContactRecord;`/`String s = aContactRecord;` are both real
///     `Illegal assignment` compile errors, and a same-arity
///     `pick(Id)`/`pick(Contact)` pair (also confirmed nested one level,
///     via `pick(List<Id>)`/`pick(List<Contact>)`) called with a real
///     `Contact` value unambiguously resolves to the `Contact` overload
///     in both shapes. This is exactly the user-reported bug (a real
///     NPSP call, `BDI_DataImport_API.processDataImportRecords(diSettings,
///     new List<DataImport__c>{...}, isDryRun)`, stayed
///     `Resolution::Candidates` forever between its `List<DataImport__c>`-
///     and `List<Id>`-typed overloads, since nothing could ever
///     eliminate the `List<Id>` one). `BASELINE_RESOLVED` rose +252
///     (`205_259` -> `205_511`); `BASELINE_UNRESOLVED` unaffected
///     (`Candidates` isn't tallied by either counter, so a
///     `Candidates` -> `Resolved` move only ever changes this side).
/// 11. `resolve::bind_method_call_expr`'s `Ty::System` arm only ever
///     looked up a receiver's *exact* type name in `apex_stdlib`
///     (`stdlib.class("DataImport__c")` -> nothing, since a real object
///     is never itself a stdlib *class*), so it never found the generic
///     instance methods every real object actually has --
///     `get`/`put`/`getSObjectType`/`clone`/`addError`/`getErrors`/...
///     are all declared once on the scraped `SObject` class itself
///     (`apex_stdlib::standard_classes()` really does have a
///     `"SObject"`/`"System"` entry with these, confirmed by direct
///     inspection of `data/apex_reference.json` -- the data was already
///     there, just never consulted for anything but a literal `SObject`-
///     typed receiver). Fixed by falling back to `stdlib.class("SObject")`
///     whenever the receiver is confirmed to actually be a real object
///     (`self.schema.object`, not a name guess) and its own exact-name
///     lookup didn't already have the member. `BASELINE_UNRESOLVED`
///     dropped -804 (`28_976` -> `28_172`), all of it calls (confirmed
///     via a direct count: 6,097 -> 5,293 `Unresolved` `MethodCallExpr`/
///     `CallExpr`/`NewExpr` references specifically); `BASELINE_RESOLVED`
///     rose +8 (`205_511` -> `205_519`) from the same "argument/chain
///     type newly known" ripple effect documented in step 1.
const BASELINE_RESOLVED: usize = 205_519;
const BASELINE_UNRESOLVED: usize = 28_172;

#[test]
fn resolved_and_unresolved_counts_never_regress_from_their_pinned_baseline() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let program = BoundProgram::from_files(&root);

    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    for (_, resolution) in program.all_resolutions() {
        match resolution {
            Resolution::Resolved(_) => resolved += 1,
            Resolution::Unresolved => unresolved += 1,
            Resolution::Candidates(_)
            | Resolution::SchemaObject(_)
            | Resolution::UnknownSchema(_)
            | Resolution::StdlibMember(_) => {}
        }
    }

    assert!(
        resolved >= BASELINE_RESOLVED,
        "Resolved count dropped from {BASELINE_RESOLVED} to {resolved} ({} fewer) -- likely a \
         real resolution regression somewhere in crate::resolve/crate::inherit, not just \
         incidental drift: something that used to connect a reference to its declaration no \
         longer does. If this drop is expected (e.g. a grammar/AST change legitimately changed \
         how references are counted), update BASELINE_RESOLVED with a one-line note on why.",
        BASELINE_RESOLVED - resolved
    );
    assert!(
        unresolved <= BASELINE_UNRESOLVED,
        "Unresolved count rose from {BASELINE_UNRESOLVED} to {unresolved} ({} more) -- likely a \
         real resolution regression: something that used to resolve (or at least land in \
         Candidates/SchemaObject/UnknownSchema) no longer does. If this rise is expected, update \
         BASELINE_UNRESOLVED with a one-line note on why.",
        unresolved - BASELINE_UNRESOLVED
    );
}
