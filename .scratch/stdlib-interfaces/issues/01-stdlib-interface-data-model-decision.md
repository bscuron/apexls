Type: grilling
Status: resolved

## Question

Design the mechanism by which `apex-binder` records "this project-local class's `implements` clause names a real standard-library interface" as first-class data, general enough for any of the 68 stdlib interfaces in `apex_reference.json` (not a hardcoded async-job list), and wire `crates/apex-binder/src/dead_code.rs` and `crates/apex-binder/src/visibility_narrowing.rs` to consult it so a method that only satisfies a *stdlib* interface's contract (e.g. `execute()` on a `Database.Batchable`) stops being false-flagged as dead code / a visibility-narrowing candidate.

The map's Notes carry the load-bearing prior art -- read them first. In short: `resolve_inheritance`/`SymbolTable` (`crates/apex-binder/src/inherit.rs`) silently discards every `implements` name that fails to resolve as project-local; `apex-stdlib`'s `StdlibClass` has no public `kind` field to distinguish an interface from a class; `StdlibIndex` already has the accessors needed for namespace-qualified lookup, just unused for this purpose; and this codebase already has a precedent for "something real with no `SymbolId` to key by" (`reference_table.rs`'s `Resolution::StdlibMember`/`ExternalKey::Stdlib`) that a synthetic-`SymbolId`-into-`inherited_chain` design would not follow.

Decide, concretely:

1. **The unresolved-implements signal.** What's the minimal `resolve_inheritance`/`SymbolTable` change to record which raw `implements` names failed to resolve as project-local, per type (mirroring the existing single-slot `unresolved_direct_super`, `symbol_table.rs:135,384-396`)? Confirm whether `raw_extends`'s positional conflation of `extends`/`implements` origin (`collect.rs`) needs a real per-name clause tag fixed at collection time for this to work correctly, or whether a position-based workaround (subtracting the already-known direct-`extends` name) is acceptable for v1.

2. **The stdlib cross-reference lookup and data shape.** Given an unresolved `implements` name, how does it get checked against `apex-stdlib`'s data -- namespace-qualified dotted-name splitting into `StdlibIndex::class`/`class_in_namespace`, and a restored `kind` field on `StdlibClass` (threaded from `RawClass::kind`) to confirm it's specifically an `Interface`, not a `Class`/`Enum`. What's the actual new structure that holds "this project-local type implements these stdlib interfaces" -- keyed how, stored where (a new field on `SymbolTable`'s `Indices`, or elsewhere), following the `Resolution::StdlibMember`/`ExternalKey` precedent rather than minting synthetic `SymbolId`s.

3. **The exemption check itself.** Exactly how `dead_code.rs`'s reference-counting exemption and `visibility_narrowing.rs`'s `overrides_or_implements` each consult the new structure: for a candidate method, does its name+arity match a required method of any stdlib interface named in its containing class's `implements` clause? Confirm this never needs the cross-interface attribution that `Schedulable`/`Queueable`/`Finalizer`'s `execute()` collision would require for the (out-of-scope) missing-implementation diagnostic, since exemption only ever checks membership against the specific interfaces a class itself declares.

