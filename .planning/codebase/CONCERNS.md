# Codebase Concerns

**Analysis Date:** 2026-09-01

## Tech Debt

**Missing semantic token classification:**
- Issue: `textDocument/semanticTokens` LSP capability not yet implemented
- Files: `crates/apexls-server/src/capabilities.rs`
- Impact: Editors can't highlight syntax semantically (distinguished field access from local variables, resolved vs. unresolved type names)
- Fix approach: Requires token-classification pass over resolved AST, categorizing each reference by its `Resolution` kind

**Formatter not implemented:**
- Issue: `apex-printer` only performs verbatim round-trip rendering, no actual reformatting
- Files: `crates/apex-printer/src/lib.rs`
- Impact: `textDocument/formatting` and `textDocument/rangeFormatting` cannot be implemented
- Fix approach: Build a separate formatting/layout component (substantial work, separate from parsing/binding)

**Incomplete stdlib/schema type-model:**
- Issue: Multiple system types still unmodeled or incompletely modeled in `crate::conversions`
- Files: `crates/apex-binder/src/conversions.rs`, `crates/apex-stdlib/src/lib.rs`
- Impact: Complex overload calls stay `Candidates` when they could be narrowed; some SOQL/system features can't be resolved
- Fix approach: Extend curated type-compatibility set, verify each rule against real org via `sf apex run` before trusting (established in this project)

## Known Bugs (Resolved)

**Range precision bug in name extraction (FIXED 2026-08-XX):**
- Symptoms: Renaming a local variable to a short name corrupted the file (e.g., `Integer x= 0;` instead of `Integer x = 0;`)
- Root cause: Trivia (whitespace) was included as a child inside AST nodes (`DeclName`/`NameExpr`/`Type`/`QualifiedName`), making `Symbol::name_range()` one trivia token too wide
- Files affected: `crates/apex-syntax/src/ast/mod.rs`, `crates/apex-binder/src/collect.rs`, `crates/apex-binder/src/resolve.rs`
- Fix: Two-part: (1) `Name::ident_range()` returns identifier token's own range, (2) `BoundProgram::highlight_range()` computes range on demand per query instead of pre-storing
- Regression test: `crates/apex-binder/tests/name_range_precision.rs`, `crates/apexls-server/tests/rename.rs`

**Missing-semicolon diagnostics positioned incorrectly (FIXED 2026-08-XX):**
- Symptoms: A missing semicolon at end-of-line was reported on the *next* statement instead of the location of the missing token
- Root cause: `Parser::expect()` recorded failure at the start of the following real token, not at the gap where the token was missing
- Files: `crates/apex-parser/src/parser.rs`
- Fix: Added `error_at_gap()` that positions errors at byte after last consumed token (skipping trivia), matching rustc/rust-analyzer convention
- Regression test: `crates/apexls-server/tests/syntax_error_diagnostics.rs`

## Security Considerations

**Lock poisoning risk (MITIGATED 2026-08-XX):**
- Risk: Any panic while holding `std::sync::RwLock` guard permanently poisons the lock, silently degrading all future requests to errors until server restart
- Files: `crates/apexls-server/src/lib.rs` (BindState initialization)
- Current mitigation: Switched all four `BindState` locks from `std::sync::RwLock` to `parking_lot::RwLock`, which never poison
- Verification: Unit test deliberately panics while holding write guard, confirms lock remains usable immediately after (would fail with std::sync)

**No unsafe code in production:**
- Verified: Codebase contains zero `unsafe` blocks in production code (only in comments referring to the concept)
- All panics are in test helpers or precondition validation, not in request handlers

## Performance Bottlenecks

**Warm single-edit rebind latency depends on project size:**
- Problem: Even with incremental rebind, warm-edit latency is dominated by file-discovery and metadata caching, both scale with total project size (~1044 files in NPSP corpus)
- Files: `crates/apex-binder/src/lib.rs`, `crates/apex-metadata/src/discover.rs`
- Cause: `stage_1a_read_and_parse` calls `std::fs::metadata()` stat syscall for every candidate file on every call (even unchanged ones), accounting for 55% of warm-rebind wall time (~3.5ms of 6.4ms on NPSP)
- Improvement path: Filesystem-watcher integration (`workspace/didChangeWatchedFiles`) to skip stat syscalls for known-unchanged files; separate architectural work, already tracked as gap in BACKLOG.md

