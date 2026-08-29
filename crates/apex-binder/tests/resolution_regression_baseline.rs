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
/// 12. `apex_stdlib::standard_classes()` had no entry at all for
///     `Exception` -- the real page documenting its common methods
///     (`getMessage`, `setMessage`, `getCause`, ...) is laid out too
///     differently from a normal method-reference page for the scraper's
///     table walker to extract, so it came through with `kind: "Unknown"`
///     and zero methods, which `standard_classes()`'s own filter then
///     excluded entirely. Every custom exception subclass has `Exception`
///     as its base, so this single gap was outsized: `extends Exception`
///     itself never resolved, and neither did any inherited `Exception`
///     method call from within such a subclass. Fixed in two parts: (a)
///     hand-corrected the one bundled JSON entry directly (`kind` ->
///     `"Class"`, `name` -> `"Exception"`, real methods/constructors
///     populated -- verified against a live connected org via `sf apex
///     run`, not guessed; see `apex_stdlib::standard_classes`'s own doc
///     comment), which alone fixes `extends Exception`'s own `Type`-kind
///     reference (`resolve::resolve_type_ref`'s existing `stdlib.class(&name)`
///     fallback now finds it); (b) added a new, more general fallback --
///     `resolve::stdlib_class_via_unresolved_supertype`, wired into
///     `bind_method_call_expr`'s `Ty::Project` arm -- for a project type's
///     own *inherited* member lookup, which previously had no fallback at
///     all beyond this project's own `SymbolTable`: when a project type's
///     `extends` name never resolved as a project symbol in the first
///     place (`SymbolTable::unresolved_direct_super`, new), but the name
///     *is* a real stdlib class, an inherited call now resolves against
///     that stdlib class's own members instead of staying `Unresolved`
///     forever. Not specific to `Exception` -- any project type extending
///     an unresolvable-but-real stdlib base benefits the same way.
///     `BASELINE_RESOLVED` rose +545 (`205_519` -> `206_064`);
///     `BASELINE_UNRESOLVED` dropped -3,535 (`28_172` -> `24_637`).
/// 13. `resolve::record_qualified_segments`'s per-segment resolution (the
///     one `BoundProgram::resolution_at` checks before falling back to a
///     dotted `Type` node's own whole-reference resolution -- see that
///     function's doc comment) only ever tried this project's own
///     `SymbolTable`, never `StdlibIndex`, even though the *whole*
///     reference (`resolve_type_ref`'s separate, later fallback) already
///     did. A namespace-qualified system type used as a declared type
///     (`Schema.SObjectType token;`, extremely common real Apex) resolved
///     fine as a whole, but clicking (or, after step 1's diagnostic,
///     getting a squiggle on) either individual segment reported
///     `Unresolved` regardless -- confirmed a real, high-volume gap via a
///     real fflib_QueryFactory.cls, not a rare edge case: 34 such
///     references in that one file alone, 68 per-segment `Unresolved`
///     entries between them. Fixed by giving the first segment a
///     `StdlibIndex::class` fallback (a namespace like `Schema`/`System`
///     is very often a real class in its own right, e.g.
///     `Schema.getGlobalDescribe()`) and the second segment a
///     `StdlibIndex::class_in_namespace` one (mirroring
///     `resolve::bind_field_expr`'s identical fallback for this same
///     shape in *expression* position, `Schema.SoapType.ID`, added in the
///     same pass as this diagnostic). Only tried for the segment
///     immediately after the first: real Apex namespace-qualified
///     references are always exactly `Namespace.Class`, never deeper.
///     `BASELINE_RESOLVED` barely moved (+4, `206_064` -> `206_068`,
///     `StdlibMember` isn't tallied by either counter, so this fallback's
///     own hits mostly don't show up here at all); `BASELINE_UNRESOLVED`
///     dropped a further -6,437 (`24_637` -> `18_200`), all project-wide
///     instances of the exact same per-segment gap.
/// 14. Two more real, general gaps, found the same way as steps 12-13
///     (dogfooding against a real file, this time `fflib_AppBindingResolver.cls`):
///     (a) `type_of_symbol`'s generic-type-argument resolution
///     (`Map<System.Type, System.Type> bindings;`'s own `System.Type`
///     arguments) never got the `class_in_namespace` fallback its
///     *outer* declared type already has a few lines below in the same
///     function -- a namespace-qualified stdlib type as a *generic
///     argument* specifically stayed the literal unsplit dotted string
///     (`"System.Type"`, never a real `StdlibIndex` key), so every
///     further hop off a `.get(...)`-substituted argument
///     (`this.bindings.get(interfaceType).newInstance()`) stayed
///     `Unresolved` even though the field's own top-level `Map` type
///     resolved fine. (b) The `X.class` reflection idiom
///     (`fflib_IAppBinding.class`) had no handling at all outside the
///     unrelated `List<Foo>.class` generic-collection form -- `class` is
///     a reserved word, never a real declared member, so `bind_field_expr`'s
///     ordinary member lookup always missed it regardless of the
///     receiver's own type. Fixed with an early, receiver-type-agnostic
///     check in `bind_field_expr` resolving straight to the real
///     `System.Type` class. `BASELINE_RESOLVED` unchanged (`206_068`,
///     `StdlibMember` again not tallied); `BASELINE_UNRESOLVED` dropped
///     -837 (`18_200` -> `17_363`).
/// 15. Five more real, general gaps, third file dogfooded
///     (`fflib_SObjectDomain.cls`): (a) `Trigger` had no `apex_stdlib`
///     entry at all -- same scraper-extraction gap as `Exception` (step
///     12), a real page found but its content (the trigger context
///     variables: `new`/`old`/`newMap`/`oldMap`/`isBefore`/.../`operationType`)
///     never captured as structured properties. Hand-corrected the same
///     way, 13 real properties, verified against a live org. (b)
///     `SymbolTable::lookup_member` had no notion of a field/property
///     *shadowing* a same-named ancestor member at all (only a same-
///     *arity* `override` method did) -- so a subclass declaring its own
///     `static` member with the same name as an unrelated one on its
///     supertype (`fflib_SObjectDomain extends fflib_SObjects`, both
///     independently declaring `static ... Errors`) landed in
///     `Resolution::Candidates` forever, permanently ambiguous, even
///     though a real compiler resolves it to the more-derived one without
///     any ambiguity. Fixed generally: a non-method match at any chain
///     level now stops the walk before reaching further ancestors, real
///     Apex field-hiding semantics. (c) `inherit::resolve_inheritance`'s
///     `extends`/`implements` name resolution (`SymbolTable::resolve_dotted_name`)
///     had no enclosing-chain fallback for an unqualified *sibling*
///     nested-type name at all -- `class ObjectError extends Error`,
///     where `Error` is a sibling nested class (both declared directly
///     inside `fflib_SObjectDomain`), never resolved as a supertype, so
///     `Error`'s own inherited fields stayed permanently unreachable from
///     `ObjectError`. New `SymbolTable::resolve_dotted_name_from`, mirroring
///     `resolve_type_ref`'s identical single-segment enclosing-chain
///     fallback, wired into both `raw_super`/`raw_extends` resolution.
///     (d) `Expr::Index` (`list[0]`) never propagated a `List<T>`'s own
///     element type at all (a documented "v1" gap) -- fixed the same way
///     `crate::generics`'s own `"list"`/`"get"` arm already does, `[...]`
///     being `.get(...)`'s own syntax sugar. (e) A schema field access
///     outside SOQL only ever got an inferred `Ty` for a *relationship*
///     field (via its `reference_to`) -- every *scalar* field (`opp.Name`,
///     `opp.Type`, ...) had none at all, so a chained call on it
///     (`opp.Name.equals(...)`) always stayed `Unresolved`, an extremely
///     common real pattern, not an edge case. New
///     `resolve::apex_type_for_schema_field_type` maps a field's own
///     metadata `field_type` string to its real Apex type, conservatively
///     (only clear, `sf`-verified cases; the scraped standard-schema
///     `field_type` strings are real prose, not a clean enum, confirmed
///     by direct inspection). Also fixed, same file: `sobjectExpr.Field.addError(msg)`,
///     a real, documented Apex compiler idiom (confirmed against a real
///     org) with no real method to find on the field's own scalar type.
///     `BASELINE_RESOLVED` rose +559 (`206_068` -> `206_627`);
///     `BASELINE_UNRESOLVED` dropped -6,336 (`17_363` -> `11_027`).
/// 16. Fourth file (`fflib_SObjectUnitOfWorkTest.cls`), four more real,
///     general gaps: (a) `SObjectTypeName.SObjectType` (`Opportunity.SObjectType`,
///     `Schema.Opportunity.SObjectType`) -- another compiler-magic
///     universal property (confirmed against a real org), this time on
///     any real SObject type name, plus the matching `bind_field_expr`
///     fallback that lets `Schema.Opportunity` itself resolve as a real
///     schema object in the first place. (b) A `catch (Type e)` clause's
///     own variable was declared with `declare_local(..., None)`,
///     discarding its type *entirely* regardless of whether the
///     exception type itself was ever modeled -- so `e.getMessage()`
///     stayed `Unresolved` inside *every* catch block, unconditionally,
///     not just for unmodeled types. New `declare_local_with_type_name`
///     (a catch clause's exception name parses as a `QualifiedName`, not
///     a `Type`, so `declare_local` itself can't be reused directly).
///     (c) The stdlib fallback added in step 12/14 for a project type's
///     own inherited method call only ever computed the right
///     `Resolution`, never the right *result type* (`result_type_of`
///     only ever handles a project `SymbolId`'s own type, never
///     `StdlibMember`) -- so the call itself resolved, but a further
///     chained call right after it (`caughtEx.getMessage().contains(...)`)
///     stayed `Unresolved` regardless -- a real bug in that fallback's
///     own first version, not a pre-existing gap. Fixed by computing the
///     winning overload's own return type directly, the same way the
///     `Ty::System` arm already does for every other stdlib call. (d) A
///     *built-in* Apex exception subtype (`DmlException`, `System.DmlException`)
///     has no `apex_stdlib` entry at all and structurally never will
///     (Salesforce's docs cover these only in prose, never their own
///     class/method reference page) -- new fallback in
///     `bind_method_call_expr`'s `Ty::System` arm: since every real Apex
///     exception class name ends in literally `Exception` (a hard
///     compiler rule, confirmed against a real org: "Classes extending
///     Exception must have a name ending in Exception"), an unmodeled
///     `*Exception`-named type falls back to `Exception`'s own methods.
///     `classify_unresolved` (`apexls-server`) extended to match: a
///     `Type`-kind reference shaped this way is now classified structural
///     (`WARNING`) rather than the default (`ERROR`), the same "this
///     binder can never individually confirm this, and never will"
///     reasoning `QualifiedName` already gets. Also, dogfooding this file
///     surfaced one more `"Unknown"`-kind `apex_stdlib` entry
///     (`"Email Class (Base Email Methods)"`, real content, just
///     misclassified) -- while fixing it, found and fixed 5 more of the
///     same shape project-wide (`AuditParamsRequest`, `ReferencedRefundRequest`,
///     `SalesforceResultCodeInfo`, `IntegrationTest`, `RemoteObjectController`),
///     leaving only 2 of the original ~8 still excluded (see
///     `apex_stdlib::standard_classes`'s own doc comment for why those
///     two specifically still need more thought before a safe rename).
///     `BASELINE_RESOLVED` rose +88 (`206_627` -> `206_715`);
///     `BASELINE_UNRESOLVED` dropped -1,872 (`11_027` -> `9_155`).
/// 17. Custom labels (`.labels-meta.xml`, real Apex `Label.<name>`/
///     `System.Label.<name>` syntax) were never discovered at all --
///     `apex-discover`'s prune list skipped the `labels/` directory
///     outright (unlike `objects`/`fields`/`pages`, which already had a
///     kept-metadata-dirs exception), so no project ever had its custom
///     label *names* modeled, even though `Label` itself was already a
///     real, resolving stdlib class. New `apex_metadata::LabelSchema` +
///     `apex_binder::LabelIndex` (mirrors `SchemaIndex`'s discovery/build
///     shape, minus the bundled-standard-snapshot merge `SchemaIndex` has
///     -- there's no such thing as a "standard" label) wired into
///     `bind_field_expr`'s existing `Ty::System { name: "Label", .. }`
///     receiver check. `BASELINE_UNRESOLVED` dropped -2,058 (`9_155` ->
///     `7_097`): the overwhelming majority of real `Label.<name>` reads in
///     NPSP now resolve as `Resolution::Label` (untallied here, same as
///     `StdlibMember`/`SchemaObject`). Deliberately does **not** resolve
///     the `Label.<namespace>.<name>` cross-package form
///     (`System.Label.npo02.DefaultHouseholdName`, real Apex syntax for
///     disambiguating a label declared in a specific installed package) --
///     confirmed via direct inspection that this remainder is exactly
///     that shape, not a new gap this change introduced: SFDX's
///     `.labels-meta.xml` files never record a package's own namespace, so
///     there's no local metadata to resolve that segment against, the
///     same class of gap `apex-metadata`'s own module doc comment already
///     documents for standard schema. `BASELINE_RESOLVED` rose +1
///     (`206_715` -> `206_716`), the same "argument/chain type newly
///     known" ripple effect documented in step 1.
/// 18. Visualforce page references (`Page.<name>`, real Apex compiler-
///     magic syntax -- `PageReference pr = Page.MyPage;`), found the same
///     way as step 17: `examples/unresolved_clusters.rs`'s top clusters,
///     re-run after step 17 landed. Unlike `Label`, there is no real
///     "Page" class anywhere in Salesforce's own docs at all (confirmed:
///     no such `apex_reference.json` entry), so the bare `Page` identifier
///     itself has nothing to resolve to -- detected instead from the
///     receiver's own raw token text in `bind_field_expr` (new
///     `apex_binder::PageIndex`, keyed by a `.page` file's own file-stem
///     name; no content parsing needed at all, unlike `LabelIndex`, since
///     a page's name *is* its file's base name). `classify_unresolved`
///     (`apexls-server`) extended to grade the bare `Page` identifier's
///     own still-`Unresolved` outcome `WARNING` rather than `ERROR` --
///     without that, every real `Page.<name>` reference would still show
///     one spurious `ERROR` apiece even after the reference as a whole
///     resolves correctly. `BASELINE_UNRESOLVED` dropped -117 (`7_097` ->
///     `6_980`); `BASELINE_RESOLVED` unaffected (`VisualforcePage` isn't
///     tallied by either counter, same as `Label`/`StdlibMember`).
/// 19. Two related schema-describe-token shapes in `bind_field_expr`,
///     found the same way as step 18: `examples/unresolved_clusters.rs`'s
///     top clusters, re-run after that step landed, then a real-corpus
///     file (`AdditionalObjectJSON_TEST.cls`) verified for both shapes at
///     once. (a) `<ObjectType>.<Field>` (a bare SObject *type* name, not
///     an instance, dotted with a field API name -- `DataImport__c.Account1Imported__c.getDescribe()`)
///     is real, documented Apex shorthand for a `Schema.SObjectField`
///     describe token, confirmed against a real org
///     (`Schema.SObjectField f = Account.Name;` compiles,
///     `f.getDescribe()` works) -- but `bind_field_expr` always
///     propagated the field's own scalar/relationship type regardless of
///     whether the receiver was a bare type name or a real instance,
///     correct for the instance case (`acct.Name` really is a `String`)
///     but wrong here, where the chained `.getDescribe()`/`.getName()`/
///     `.getLabel()` call always stayed `Unresolved` (a `Boolean`/
///     `String`/... has no such method). Fixed by checking the
///     receiver's own already-recorded resolution (a bare type name
///     resolves as `Resolution::SchemaObject` with no field at all,
///     `bind_name_expr`'s own `self.schema.object(name)` fallback -- an
///     instance variable never resolves that way) and propagating
///     `Schema.SObjectField` instead whenever it matches. (b) The
///     *reversed* order, `SObjectType.<ObjectName>` (bare, not
///     `Schema.SObjectType.<ObjectName>`), is a genuinely *different*
///     compiler-magic idiom from (a) and from `<ObjectName>.SObjectType`
///     (an existing, already-handled fallback) -- confirmed against a
///     real org that it evaluates to a `Schema.DescribeSObjectResult`,
///     not a `Schema.SObjectType` token (`SObjectType.Account`'s live
///     runtime type dumped as `Schema.DescribeSObjectResult`). Fixed with
///     a dedicated early check in `bind_field_expr`, mirroring the
///     `Label`/`.class` checks' placement. Measured independently: (a)
///     alone moved `BASELINE_RESOLVED` +23 (`206_716` -> `206_739`) and
///     `BASELINE_UNRESOLVED` -189 (`6_980` -> `6_791`, the same "argument/
///     chain type newly known" ripple effect documented in step 1); (b)
///     on top of (a) moved `BASELINE_RESOLVED` +1 more (`206_739` ->
///     `206_740`) and `BASELINE_UNRESOLVED` -148 more (`6_791` ->
///     `6_643`).
/// 20. The `object.fields.<FieldName>` describe-token shorthand, found
///     the same way as steps 18-19: `examples/unresolved_clusters.rs`'s
///     top clusters, re-run after step 19 landed (300+ occurrences each
///     of two related receiver forms). `fields` itself isn't a real
///     property of anything -- there's no single declaration to point at
///     -- so `bind_field_expr` already resolved it as
///     `Resolution::UnknownSchema` via the generic
///     `self.schema.object(&object).is_some()` fallback, but propagated
///     no `Ty` at all, so a chained `.fields.<FieldName>` hop always
///     stayed `Unresolved` regardless of receiver. Confirmed against a
///     real org that the *same-looking* `.fields.<FieldName>` means a
///     genuinely different result type depending on the receiver: off a
///     bare object type name, `Schema.SObjectField f = Account.fields.Name;`
///     compiles (a real `Schema.SObjectField` token); off the
///     `SObjectType.<ObjectName>` describe-result receiver from step 19,
///     `Schema.SObjectField f2 = Schema.SObjectType.Account.fields.Name;`
///     instead fails with "Illegal assignment from Schema.DescribeFieldResult
///     to Schema.SObjectField" -- a real `Schema.DescribeFieldResult`.
///     Fixed with two internal-only synthetic `Ty` names (never exposed
///     via `Resolution`, safe from ever colliding with a real class name
///     since `$` isn't a legal Apex identifier character) that carry both
///     which mode applies and the owning object's name forward from the
///     `.fields` hop into the next one -- reusing `Ty::System`'s existing
///     `args` field for that, the same "one field left to carry extra
///     context" reuse step 19's own `SObjectType.<ObjectName>` fix
///     needed. Caught one real disambiguation bug during this step's own
///     verification: a first version distinguished the two receiver
///     modes by checking the receiver's own already-recorded resolution
///     shape (`Resolution::SchemaObject` with no field), the same signal
///     step 19's own fix reuses -- but that signal is genuinely ambiguous
///     here, since the `SObjectType.<ObjectName>` describe-result
///     receiver records that *exact* same shape for an unrelated reason,
///     so a `SObjectType.Account.fields.Name` fixture resolved against
///     the literal object name `"DescribeSObjectResult"` instead of
///     `"Account"`. Fixed by checking the receiver's own `Ty` name
///     directly instead (a real schema object name means token mode;
///     `"DescribeSObjectResult"` means describe mode, recovering the real
///     owner from `args`), an unambiguous signal the resolution shape
///     alone couldn't provide. `BASELINE_UNRESOLVED` dropped -412
///     (`6_643` -> `6_231`); `BASELINE_RESOLVED` rose +1 (`206_740` ->
///     `206_741`).
///
/// 21. Legacy `Type[]` array sugar (`Object[]`, `String[]`, ...) never
///     accounted for `apex_syntax::ast::Type::is_array` anywhere: both
///     `resolve::resolve_type_ref` (an expression's/declared type's
///     inferred `Ty`) and `collect::type_ptr_and_name` (a `Symbol`'s
///     cached `type_name`/`type_args`, used by overload narrowing) read
///     an array type's base name and generic args exactly as if the `[]`
///     weren't there, since `Type::text()`/`base_name_tokens()`/
///     `type_args()` all already strip it (real NPSP shape:
///     `fflib_Match.gatherMatchers(Object[] ignoredMatcherObjects)`
///     calling `.isEmpty()`/`.size()` on the parameter -- `Object[]`
///     resolved as bare `Object`, which has neither method, so both
///     calls landed in `Unresolved`). Fixed by treating `Type[]`
///     identically to `List<Type>` at both call sites. Also flipped one
///     overload resolution from an accidental correct guess to a
///     principled one: `UTIL_Query.withSelectFields`'s `Set<String>` vs.
///     `String[]` overloads previously disambiguated only because a
///     `new String[]{...}` argument and the `String[]` parameter were
///     *both* miscounted as bare `String` (an incidental exact-string
///     match); now both are correctly `List<String>`, still uniquely
///     eliminating the `Set<String>` overload. `BASELINE_UNRESOLVED`
///     dropped -55 (`6_231` -> `6_176`); `BASELINE_RESOLVED` rose +662
///     (`206_741` -> `207_403`).
///
/// 22. A property's custom `set { ... }` accessor body can reference
///     `value`, an implicit parameter of the property's own type that
///     real Apex declares for it without it ever appearing in source
///     (real NPSP shape: `fflib_ApexMocks.DoThrowWhenExceptions`'s setter
///     assigning `methodReturnValueRecorder.DoThrowWhenExceptions =
///     value;`) -- previously unmodeled entirely, since
///     `resolve::bind_symbol_body`'s `SymbolKind::Property` arm always
///     bound every accessor body with an empty parameter list. Fixed by
///     collecting `value` as an ordinary `Parameter` symbol
///     (`collect::collect_property`), kept under the *property's* own id
///     as `container` (not the enclosing class's) so it stays invisible
///     to ordinary member lookup, then seeding it into just the `set`
///     accessor's body scope via the same `table.params` machinery a
///     real method already uses. `BASELINE_UNRESOLVED` dropped -19
///     (`6_176` -> `6_157`); `BASELINE_RESOLVED` rose +19 (`207_403` ->
///     `207_422`).
///
/// 23. Two unrelated, real gaps found from a corpus-wide error-diagnostic
///     sweep (clustering every `ERROR`-severity `Resolution::Unresolved`
///     reference, mirroring `capabilities.rs`'s own `classify_unresolved`
///     split), fixed together:
///
///     - An unqualified type name that exists *both* as a top-level
///       class and as a nested type of the lexically enclosing class
///       resolved to the wrong (top-level) one, because every one of
///       four separate lookup sites checked the project's flat
///       top-level name table before ever trying the lexically
///       enclosing scope, backwards from real Apex's own precedence --
///       confirmed empirically against a real org (`sf apex run`: a
///       nested `Widget` shadowed an unrelated top-level `Widget` from
///       inside its own outer class's method). Real NPSP shape:
///       `PSC_ManageSoftCredits_CTRL` declares its own nested
///       `SoftCredit`, but the project also has an unrelated top-level
///       `SoftCredit.cls` -- every `SoftCredit`-typed local, and every
///       `List<SoftCredit>`-typed property, silently bound to the wrong
///       class, so real members (`sc.partial`, `sc.contactRole`) fell to
///       `Unresolved`. Fixed at all four sites that independently
///       duplicated this same "top-level first" order:
///       `resolve::resolve_type_ref_base` (a live `Type` AST node),
///       `resolve::type_of_symbol` (a declared symbol's cached
///       `type_name`, both for the type itself and for each of its
///       generic type arguments), and `SymbolTable::resolve_dotted_name_from`
///       (an `extends`/`implements` clause).
///     - Every real SObject -- standard or custom -- inherits a handful
///       of base fields (`Id`, `OwnerId`, `CreatedDate`, `CreatedById`,
///       `LastModifiedDate`, `LastModifiedById`, `SystemModstamp`,
///       `IsDeleted`) that Salesforce's own docs describe once, in
///       prose, as common to every object, rather than repeating them
///       per object -- confirmed directly against the raw bundled
///       `standard_objects.json`: zero of Account/Contact/Opportunity/
///       OpportunityContactRole list an `Id` field. `"id"` alone was the
///       single most common unresolved reference name across the whole
///       corpus. Fixed with a small `'static` fallback table in
///       `SchemaIndex::field`, tried only once an object's own real
///       fields have already missed.
///
///     `BASELINE_UNRESOLVED` dropped -433 (`6_157` -> `5_724`);
///     `BASELINE_RESOLVED` rose +354 (`207_422` -> `207_776`).
const BASELINE_RESOLVED: usize = 207_776;
const BASELINE_UNRESOLVED: usize = 5_724;

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
            | Resolution::StdlibMember(_)
            | Resolution::Label(_)
            | Resolution::VisualforcePage(_) => {}
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
