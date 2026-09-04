Type: research
Status: resolved

## Question

Investigate the concrete mechanics needed to design `visibility_narrowing_diagnostics`: a diagnostic that flags a `public`/`protected` field, property, method, or constructor whose actual usage (proven via `apex-binder`'s existing `ReferenceTable`/`inherited_chain`/`subtypes` machinery) only requires a narrower declared visibility, and suggests the symbol's **required visibility** -- the narrowest `Visibility` value (`crates/apex-binder/src/symbol.rs:72-79`) its real references prove is legally sufficient. This mirrors `dead_code_diagnostics` (used-nowhere) but is its inverse: used, just more narrowly than declared.

Settled already, out of scope for this ticket to re-litigate:
- In scope: `Public` -> narrower, `Protected` -> `Private`. Out of scope: `Global` (no namespace model exists to prove it safe, same reasoning `dead_code.rs` already uses to exclude `Global` from dead-code candidacy).
- Member kinds in v1: fields, properties, methods, constructors. Nested/inner class visibility deferred to a follow-on.
- Reuses `dead_code_diagnostics`'s existing exemption list verbatim: members reachable only via `@AuraEnabled`/`@InvocableMethod`/`@InvocableVariable`/`@RemoteAction`/`@Http*` annotations, or Visualforce-page-referenced classes, are never flagged.
- Any method that overrides a virtual/abstract member or satisfies an interface contract is excluded from candidacy entirely (Apex forbids narrowing visibility on override/interface-satisfying methods).
- Severity: plain `WARNING`, no `DiagnosticTag`.

This ticket is fact-finding only -- it does not decide the algorithm/architecture. Its findings feed a follow-on decision ticket. Establish, concretely:

- **Mapping a reference back to its containing declaration.** `ReferenceTable::by_symbol` (`crates/apex-binder/src/reference_table.rs:309`) stores `FxHashMap<SymbolId, Vec<SyntaxPtr>>` -- a `SyntaxPtr` locates a reference's own syntax position, but does not directly say "which class/type declares the code this reference sits inside." What's the cheapest existing way to recover that (e.g. does `apex-binder` already have a syntax-ptr-to-enclosing-symbol lookup used elsewhere, or would this need a new walk up the tree per reference)? This is the load-bearing primitive the whole "which bucket does this reference fall into" computation depends on.
- **Same-file nested/inner-class access to `private` members.** `dead_code.rs`'s existing rationale for scoping `Private` candidates to `references_to_in_file` calls private "file-scoped per Apex's one-top-level-type-per-file rule" -- but a file can contain the top-level type *plus* nested/inner types. Confirm, against a real connected org via the `sf` CLI (this project's established oracle practice for disputed real-compiler-behavior questions -- see `apex-grammar-oracle-sf-cli`), whether a nested type can access its outer type's `private` members and vice versa. If so, "declaring class" for this diagnostic's bucketing needs to mean "the file's whole type family," not the literal declaring class -- confirm this doesn't already silently affect `dead_code_diagnostics`'s own `Private` candidacy today (a real, possibly pre-existing gap worth surfacing even if out of this ticket's own scope to fix).
- **Overlap with `dead_code_diagnostics`.** A symbol with *zero* references anywhere is `dead_code_diagnostics`'s territory. Should `visibility_narrowing_diagnostics` simply skip zero-reference candidates entirely (deferring to dead-code, avoiding double-flagging the same symbol from two different diagnostics), or is there a real scenario where both should fire on the same symbol? Check whether the two diagnostics, run together over the real NPSP corpus, would ever double-flag under the "skip zero-reference" rule as currently understood.
- **The `subtypes`/`inherited_chain` primitives' actual shape for this use.** `SymbolTable::subtypes(id)` (`symbol_table.rs:406-408`) and `SymbolTable::is_visible_from` (`symbol_table.rs:461-490`) already exist and are described (per prior survey) as directly reusable for a "protected could be private" check. Confirm their exact signatures/semantics (transitive vs. direct-only, whether they're already keyed the way this diagnostic would need) and give concrete usage sketches, not just a description.
- **Real NPSP corpus signal.** Run a rough, throwaway scan (doesn't need to be production code) over the real NPSP corpus to get an approximate count of how many `public`/`protected` members would be candidates under the settled scope -- this project's standing practice benchmarks every new binder-adjacent check against real data before design, not just at implementation time (see ticket 11's cross-method-bulkification research for precedent: a rough corpus count materially changed that ticket's own architecture decision).

## Answer

### 1. Mapping a reference back to its containing declaration

**No such lookup exists today.** Grepped/read `reference_table.rs`, `incremental.rs`, and `lib.rs` in full: `ReferenceTable` (`reference_table.rs:295-343`) stores exactly three maps -- `resolutions: FxHashMap<SyntaxPtr, Resolution>`, `by_symbol: FxHashMap<SymbolId, Vec<SyntaxPtr>>`, `by_external: FxHashMap<ExternalKey, Vec<SyntaxPtr>>` -- plus `highlight_ranges`/`bind_var_spans`, none of which record a reference's *enclosing* declaration, only the reference's own position and what it resolved to. `FileBodies` (`incremental.rs:80-88`, the per-file Pass 2 output `ReferenceTable`s are merged from) is equally bare: `{ refs: ReferenceTable, scopes: FxHashMap<SyntaxPtr, ScopeTree>, type_mismatches: Vec<TypeMismatch> }` -- no enclosing-declaration map anywhere in the persisted, per-file cache.

The information *does* exist, but only transiently, at bind time: `resolve::BodyBinder` (`resolve.rs:800-822`) carries `enclosing_type: Option<SymbolId>` and `enclosing_member: Option<SymbolId>` for the one body currently being walked (set once per `bind_body`/`bind_trigger_body`/`bind_initializer` call, `resolve.rs:829-936`), and every reference recorded via `refs.set(...)` inside that call happens to be inside that same enclosing type/member -- but `BoundBody` (`resolve.rs:729-734`) never carries `enclosing_type`/`enclosing_member` back out, so this fact is discarded the instant one body's binding finishes and its `refs`/`scopes` are folded into the shared `ReferenceTable`/`FileBodies` (`reference_table.rs:458-476`'s `map_ids_into`, called once per body).