**Discovery/schema walk was redundantly called twice (FIXED 2026-08-XX):**
- Symptoms: Cold bind was calling both `apex_discover::discover` and `apex_metadata::discover_sobjects`, each re-walking the directory tree independently
- Root cause: Caller and callee both walked independently instead of reusing the same walk
- Files: `crates/apex-binder/src/lib.rs`, `crates/apex-metadata/src/discover.rs`
- Fix: Added `apex_metadata::sobjects_from_discovery` and `SchemaIndex::from_discovery` to share one walk across both callers
- Result: Warm-edit latency dropped from ~78ms to ~17ms (40x speedup from cold baseline)

**SFDX metadata XML parsing was sequential (FIXED 2026-08-XX):**
- Problem: `SchemaIndex::from_discovery` XML parsing took 78ms (21% of cold bind) without parallelization
- Files: `crates/apex-metadata/src/discover.rs`
- Fix: Parallelized field metadata parsing with `rayon` parallel iterator + sequential merge
- Result: Dropped to consistent ~21ms (-73-76% reduction)

**Pass 2 merge-bodies stage allocated 20% of total project memory (FIXED 2026-08-XX):**
- Problem: `pass2_merge_bodies` sequential step allocated 110.0 MB exclusive (out of 544.5 MB total) via intermediate `ReferenceTable` allocations and repeated `extend`/`insert` calls
- Files: `crates/apex-binder/src/reference_table.rs`, `crates/apex-binder/src/lib.rs`
- Fix: Two changes: (1) fused `ReferenceTable::map_ids` + `merge_into` into single `map_ids_into`, (2) pre-sized `extra_symbols`/`file_bodies.scopes` per file
- Result: Cold-bind allocation dropped from 544.5 MB to ~502 MB (-8%), `pass2_merge_bodies` exclusive allocation dropped from 110.0 MB to 75.6 MB (-31%)

**Rowan green-tree node interning was per-file (FIXED 2026-08-XX):**
- Problem: Each file's parse built its own fresh `rowan::NodeCache`, missing cross-file deduplication of common tokens/keywords
- Files: `crates/apex-parser/src/event.rs`, `crates/apex-binder/src/lib.rs`
- Fix: Shared one `NodeCache` per adaptive `rayon` fold segment (via `.with_min_len(64)`) instead of per-file, while keeping warm-rebind fast
- Result: Memory retained dropped from 134.7 MB to 128.1 MB (-4.9% bytes, -12.9% blocks), no warm-rebind regression

## Fragile Areas

**Call hierarchy outgoing calls excludes external references:**
- Files: `crates/apex-binder/src/call_hierarchy.rs` (lines 76-96)
- Why fragile: A call to a real stdlib method (`String.isBlank(...)`) or schema-backed object silently drops from `outgoing_calls` since it matches `Resolution::SymbolId`-only, not `ExternalKey` like `references` now does. Creates false-negative gap where a method appears to make no outgoing calls when it actually calls stdlib extensively
- Safe modification: Requires `CallHierarchyItem` to support external targets (no location to build from), architectural change beyond just adding `ExternalKey` matching like `references` did
- Test coverage: `crates/apex-binder/tests/call_hierarchy.rs` covers project-local calls and ambiguous overloads, but no stdlib/schema call cases

**Completion engine relies on fresh receiver-type inference:**
- Files: `crates/apex-binder/src/completion.rs` (lines 1000+), `crates/apex-binder/src/resolve.rs`
- Why fragile: Member-access completion reuses `resolve::bind_expr` verbatim with a **cloned** `ScopeTree` but **fresh** `ReferenceTable` to avoid mutating the real binding state. Correct by design (read-only scope walk), but mutations to the real binding process (especially in `resolve` or scope tracking) need to be mirrored here
- Safe modification: Test any scope/resolution changes against completion fixtures (`crates/apex-binder/tests/completion.rs`) to catch drift
- Test coverage: `crates/apex-binder/tests/completion.rs` covers nested scopes, inheritance, member-access chains, but no advanced type-inference cases

**Rename deliberately refuses ambiguous cases:**
- Files: `crates/apexls-server/src/capabilities.rs` (lines 877-904)
- Why fragile: Refuses outright (returns `ResponseError`) if target or any reference resolves as `Candidates`/`Unresolved`, is a `Trigger`, is an `override` method, new name collides, etc. This conservative design is correct but means a project with genuinely unambiguous-but-unnarrowed method overloads can never rename at all
- Safe modification: Each refusal case is documented and tested separately; verify narrowing logic improvements (via overload-resolution fixes) work end-to-end
- Test coverage: `crates/apexls-server/tests/rename.rs` covers every refusal case, including ambiguous/override/collision scenarios, but assumes overload narrowing already works as expected

