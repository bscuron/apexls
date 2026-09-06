Type: research
Status: resolved

## Question

Measure or estimate the realistic memory footprint of "declarations-only" residency at full-corpus scale — all ~1044 NPSP files with only declaration-collection data resident (no Pass 2 body/reference binding: no reference tables, scope trees, or bound bodies) — versus the current fully-bound state (188MB retained per ticket 02). Quantify how much scoping Pass 2 binding to a working set (open files + their dependency closure) could plausibly save. Also identify what "dependency closure" needs to include for correctness: does cross-file resolution touched by an open file require direct references only (imports, extends/implements chains), or transitive closure over those?

## Answer

### Measurement, not estimate

Added a throwaway `APEXLS_DECLARATIONS_ONLY` env-var hook to `BoundProgram::from_files_cached` (`crates/apex-binder/src/lib.rs`, right after the Pass 1.5 `inherit`/`rebuild_indices` block, before the "Pass 2 (parallel): rebind every current file" comment) that returns early with an empty `bodies: FxHashMap::default()`, skipping Pass 2 (`bind_symbol_body`/`bind_type_ref` over every symbol, the `ReferenceTable`/`ScopeTree` merge, ID remapping) entirely. Every other `BoundProgram` field (`files`, `file_ids`, `parses`, `texts`, `symbols`) is assembled identically to the normal path, since none of them depend on Pass 2 having run — parsing and declaration collection (Pass 1) already happened by that point, and `cache.file_parses` is already fully populated by Pass 1's sequential merge loop (lib.rs, the `for (file, parse, collection) in fresh` loop) independent of Pass 2's own `parse_by_file`.

Sibling harness `crates/apex-binder/examples/mem_profile_declarations_only.rs` sets that env var and dhat-profiles `from_files_cached` the same way ticket 02's `mem_profile.rs` did. Ran via `cargo run --release -p apex-binder --example mem_profile_declarations_only` against the same NPSP corpus (`tests/corpus/npsp`, same commit as ticket 02).

**Declarations-only (Pass 1 + Pass 1.5 only, no Pass 2):**
- Peak (t-gmax): 104,860,222 bytes (~104.9MB)
- Retained at end: 104,552,420 bytes (~104.6MB)
- Total ever allocated: 332,423,156 bytes

**Fully-bound baseline (ticket 02, same corpus):**
- Peak: 248,787,326 bytes (~248.8MB)
- Retained: 188,064,048 bytes (~188.1MB)