The closest *existing* primitive is `BoundProgram::enclosing_callable` (`lib.rs:1157-1194`): given a raw file offset, it walks `token_at_offset` up via `.ancestors().find(...)` to the nearest `MethodDecl`/`ConstructorDecl` node, then does a **linear scan** over `symbols_of_file(file)` matching that node's `TextRange` against each symbol's own `ptr.range()` to recover a `SymbolId`. This is real precedent for "walk a syntax node's ancestors to find its enclosing declaration, then map that declaration node back to a `SymbolId` by range-equality against `symbols_of_file`" -- but it's narrowly scoped to `Method`/`Constructor` only (it exists for `call_hierarchy::incoming_calls`'s "who called this, from where" need, `lib.rs:1157-1165`'s own doc comment), it's O(symbols-in-file) per call (no index), and it takes a raw offset, not an existing `SyntaxPtr`.

**Concrete answer: this needs a new walk per reference, but it's a small, direct generalization of `enclosing_callable`'s own established pattern**, not something built from scratch:

```rust
// lives inside apex-binder (SymbolId::new is pub(crate) only, symbol.rs:30) --
// e.g. alongside a new visibility_narrowing.rs, mirroring dead_code.rs's
// own placement/module shape.
fn declaring_type_of_reference(program: &BoundProgram, reference: SyntaxPtr) -> Option<SymbolId> {
    let file = reference.file();
    let root = program.syntax(file);
    let node = reference.to_node(&root)?;
    let type_node = node.ancestors().find(|n| {
        matches!(
            n.kind(),
            apex_syntax::SyntaxKind::ClassDecl
                | apex_syntax::SyntaxKind::InterfaceDecl
                | apex_syntax::SyntaxKind::EnumDecl
        )
    })?;
    let range = type_node.text_range();
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .find(|(_, s)| {
            matches!(s.kind, SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum)
                && s.ptr.range() == range
        })
        .map(|(local, _)| SymbolId::new(file, local as u32))
}
```