**Catch-clause exception type resolution is project-local-only:**
- Files: `crates/apex-binder/src/resolve.rs`, `crates/apex-binder/src/symbol_table.rs`
- Why fragile: `SymbolTable::resolve_dotted_name` used for catch-clause exception types has no stdlib fallback, so `catch(DmlException e)` (a stdlib type) stays `Unresolved`. Workaround was implemented (`classify_unresolved` with WARNING severity instead of ERROR) but root cause remains
- Safe modification: Extend `resolve_dotted_name` to consult `StdlibIndex` before giving up, but must handle name-collision cases (project-local type shadows same-named stdlib type)
- Test coverage: Existing warnings test documents this limitation but doesn't drive it to closure

## Scaling Limits

**Incremental rebind falls back to full project rebuild on declaration changes:**
- Current capacity: Fast incremental rebind (~17ms) only for body-only edits, full project rebuild (~431ms) on any declaration signature/field/class-shape change
- Limit: Any signature edit cascades to full re-bind, even if the change could only affect a small subset of the project
- Scaling path: Per-reference dependency tracking (track which symbols/scopes each reference depends on) to narrow cascade detection beyond the current "any file changed" rule; significant architectural work, tracked in BACKLOG.md §2 as deliberately deferred in favor of dominant-use-case optimization

**File discovery walk re-runs unconditionally every call:**
- Current capacity: Stat syscall per candidate file per call even if nothing changed
- Limit: Warm-rebind bottleneck for large projects (55% of wall time)
- Scaling path: Filesystem-watcher integration to signal which files actually changed, then skip stat for cached files; prerequisite for further warm-edit latency cuts

## Dependencies at Risk

**No direct dependency vulnerabilities detected, but:**
- `rowan` green-tree interning still has cross-file memory overhead even after per-segment cache sharing (was 134.7 MB before fix, 128.1 MB after)
- Metadata XML parsing via `roxmltree` (deliberately non-validating, per BACKLOG.md) is stable but could miss malformed pages edge cases

## Missing Critical Features

**SOQL/SOSL completion not implemented:**
- Problem: `FROM`/`WHERE` field/object name completion in SOQL/SOSL queries is not available
- Blocks: Users can't autocomplete SOQL object/field references
- Priority: High (SOQL is core Apex feature)

**Override method completion not implemented:**
- Problem: When typing inside a class that extends another or implements an interface, no completion for `@Override` method stubs
- Blocks: Developers must manually type method signatures for overrides
- Priority: Medium

**Auto-import not implemented:**
- Problem: No automatic namespace/class import insertion on completion of an unqualified name
- Blocks: Users must manually add top-level imports for completed references
- Priority: Medium

**Snippet completion not implemented:**
- Problem: No Apex code-snippet templates (e.g., `if`, `for`, `try-catch` blocks)
- Blocks: Lower discoverability of control-flow syntax
- Priority: Low

**File-watching integration not implemented:**
- Problem: Files added to disk or modified via `git pull` / build tools aren't picked up until next editor action
- Blocks: Background changes to project structure aren't visible until a refresh signal
- Priority: Medium (affects real multi-tool workflows)

## Test Coverage Gaps

**No end-to-end cancellation test:**
- What's not tested: `$/cancelRequest` flow with an actually slow operation (infrastructure in place via `async-lsp`'s `ConcurrencyLayer`, but all request handlers complete near-instantly)
- Files: `crates/apexls-server/src/lib.rs`, `crates/apexls-server/tests/handshake.rs`
- Risk: Cancellation infrastructure untested against real workload; breakage goes unnoticed until a slow operation lands
- Priority: Low (infrastructure is solid, just needs a real consumer)

**Limited generics/collection-type narrowing coverage:**
- What's not tested: Complex nested generics like `List<Map<String, List<Integer>>>` with overload-narrowing, cross-type collection compatibility chains
- Files: `crates/apex-binder/tests/overload_narrowing_conversions.rs`
- Risk: Edge cases in collection argument type matching could silently narrow wrong
- Priority: Medium (affects real Salesforce code patterns)

**Namespace-qualified type references still have false-positive unresolved:**
- What's not tested: Every segment of a `Schema.SObjectType.ID` reference now consults stdlib, but "per-segment unresolved" diagnostic is still emitted for intermediate segments even when the whole reference resolves (FIXED by consulting stdlib at segment level, but test coverage for the fix is minimal)
- Files: `crates/apex-binder/src/resolve.rs`, `crates/apexls-server/tests/unresolved_reference_diagnostics.rs`
- Risk: Users see "unresolved" warnings on code that actually compiles fine
- Priority: Medium (UX noise, not a correctness bug)

---

*Concerns audit: 2026-09-01*
