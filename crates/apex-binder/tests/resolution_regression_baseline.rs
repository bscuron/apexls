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
const BASELINE_RESOLVED: usize = 205_305;
const BASELINE_UNRESOLVED: usize = 83_954;

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