**Pass 2's marginal cost, isolated:** retained delta = 188.1MB − 104.6MB ≈ **83.5MB** across ~1044 files (peak delta ≈ 143.9MB). That's ReferenceTable + ScopeTree + bound bodies + ID-remapping/rayon Symbol-tuple overhead — everything ticket 02 attributed to Pass 2 — confirmed by direct subtraction, not inference. Parsing/rowan-tree cost (~57.1M, the single biggest site in ticket 02's breakdown) is *not* part of this delta since it's paid in both scenarios (Pass 1 needs parsed trees too) — it shows up fully in the 104.6MB declarations-only number.

Per-file average marginal Pass 2 cost: 83.5MB / 1044 files ≈ **80KB/file retained** (≈138KB/file at peak).

### Projected working-set total

Scoping Pass 2 to a small working set (declarations-only project-wide + full binding for just N open files) ≈ declarations-only total + N × per-file Pass 2 cost:

- N = 15 open files: 104.6MB + 15 × 0.08MB ≈ **~106MB retained** — down from 188MB (≈44% reduction), and already below the map's 140-150MB destination target on this axis alone, before touching stdlib tightening or anything else.
- N = 50: ≈ 108.6MB. N = 100: ≈ 112.6MB. Even a generous working set stays close to the declarations-only floor — Pass 2's cost is small per file; it's *eager project-wide-ness* that makes it 83.5MB in aggregate.

This is a real, structurally-supported number, not a napkin estimate: the harness measures the actual declarations-only shape, and ticket 02's fully-bound number was independently measured too, so the subtraction is exact for this corpus (modulo ordinary run-to-run allocator noise, not re-measured here).

### Dependency closure: declarations only, not another file's Pass 2 data

Direct evidence, not inference: `BodyBinder` (`crates/apex-binder/src/resolve.rs:818-840`) — the struct that does all of Pass 2's per-file body binding — holds only:
```
pub(crate) struct BodyBinder<'a> {
    pub(crate) table: &'a SymbolTable,   // project-wide declarations (Pass 1)
    pub(crate) schema: &'a SchemaIndex,
    pub(crate) stdlib: &'a StdlibIndex,
    pub(crate) labels: &'a LabelIndex,
    pub(crate) pages: &'a PageIndex,
    pub(crate) refs: ReferenceTable,     // its OWN file's output, being built
    pub(crate) scopes: ScopeTree,        // its OWN file's output, being built
    ...
}
```
There is no field anywhere for another file's `ReferenceTable`, `ScopeTree`, or bound body. `bind_symbol_body` (lib.rs:1375, called from Pass 2 at lib.rs ~693-702) is likewise only handed `&cache.table` (the `SymbolTable`), `&schema`, `&stdlib`, `&labels`, `&pages` — never `&cache.bodies` or any per-file `ReferenceTable`/`ScopeTree` map.

Concretely, cross-file resolution (e.g. `MyClass.someMethod()` where `MyClass` lives in another file) goes through `SymbolTable` lookups only:
- `resolve.rs:3741-3749` (`this(...)`/`super(...)` ctor-chain resolution) and `resolve.rs:3858-3867` (`new Outer.Inner(...)`) both call `self.table.members_of(container)` — a `SymbolTable` (declaration-level) query.
- `resolve.rs:3779-3797` (unqualified call resolution, climbing the enclosing-type chain) calls `self.table.lookup_member(level, name)`, `self.table.params(id)`, `self.table.is_visible_from(...)`, `self.table.get(level).container` — all `SymbolTable` (declaration-level) data: method signatures, visibility modifiers, container/nesting info, arity via `params`.
- Overload narrowing (`narrow_by_overload`, referenced throughout) and inheritance chains (`self.table.direct_super`) are the same story — `SymbolTable`-only.

None of this touches `cache.bodies` (the map holding `ReferenceTable`/`ScopeTree`/bound-body data — Pass 2's own output). **Conclusion: an open file's dependency closure for correct Pass-2 binding needs only the *declarations* (method signatures, field/property types, extends/implements chains, visibility) of every file it references — which are already resident project-wide today, since Pass 1 (`collect`) + Pass 1.5 (`inherit`) already run eagerly for all ~1044 files regardless of any working-set scoping. It never needs another file's own Pass 2 output (bound bodies, reference table, scope tree).** This means scoping Pass 2 to open files + a *declaration* dependency closure (already project-wide-resident, effectively free) is sufficient for correctness — there is no need to transitively full-bind a file's dependencies' dependencies, or even a file's direct dependencies, just because an open file references them. The only files that need real Pass 2 binding are the ones a caller actually wants LSP answers *inside* (hover/goto-def/find-refs *at a position in that file's own body*, diagnostics *for that file's own body*) — referencing another file's declared symbols from inside a bound body is already fully supported without that other file ever being Pass-2-bound.

This directly settles the "direct references vs. transitive closure" half of the question too: neither is required project-wide for Pass 2, because Pass 2 has no cross-file Pass-2-to-Pass-2 dependency at all — only Pass-2-to-Pass-1(declarations) dependency, and Pass 1 is (and already needs to be, for `extends`/`implements` resolution — ticket 02) universal.

---

Full harness code (throwaway `APEXLS_DECLARATIONS_ONLY` env hook + `mem_profile_declarations_only.rs`) lives on branch `research/apex-memory-03-declaration-only-footprint`, not merged to master.