This deliberately matches `ClassDecl`/`InterfaceDecl`/`EnumDecl` (`apex_syntax::SyntaxKind`, confirmed real variant names at `crates/apex-syntax/src/ast/decl.rs:45-47`), not `Method`/`Constructor` like `enclosing_callable` does -- the diagnostic needs "which *type*," and `.ancestors()` walking innermost-to-outermost means a reference sitting inside a nested class's method finds the **nested** class first, not the outer one, which matters a lot for §2 below.

**Usage sketch** for the diagnostic's actual bucketing step, per candidate symbol:

```rust
for candidate_id in public_and_protected_candidates() {
    let declaring_type = program.symbols.get(candidate_id).container; // already stored, symbol.rs:200
    let mut referencing_types: Vec<SymbolId> = Vec::new();
    for reference in program.references_to(candidate_id) {
        if let Some(t) = declaring_type_of_reference(program, reference) {
            referencing_types.push(t);
        }
    }
    // referencing_types now feeds the is_visible_from-shaped narrowing
    // check in §4.
}
```

**Cost caveat worth flagging for the follow-on design ticket**: this walk is O(references-of-candidate) per candidate, each doing a fresh `.ancestors()` climb plus an O(symbols-in-file) linear scan (same cost shape `enclosing_callable` already pays). §5's real corpus numbers show this is bounded (5,590 real candidates, most with a handful of references each, none with references numbering anywhere near "every reference in the project"), so a lazy per-candidate walk at diagnostic-run time is very likely fine -- but the alternative (threading `enclosing_type`/`enclosing_member` into `ReferenceTable::set` itself, paying the cost once per reference project-wide during Pass 2) was tried for a conceptually similar problem before and rejected on cost grounds: `reference_table.rs:374-385`'s own doc comment on `stored_highlight_range` documents that eagerly storing a *second* range per reference for the common `NameExpr`/`Type`/`QualifiedName` case "measurably regressed bind time when tried." This ticket doesn't decide between "walk lazily per diagnostic run" vs. "store eagerly at bind time" -- flagging it as a real tradeoff the next ticket should weigh explicitly, with `reference_table.rs`'s own precedent as a warning against defaulting to the eager option.

### 2. Same-file nested/inner-class access to `private` members

Checked via the `sf` CLI against a real connected org (`bscuron19.f9f273f0b808@agentforce.com`, confirmed connected via `sf org list`), following this project's own established precedent (ticket 02's methodology, verbatim): a scratch SFDX project outside the repo checkout, one throwaway Apex class deployed at a time via `sf project deploy start --source-dir ... -o org --json`, inspecting the returned `componentFailures`/`componentSuccesses` JSON. The two shapes that deployed successfully (`numberComponentsDeployed: 1`) were cleaned up afterward via `sf project delete source --metadata "ApexClass:VisNarrow32Probe" -o org --no-prompt --json` (confirmed `deletedSource` in the response); the failing shapes never created anything server-side (`numberComponentsDeployed: 0`), matching ticket 02's own "no cleanup needed for a rejected deploy" finding.

Four shapes tried, all against one outer class `VisNarrow32Probe` with a nested `public class Nested { ... }` (`Inner` and `outer` are both **reserved identifiers** in real Apex -- confirmed empirically: `"Identifier name is reserved: Inner"` / `"Identifier name is reserved: outer"` -- worth remembering for anyone writing Apex test fixtures):

