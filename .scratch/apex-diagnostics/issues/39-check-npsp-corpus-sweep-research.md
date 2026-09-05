Type: research
Status: resolved

## Question

Run the newly-shipped `apexls check` subcommand (see the "Replace `apexls dead` with `apexls check`" commit) against the full real NPSP corpus (`tests/corpus/npsp`, the git submodule at `https://github.com/SalesforceFoundation/NPSP.git`) and separate what it reports into two buckets: genuine gaps in apexls's own binder/stdlib data (worth tickets), versus expected noise that isn't actually a tool defect (a real absence of metadata this corpus can't provide, or an intentionally-noisy check like `visibility_narrowing_diagnostics`/`dead_code_diagnostics` firing on a library's own public API). This map's own notes already establish the zero-false-positive bar every diagnostic here is held to (map.md's Notes section) -- this sweep is the same "benchmark against the real NPSP corpus" practice the map's Notes mandate, just run wholesale via the new `check` subcommand rather than per-ticket.

## Answer

`cargo build --release -p apexls`, then `apexls check tests/corpus/npsp` from the repo root (project root auto-detected via `tests/corpus/npsp/sfdx-project.json`, per `check.rs`'s `find_project_root`). Raw output: 9,806 lines, 4,087 `warning`s, 5,719 `error`s, across 458 of the corpus's 1,044 `.cls` files (44%).

### 1. Warnings: all expected library noise, zero new findings

Every warning sampled is `dead_code_diagnostics`/`visibility_narrowing_diagnostics` firing correctly on `fflib_*` classes (`force-app/infrastructure/apex-common`) -- a bundled third-party library (`ApexCommons`/`fflib`) vendored into this corpus whole. Its `public`/`protected` members are real API surface for external consumers the NPSP corpus itself never calls (e.g. `fflib_QueryFactory.getOffset()`, `fflib_SObjectDomain.onApplyDefaults()`) -- exactly the kind of "correct but not actionable for this specific corpus" case ticket 32's own research anticipated (a vendored library, not a project with a single closed consumer set). Not a tool defect: the diagnostics already only fire on genuinely zero-reference/narrower-than-declared symbols *within this corpus*; a library's whole point is public API nothing in its own repo calls. No further action -- this is the expected shape once a real vendored dependency is included in a scan, not new information.

### 2. Errors: two are real, high-impact binder/stdlib gaps; the rest is an unavoidable corpus data gap

`error` count by shape (`sed -E "s/'[^']*'/X/g"` over every `: error:` line):

| count | shape |
|---|---|
| 3,790 | `cannot resolve reference to X` |
| 1,876 | `X is not a valid field on object X` |
| 53 | `X is not a valid object` |

#### 2a. Real gap: `<ObjectType>.SObjectType` drops the object name, breaking every `.fields`/`.fieldSets` hop chained after it (highest-impact single bug)

`resolve.rs` models the `SObjectType` compiler-magic idiom in **both written orders**, but only one carries the receiver's own identity forward:

- `SObjectType.<ObjectName>` (`resolve.rs:2642-2657`, the *reversed* order, e.g. `SObjectType.Account`) returns `Ty::system_with_args("DescribeSObjectResult", vec![Ty::system_owned(object_name, ...)])` -- the object name rides along in `args`.
- `<ObjectName>.SObjectType` (`resolve.rs:2510-2527`, the *canonical*, far more common order, e.g. `Account.SObjectType`) returns `class.map(|c| Ty::system_owned(c.name.clone(), Vec::new()))` -- a bare `Ty::System { name: "SObjectType", args: [] }` with **no object name at all**.

The very next hop, `.fields` (`resolve.rs:2686-2730`), only recognizes a receiver whose `Ty::System.name` is either a real schema object name or literally `"DescribeSObjectResult"`; for the canonical-order case the receiver's name is `"SObjectType"` itself, so `owner` resolves to `None` and the whole special-case is skipped, falling through to a generic stdlib-member lookup that also fails (`Schema.SObjectTypeFields` isn't in the stdlib snapshot at all -- see 2b). The result: any real-world `Account.SObjectType.fields....`/`Contact.SObjectType.fields....` chain -- the idiomatic, everywhere-in-NPSP way to get a field-describe token or a fields map -- dead-ends as `Unresolved` starting at `.fields`, and every further hop off it (`.getMap()`, `.name`, `.get(...)`, `.keySet().contains(...)`) cascades into its own `cannot resolve reference to` error.

Confirmed directly against real NPSP source, e.g. `force-app/infrastructure/apex-common/test/classes/fflib_SObjectDescribeTest.cls:62`:
```
System.assertEquals(fields.get('name'), Account.SObjectType.fields.name);
```
and `force-app/infrastructure/apex-common/main/classes/fflib_SObjectUnitOfWork.cls:804`:
```
Boolean relatedHasExternalIdField = relatedObject.getDescribe().fields.getMap().keySet().contains(externalIdFieldName.toLowerCase());
```
(here the receiver is `relatedObject.getDescribe()`, i.e. the already-described `DescribeSObjectResult` path, which *does* carry the object name correctly through `.getDescribe()` -- the break is specifically in the `<ObjectName>.SObjectType` bare-type-token path, not every route to `.fields`.)