4. **What's explicitly deferred.** Confirm in the answer that `conversions.rs`/`resolve.rs`/call-hierarchy are not wired to the new mechanism by this ticket (map's Not yet specified), that `Messaging.InboundEmailHandler`/`Database.Stateful`/`Database.AllowsCallouts` stay unscraped gaps for implementation time to handle (design must degrade gracefully -- an unrecognized `implements` name is simply not a stdlib interface as far as this mechanism can tell, same as today), and note the expected validation approach for whoever implements (re-run ticket 18's NPSP frequency methodology: before, N false positives on `dead_code`/`visibility_narrowing` for classes implementing a scraped stdlib interface; after, 0).

## Answer

Design locked via a two-round grilling session plus a domain-modeling check-in. Concretely, in implementation order:

**1. `apex-stdlib`: restore `kind` as a real enum.** Add `pub enum StdlibKind { Class, Interface, Enum }` and `pub kind: StdlibKind` on `StdlibClass` (`crates/apex-stdlib/src/lib.rs:87-96`), threaded from `RawClass::kind` in `to_stdlib_class`. Safe because `standard_classes()`'s existing filter (`kind.as_str()` in `"Class" | "Interface" | "Enum"`, line 228) already guarantees only those three values ever survive onto a real `StdlibClass`.

**2. `apex-binder::inherit::resolve_inheritance`: fold the stdlib cross-reference directly into the existing per-type pass.** No intermediate "unresolved supertype names" field is persisted (the prior `apex-diagnostics` ticket 18 sketch's two-step design was considered and rejected -- nothing in this ticket's scope needs raw unresolved names preserved as their own queryable field, only the final "implements these stdlib interfaces" answer is ever consulted). Where `resolve_inheritance`'s `direct` map construction (`inherit.rs:55-61`) currently calls `resolve_supertype_name` and silently drops a name that fails to resolve as project-local, also try that name against `StdlibIndex::class`/`class_in_namespace` right there (splitting a dotted name like `Database.Batchable` into namespace + tail), keeping the match (as `&'static StdlibClass`) only when its `kind` is `Interface`. This produces one new per-type "direct stdlib matches" set.

Then, transitively propagate: while `flatten()` walks `direct` to build each type's `inherited_chain` (`inherit.rs:124-145`), also union each visited ancestor's own direct-stdlib-matches into the result -- so a class implementing a project-local interface that itself extends a stdlib interface still inherits that stdlib interface's exemption, exactly the same transitivity `inherited_chain` itself already has. The final result is one new field on `Indices`/`SymbolTable`:

```rust
stdlib_implements: FxHashMap<SymbolId, Vec<&'static StdlibClass>>
```

set via `set_inherited_chain`'s own sibling pattern, alongside the existing `inherited_chain`/`direct_super`/`subtypes` outputs of the same pass.

**3. `apex-binder::symbol_table::SymbolTable`: one new query method**, next to `inherited_chain`/`lookup_member`:

```rust
pub fn implements_stdlib_interface_method(&self, container: SymbolId, name: &str, arity: usize) -> bool
```

True if any `StdlibClass` in `stdlib_implements(container)` has a method matching `name` case-insensitively (matching `StdlibIndex`'s own convention) whose `params.len() == arity`. Arity (parameter count) only, no type-checking -- matching `visibility_narrowing.rs`'s own existing `overrides_or_implements` precedent (`visibility_narrowing.rs:105-112`), and confirmed sufficient: the exemption only ever needs to check membership against the specific stdlib interfaces a class's own supertype list names, never cross-interface attribution (the real `Schedulable.execute`/`Queueable.execute`/`Finalizer.execute` name+arity collision only matters for attributing *which* interface a method satisfies, needed by the out-of-scope missing-implementation diagnostic, not by this exemption).

**4. Two consumers, two different kinds of change** (this was the key correction found this session against the map's framing -- see below):

- `visibility_narrowing.rs`'s `overrides_or_implements` (`visibility_narrowing.rs:95-113`) already walks `inherited_chain` structurally (no reference needed) -- it gets one more `||` branch calling `implements_stdlib_interface_method` after its existing walk finds nothing. A genuine extension of an existing check.
- `dead_code.rs` has **no existing structural interface-satisfaction exemption at all** -- confirmed by reading the whole file: its only interface-related exemption is reference-based (`crate::resolve::expand_dynamic_dispatch` widening a *real project call site* through an interface-typed value, `dead_code.rs:980-1007`), which can never fire for a platform-invoked callback like `Database.Batchable.execute()` since no project code ever calls through a `Database.Batchable`-typed value -- the platform does. So this needs a **brand-new exemption channel** in `dead_code.rs`, structurally parallel to its existing `has_platform_invocation_annotation`/`is_visualforce_referenced`/`is_platform_invoked_test_method` channels (`dead_code.rs:53-258`): one more condition in `dead_symbols_in_file`'s existing `Public`-candidate filter (`dead_code.rs:294-298`) that skips the reference-count check entirely when `implements_stdlib_interface_method` is true. Only reachable for `Public` candidates already, since Apex forbids a non-public interface method.

**5. Explicitly deferred / degrades gracefully.** `conversions.rs`/`resolve.rs`/call-hierarchy are not wired to `stdlib_implements`/`implements_stdlib_interface_method` by this ticket (map's Not yet specified). `Messaging.InboundEmailHandler`/`Database.Stateful`/`Database.AllowsCallouts` stay unscraped: `StdlibIndex::class` simply returns `None` for them today, so a class implementing one of these gets no entry in `stdlib_implements` and no exemption -- same silent-miss behavior as any other unrecognized name, not a crash or special case. Validation for whoever implements: re-run ticket 18's NPSP frequency methodology (before: N false positives on `dead_code`/`visibility_narrowing` for the 121 real NPSP classes implementing a scraped stdlib interface; after: 0 for every scraped interface, unchanged for the 3 unscraped ones).

**Domain-modeling check-in**: no new CONTEXT.md glossary entries (apexls's glossary today is scoped to the diagnostic/fix pipeline only; this stays as code-level doc comments, per the user). One ADR recorded: `docs/adr/0001-stdlib-interface-implementation-as-parallel-index.md`, capturing why a parallel `stdlib_implements` index was chosen over minting synthetic `SymbolId`s for stdlib interfaces.

This fully settles all four of the ticket's numbered sub-decisions. Nothing implementation-shaped happens as part of resolving this ticket, per the map's Notes.