1. **Outer accesses nested's `private` instance field/method through an object reference** (`Nested n = new Nested(); n.innerSecretMethod() + n.innerSecret;`, both `private`) -- **compiles.**
2. **Nested accesses outer's `private` instance field/method *unqualified*** (`outerSecretMethod() + outerSecret` from inside a `Nested` instance method, no explicit outer reference) -- **fails**: `"Method does not exist or incorrect signature: void outerSecretMethod() from the type VisNarrow32Probe.Nested"` and `"Variable does not exist: outerSecret"`. This is a real, structural finding, not a visibility error: Apex nested classes get **no implicit outer-instance reference** (no Java-style enclosing `this`), so an unqualified name inside a nested-class instance method simply never resolves to anything on the outer instance at all -- it's a *name resolution* miss, not an *access-control* rejection.
3. **Nested accesses outer's `private` instance field/method through an *explicit* qualified reference** (`callOuterPrivateFromInnerQualified(VisNarrow32Probe ownerRef) { return ownerRef.outerSecretMethod() + ownerRef.outerSecret; }`) -- **compiles.** Confirms the restriction in (2) is purely about implicit-`this` name resolution, not `private` access control: once there's an explicit reference to reach through, `private` doesn't block it.
4. **Nested accesses outer's `private` *static* field/method unqualified** (`outerSecretStaticMethod() + outerSecretStatic` from inside `Nested`, both `private static` on the outer) -- **compiles**, no qualification needed. (Separately confirmed, and worth noting as a real Apex constraint independent of this ticket: a *nested* class itself cannot declare `static` members at all -- `"static can only be used on fields of a top level type"` -- so "outer accesses nested's private static member" isn't even a producible scenario; the only relevant outer<->nested direction for statics is the one just described.)

**Concrete conclusion**: `private` in Apex is genuinely scoped to the whole file's type family (outer + every nested/inner type), exactly as `dead_code.rs`'s own doc comment already claims ("private is genuinely file-scoped by Apex's own visibility rules," `dead_code.rs:9-11`) -- confirmed empirically here for the first time rather than assumed. The only real subtlety is *syntactic*, not about visibility: an unqualified reference from a nested class to an outer instance member doesn't resolve at all (no implicit outer `this`), while every other combination (qualified instance access either direction, unqualified static access either direction where legal) compiles cleanly.

**Does this already silently affect `dead_code_diagnostics`'s own `Private` candidacy today? No, confirmed clean.** Every one of the legal cross-nested-class `private` references above (1, 3, 4) is a real AST reference node (`FieldExpr`/`MethodCallExpr`) that Pass 2 binds and records into `ReferenceTable` like any other -- and because of Apex's own one-top-level-type-per-file rule, a nested type's `Symbol.file` is always the same `FileId` as its outer type's (every `Symbol` collected while walking one file, `collect.rs`'s whole per-file `FileCollection` shape, `symbol.rs:188`'s `pub file: FileId`). So `dead_code.rs:291`'s `program.references_to_in_file(file, *id)` check for `Private` candidates already sees every one of these references -- it's scoped by *file*, not by *literal declaring-class identity*, so nesting depth inside that one file is irrelevant to it. **No fix is needed in `dead_code_diagnostics`; the ticket's suspected gap does not exist.**