This single asymmetry, not any one-off per-file issue, accounts for the large majority of both the `cannot resolve reference to` bucket (`'name'` 685 hits, `'getMap'` 91, `'get'` 76, `'contains'` several dozen, `'size'`/`'toLowerCase'`/`'fields'` more, all downstream of a broken `.fields`/`.fieldSets` chain) and a meaningful share of `X is not a valid field on object X` (any `.fields.<FieldName>` shorthand that also breaks this way degrades further down the line). Fix: type `<ObjectName>.SObjectType` (`resolve.rs:2510-2527`) the same way the reversed order already does -- `Ty::system_with_args("SObjectType", vec![Ty::system_owned(object_name, ...)])` instead of a bare `Ty::system_owned(c.name.clone(), Vec::new())` -- and widen the `.fields` owner-recovery at `resolve.rs:2686` to also accept `object.eq_ignore_ascii_case("SObjectType")` with a carried arg (today it only special-cases `"DescribeSObjectResult"`).

#### 2b. Real gap: `Schema.SObjectTypeFields`/`Schema.SObjectTypeFieldSets` (`.fields`/`.fieldSets`'s own real result types) are entirely absent from the stdlib snapshot

Independent of 2a: `crates/apex-stdlib/data/apex_reference.json` has a `DescribeSObjectResult` entry whose own `getFields()` method is scraped correctly (`"signature": "public Schema.SObjectTypeFields getFields()"`), but there is **no `SObjectTypeFields` entry anywhere in the JSON** (confirmed by loading the raw JSON directly and searching every `name` field, per this project's own established practice of checking raw scraped data rather than inferring from struct shape). Same for `SObjectTypeFieldSets`. So even once 2a is fixed so `.fields` correctly types as the describe-token synthetic marker, a direct `.getMap()`/any other real method call on the *describe-result* variant of `.fields` (as opposed to the `.fields.<FieldName>` token-shorthand ticket 01 already modeled) has no stdlib member data to resolve against -- `resolve.rs:2686-2730`'s special-casing only ever handles the one-hop `.fields.<FieldName>` shape, never a `.fields.getMap()`/`.fields.getSObjectType()`-style real method call on the fields-map object itself. Fixing this needs both a stdlib-scrape addition (these two classes' own real Salesforce doc pages exist and are scrapable, matching ticket 18's precedent of finding entirely-unscraped stdlib classes) and a small extension to the `.fields`/`.fieldSets` special-case to fall through to an ordinary stdlib method lookup on the newly-added class when the next hop isn't a bare field-name token.

#### 2c. Real gap: Apex enum instances' built-in members (`.name()`, `.ordinal()`, static `.values()`) are entirely unmodeled

Every Apex enum constant implicitly carries `name()` (returns the constant's own name as `String`) and `ordinal()` (its declaration-order `Integer`), and every enum type itself implicitly carries a static `values()` returning `List<EnumType>` -- real, always-present Apex language members, not stdlib data, similar in kind to how a class always has an implicit no-arg constructor. Grepped `crates/apex-binder/src` end to end for any handling of these (enum-instance-method special-casing the way `.fields`/`SObjectType`/`Page`/`Label` each get their own intercept in `resolve.rs`): none exists. `SymbolKind::Enum`/`SymbolKind::EnumConstant` are collected (`collect.rs:295-330`) and typed for member-access dispatch like any other project type, but nothing ever injects `name`/`ordinal`/`values` into an enum's member set the way a real Apex compiler does. Confirmed against real NPSP call sites, e.g. `force-app/main/default/classes/STG_SettingsManager_TEST.cls:42`:
```
RD2_Constants.InstallmentCreateOptions.Disable_First_Installment.name()
```
and `force-app/infrastructure/apex-extensions/main/application/dynamic/classes/fflib_AppBindingMetaDataModule.cls:80` (a static `SomeEnum.values()` call). This is a structural binder gap independent of NPSP -- it would misfire identically on any Apex project using enums, one of the most common language features. Fix belongs in `apex-binder`'s member-resolution path (likely alongside the existing per-`SymbolKind` special-casing in `resolve.rs`/`completion.rs`): for a receiver typed as a project `Enum` symbol, synthesize `name()`/`ordinal()` as available instance methods and `values()` as an available static method, all zero-false-positive since Apex enums have no way to *not* have these.

#### 2d. Real, smaller gap: `ApexPages.Severity` (and its `ERROR`/`WARNING`/`INFO`/`CONFIRM`/`FATAL` constants) is missing from the stdlib snapshot even though `ApexPages`'s own scraped methods reference it

`apex_reference.json`'s `ApexPages` entry (`page_id: apex_methods_system_apexpages`) has a method `hasMessages(ApexPages.Severity severity)` whose own param `type_name` is literally `"ApexPages.Severity"` -- but no top-level entry named `Severity` exists anywhere in the JSON at all. The scraper captured every method that *references* the nested enum type without ever visiting/emitting the nested enum's own definition page. Confirmed against real NPSP call sites across 47 distinct files (`ADDR_CopyAddrHHObjBTN_CTRL.cls:152`, etc.): `ApexPages.Severity.ERROR` / `.WARNING` / `.INFO` -- an extremely common Visualforce-controller-error-messaging idiom, contributing the `'Severity'` (178), `'ERROR'` (119), `'WARNING'` (31) entries in the `cannot resolve reference to` tally. Same class of fix as ticket 18's stdlib-interface gaps: add the missing nested-enum scrape entry (and its constant values) to `apex_reference.json`.

#### 2e. Not a tool gap: legacy managed-package (`npe01`/`npe03`/`npe4`/`npe5`/`npo02`) schema genuinely has no metadata anywhere in this corpus

The remaining large share of both `cannot resolve reference to` (every `npe5__Affiliations_Settings__c`/`npo02__User_Rollup_Field_Settings__c`/`npe03__Custom_Field_Mapping__c`-shaped name, 25+ distinct legacy-namespaced object/field names) and essentially all of `X is not a valid field on object X` / `X is not a valid object` (`Contact`/`Account` extension fields like `npe01__SYSTEM_AccountType__c`, `npo02__Household__c`; `npe01__OppPayment__c`'s *own* core fields like `Paid__c`/`Opportunity__c`/`Payment_Amount__c`/`Payment_Method__c`) trace back to real, permanent absences in the NPSP GitHub source itself, confirmed by direct filesystem check, not a apex-metadata parsing defect:

- Whole custom-settings objects such as `npe5__Affiliations_Settings__c` have **no `objects/` directory at all** anywhere under `tests/corpus/npsp` (`find ... -iname "*Affiliations_Settings*"` returns nothing).
- `tests/corpus/npsp/force-app/main/default/objects/npe01__OppPayment__c/fields/` exists and is correctly discovered (32 real field files: `ACH_Code__c`, `Elevate_Payment_ID__c`, `DebitType__c`, ...) but contains **none** of the object's own original, pre-Elevate-integration core fields (`Paid__c`, `Opportunity__c`, `Payment_Amount__c`, `Payment_Method__c`, `Written_Off__c`, `Check_Reference_Number__c`, `Scheduled_Date__c`) -- these are the legacy `Npe01` managed package's own base-package fields, which predate NPSP's later open-sourcing of an *unpackaged extension* on top of that package, and were never migrated into this repo's own source tree.
- Same shape for `Account`'s `npe01__SYSTEM_AccountType__c`/`npe01__One2OneContact__c` and `Contact`'s `npo02__Household__c`/`npo02__Household__r` -- no `.field-meta.xml` exists anywhere in the corpus for any of them.

`apex_discover::discover` (`crates/apex-metadata/src/discover.rs:64`) walks the whole detected project root regardless of `sfdx-project.json`'s `packageDirectories` (confirmed: no code anywhere reads `packageDirectories` at all), so this isn't a package-directory-scoping bug either -- the metadata is simply not present in the git submodule checkout, full stop. In a real deployed org these legacy managed packages are installed alongside NPSP's own unpackaged metadata, contributing schema no source-only corpus can ever see. Not actionable as an apexls fix; worth remembering the next time a "not a valid field on object 'npe01__OppPayment__c'"-shaped finding shows up, so it isn't mistaken for a new regression.

## Summary for follow-on tickets

Four real, ticketable gaps, roughly in impact order:

1. **2a** (highest impact): `<ObjectName>.SObjectType` loses the object name in its `Ty`, breaking every `.fields`/`.fieldSets` hop after it -- `resolve.rs:2510-2527` vs. the correctly-carried reverse order at `resolve.rs:2642-2657`.
2. **2b**: `Schema.SObjectTypeFields`/`Schema.SObjectTypeFieldSets` missing from the stdlib snapshot, plus the `.fields`/`.fieldSets` special-case never falling through to a real method lookup (only the token-shorthand hop).
3. **2c**: enum instances' implicit `name()`/`ordinal()`/static `values()` are entirely unmodeled in `apex-binder`.
4. **2d**: `ApexPages.Severity` (nested enum + constants) missing from the stdlib snapshot despite being referenced by other scraped `ApexPages` method signatures.

2e is a real, permanent corpus limitation (missing legacy-managed-package metadata), not a tool defect -- documented here so it isn't re-investigated as a suspected regression later.