**Where this *does* matter, concretely, is for `visibility_narrowing_diagnostics`'s own new bucketing logic from §1.** `declaring_type_of_reference` (§1) returns the *literal, innermost* enclosing type (`Nested`, not `VisNarrow32Probe`, for a reference written inside `Nested`'s own method body) -- so a naive "is the referencing type == the candidate's own `container`" comparison would treat shape (3)/(4) above (a real, 100%-legal same-file cross-nested-type `private` access) as if it came from an unrelated outside type, which is the wrong signal for a *visibility-narrowing* check specifically (it would never wrongly suggest widening, but could wrongly conclude "this reference proves the member needs broader-than-Private visibility" when the real compiler is fine with `private`). The resolver's own `SymbolTable::is_visible_from` already gets this right today for the *actual* resolution/access-control question, by using `top_level_of` (whole-family identity, not literal container identity) for its `Private` case (`symbol_table.rs:465-472`) -- so the concrete, actionable finding for the follow-on design ticket is: **the new bucketing/narrowing logic must compare `top_level_of(declaring_type_of_reference(...))` against `top_level_of(candidate's container)` (or reuse `subtypes`/`is_visible_from`-shaped logic, see §4), never raw `SymbolId` container equality, or it will misjudge same-file nested-class references.** `top_level_of` itself is a private `fn` on `SymbolTable` (`symbol_table.rs:422-433`, not `pub`/`pub(crate)`) -- `dead_code.rs:208-211` already hit this exact wall and reimplemented it locally (3-line container walk) rather than expose new crate-wide surface for it; the new diagnostic should do the same, following that precedent directly rather than modifying `SymbolTable`'s own visibility.

### 3. Overlap with `dead_code_diagnostics`

`dead_code.rs`'s own `is_dead_code_candidate_kind` (`dead_code.rs:77-88`) restricts candidacy to `matches!(symbol.modifiers.visibility, Visibility::Private | Visibility::Public)` -- **`Protected` is structurally excluded from dead-code candidacy entirely**, confirmed both by that match arm and by the module's own doc comment: `"Protected"`/`"Global"` stay outside candidacy too... `Protected` is a reasonable, cheap future extension... not built yet" (`dead_code.rs:36-40`). This changes the overlap analysis by visibility:

- **`Public` candidates**: `dead_code.rs:288-289` decides deadness via exactly `program.references_to(*id).next().is_none()` -- the same `BoundProgram::references_to` primitive `visibility_narrowing_diagnostics` would use to decide "skip, zero references, defer to dead-code." Under a rule of "only consider a candidate once it has at least one reference," the two diagnostics' `Public` candidate sets are **disjoint by construction**, not by luck of any particular corpus: dead-code's `Public` candidates all have zero references; narrowing's `Public` candidates (under that rule) all have >=1. No double-flag is possible on a `Public` member, ever, as long as that skip rule is actually enforced.
- **`Protected` candidates**: since `dead_code_diagnostics` never even considers `Protected` regardless of reference count, there is (trivially) also zero double-flag risk here -- but this surfaces a real, **pre-existing, structural gap** worth naming explicitly for the design ticket: a totally unused (zero-reference) `protected` member is caught by **neither** diagnostic today, and would still be caught by neither if `visibility_narrowing_diagnostics` blindly inherits the same "skip zero-reference, defer to dead-code" rule for `Protected` too -- there's nothing to defer *to*. This isn't a regression this ticket introduces (dead-code's own doc comment already flagged `Protected` dead-code detection as a known, unbuilt future extension), but the design ticket needs to make a deliberate choice rather than copy the `Public` rule uncritically: either (a) accept the gap as pre-existing/out-of-scope, or (b) have `visibility_narrowing_diagnostics` still fire "requires: Private" on a zero-reference `Protected` candidate (arguably strictly more useful than silence, since it's actionable) instead of skipping it. **Not settled here -- flagged for the follow-on ticket.**

**Empirically confirmed over the real NPSP corpus** (methodology and full numbers in §5): a throwaway scan cross-referencing every `Public`/`Protected` candidate's `program.references_to(id).next().is_some()` against the real `dead_symbols_in_file`'s actual flagged set (matched by `(FileId, name_range)`, since `DeadSymbol` doesn't carry a `SymbolId`) found **zero** symbols that were simultaneously non-zero-reference *and* dead-code-flagged -- confirming no double-flag risk in practice, not just in theory, across 1,070 real corpus files.

### 4. The `subtypes`/`inherited_chain`/`is_visible_from` primitives' actual shape

All three read directly from `crates/apex-binder/src/inherit.rs` (`resolve_inheritance`, `inherit.rs:32-91`) and `symbol_table.rs`:

- **`SymbolTable::inherited_chain(id: SymbolId) -> &[SymbolId]`** (`symbol_table.rs:367-372`): every `SymbolId` **transitively** reachable from `id` via resolved `extends`/`implements`, flattened and cycle-guarded by `flatten` (`inherit.rs:130-145`, a visited-set-guarded stack walk, not recursion -- safe against `class A extends B; class B extends A` or any longer cycle). **Does not include `id` itself** (`flatten`'s own doc comment, `inherit.rs:124-129`: "Does not include `type_id` itself -- callers wanting 'this type or an ancestor' ... prepend it themselves," exactly what `lookup_member` does at `symbol_table.rs:520-521`).
- **`SymbolTable::subtypes(id: SymbolId) -> &[SymbolId]`** (`symbol_table.rs:406-408`): **also transitive**, confirmed directly in `resolve_inheritance` (`inherit.rs:71-90`): it's built as `direct`'s (the forward `extends`/`implements` adjacency map) *reverse* graph, then run through the exact same `flatten` used for `inherited_chain` (`inherit.rs:84-90`: `flatten(&direct_subtypes, super_id)`). So `subtypes(id)` is every type transitively `extends`/`implements`-ing `id`, not just direct subtypes -- and, like `inherited_chain`, **does not include `id` itself**. Only keyed for types that have at least one direct subtype at all (`supertypes_with_subtypes`, `inherit.rs:83`) -- `subtypes(id)` on a leaf type correctly returns `&[]` via `symbol_table.rs:407`'s `.map_or(&[], ...)`, not a missing-key panic.
- **`SymbolTable::is_visible_from(candidate: SymbolId, from: Option<SymbolId>) -> bool`** (`symbol_table.rs:461-490`): checks `candidate`'s **actual currently-declared** `Visibility`, not a hypothetical one -- `Public`/`Global` always `true`; `Private` (absent `@TestVisible`) requires `top_level_of(candidate) == top_level_of(from)` (`symbol_table.rs:471-472`, using the private `top_level_of` walk, `symbol_table.rs:422-433`, §2 above); `Protected` additionally allows `from == candidate.container` **or** `inherited_chain(from).contains(&candidate.container)` (`symbol_table.rs:484-487`) -- and `inherited_chain(from).contains(declaring)` is exactly the mirror-image statement of `subtypes(declaring).contains(from)` (both express "`from` is a transitive subtype of `declaring`"), so **`subtypes` and `is_visible_from`'s own `Protected` branch already encode the identical relationship from opposite ends** -- confirmed by direct code comparison, not inferred.

**Important, concrete caveat for the follow-on ticket**: `is_visible_from` is **not directly reusable as-is** to answer "would visibility X be sufficient," because it's hardcoded to `candidate_symbol.modifiers.visibility` -- the member's *actual*, currently-declared visibility, not a hypothetical narrower one being tested. There is no `is_visible_from_as(candidate, hypothetical_visibility, from)` variant today. A real implementation has two options, neither built yet: (a) add a small parameterized variant of `is_visible_from` to `SymbolTable` that takes a `Visibility` argument instead of reading `candidate_symbol.modifiers.visibility`, or (b) have the diagnostic reimplement the same three-way disjunction directly against each reference's own `declaring_type_of_reference` result, without going through `is_visible_from` at all. Given (b) only needs `top_level_of`-equivalent logic (already precedent-reimplemented locally in `dead_code.rs`, §2) plus `subtypes`, it's the lower-blast-radius option -- but this ticket doesn't decide it, just confirms the gap exists.

**Usage sketch, `Protected -> Private` check** (using `declaring_type_of_reference` from §1 and a locally-reimplemented `top_level_of`, following `dead_code.rs:208-211`'s precedent):

```rust
fn protected_can_narrow_to_private(program: &BoundProgram, candidate_id: SymbolId) -> bool {
    let declaring = program.symbols.get(candidate_id).container.unwrap();
    let declaring_top_level = top_level_container(program, declaring); // dead_code.rs-style reimpl
    program.references_to(candidate_id).all(|reference| {
        declaring_type_of_reference(program, reference)
            .map(|from| top_level_container(program, from) == declaring_top_level)
            .unwrap_or(false) // a reference we can't place is conservatively "doesn't narrow"
    })
}
```

**Usage sketch, `Public -> narrower` (three-way) check**:

```rust
fn required_visibility(program: &BoundProgram, candidate_id: SymbolId) -> Visibility {
    let declaring = program.symbols.get(candidate_id).container.unwrap();
    let declaring_top_level = top_level_container(program, declaring);
    let subtype_set: &[SymbolId] = program.symbols.subtypes(declaring); // already transitive, §4

    let mut needs_protected = false;
    for reference in program.references_to(candidate_id) {
        let Some(from) = declaring_type_of_reference(program, reference) else {
            return Visibility::Public; // can't place it, don't guess -- stays Public
        };
        if top_level_container(program, from) == declaring_top_level {
            continue; // same file family -- Private-safe
        }
        if from == declaring || subtype_set.contains(&from) {
            needs_protected = true; // real cross-family access, but only via inheritance
            continue;
        }
        return Visibility::Public; // referenced from an unrelated type -- can't narrow at all
    }
    if needs_protected { Visibility::Protected } else { Visibility::Private }
}
```

### 5. Real NPSP corpus signal

Ran a throwaway integration test (`crates/apex-binder/tests/ticket32_visibility_narrowing_scan.rs`, deleted before this commit, per this project's own ticket-11-precedent convention for this kind of fact-finding scan) against the real NPSP checkout at `tests/corpus/npsp`, pinned commit `3dc817c94e3f2d41d461f6a79dd9e83c279ded9c`. **Corpus-checkout caveat, worth being explicit about**: the submodule's initial `git submodule update --init` hit Windows' `MAX_PATH` limit on a handful of unrelated, long-named metadata-translation/validation-rule XML files (`error: unable to create file ... Filename too long`) -- but `git submodule status` afterward showed the pin checked out clean (no `+`/`-` prefix), and a direct file count came back **1,044 `.cls` + 26 `.trigger` = 1,070 files**, exactly matching `tests/corpus/manifest.toml:20`'s own documented `"~1070 .cls/.trigger files"` -- confirming the handful of failed long-path files were unrelated non-Apex metadata, and the Apex source set itself is complete.

The scan replicated `dead_code.rs`'s real exemption logic inline (`has_platform_invocation_annotation`/`is_visualforce_referenced`/`is_platform_invoked_test_method`, matching `dead_code.rs:65-75,172-184,229-236,90-120` respectively) plus a rough interface-satisfaction check (same-name/case-insensitive, same-arity method declared on an `Interface`-kind ancestor in `inherited_chain(container)`) and an `is_override` check, over every `Method`/`Field`/`Property`/`Constructor` symbol with `Visibility::Public`/`Visibility::Protected`. Results:

| | total | after exemptions/override/interface-satisfy | of those: zero-ref | non-zero-ref |
|---|---|---|---|---|
| `Public` | 7,850 | 5,590 | 636 | 4,954 |
| `Protected` | 181 | 162 | 10 | 152 |

So under the settled scope, roughly **5,752 members** (5,590 + 162) are real `visibility_narrowing_diagnostics` candidates project-wide before the reference-narrowing check itself runs, of which **5,106** (4,954 + 152) have at least one real reference and would actually reach the narrowing logic in §4 (the other 646 would be skipped under the §3 "zero-reference, defer to dead-code" rule). This is a large, real signal opportunity -- even if only a modest fraction of that 5,106 genuinely narrows (this ticket doesn't attempt to answer *how many actually would*, that requires the real `declaring_type_of_reference`/narrowing-check implementation from §§1/4, out of this fact-finding ticket's scope), the candidate pool itself is unambiguously large enough to be worth building.

**One real, worth-flagging finding from this scan's first pass**: before the `is_platform_invoked_test_method` exemption was added, the scan reported dozens of "GAP" cases -- a `Public` zero-reference member that `dead_code_diagnostics` did *not* flag as dead, which would have contradicted §3's "disjoint by construction" claim. Spot-checked one directly against the real source: `ADDR_Validation_Gateway_TEST.cls:58`'s `public static testMethod void testOneAddress()` -- a `public`, legacy-`testMethod`-modifier Apex test method, correctly exempted by `dead_code.rs`'s own `is_platform_invoked_test_method` (`dead_code.rs:90-120`, checks `symbol.modifiers.is_testmethod` before trusting a zero-reference count) since the platform's test runner invokes it directly with no textual call site to find. This was a gap in the *rough scan's* own exemption logic (it initially omitted this one check dead_code.rs itself applies), not a real gap in `dead_code_diagnostics` -- confirmed by adding the same exemption to the scan, after which **zero** GAP cases remained and the "zero double-flag" cross-check (§3) came back clean. Concrete, actionable takeaway for the follow-on implementation ticket: `visibility_narrowing_diagnostics` must reuse **all three** of `dead_code.rs`'s exemption checks (`has_platform_invocation_annotation`, `is_visualforce_referenced`, **and** `is_platform_invoked_test_method`), not just the annotation/VF pair the ticket's settled scope names explicitly -- a `public`/`protected` `@isTest`/`testMethod`-modifier method with a genuinely narrow real reference footprint would otherwise be misjudged as narrowable (or, worse, as a `dead_code_diagnostics` double-flag candidate) by a scan that only checks the two-item exemption list as literally written.
