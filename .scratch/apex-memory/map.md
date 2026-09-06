# Halve apexls-server Memory

## Destination

Cut apexls-server (the LSP server process) steady-state RSS on an NPSP-sized project (~1044 files) from 280-300MB toward ~140-150MB (directional — meaningfully lower is the point, hitting exactly half isn't a hard gate). Wins landed in shared crates should also reduce the apexls CLI binary's footprint. Architecture changes are fair game; acceptable trade-offs include slower cold start/first-index, degrading or dropping a disproportionately-expensive feature, and slightly higher latency on some operations. This map produces a decision/spec only — tickets settle architecture questions; implementation happens after, not as part of resolving a ticket.

## Status

**Destination reached.** All four tickets are resolved; no open tickets remain. Ticket 04's working-set scoping design projects ~106MB retained, comfortably under the 140-150MB target. Remaining fog items (Not yet specified) are optional follow-ups, not blockers. Per this map's Notes, it produces a decision/spec only — implementation is a separate follow-on, not part of this map.

## Notes

- Domain: apexls Rust workspace — `apex-binder`, `apex-discover`, `apex-lexer`, `apex-metadata`, `apex-stdlib`, `apex-syntax`, `apex-parser`, `apex-printer`, `apexls` (CLI), `apexls-server` (LSP).
- Multi-process/sharded architecture (a separate out-of-process indexer, worker sharding) is ruled out of scope — see Out of scope.
- Tickets sequence as one ranked path, biggest-lever-first: later tickets may depend conceptually on earlier ones landing, since scoping binding changes what downstream representation questions even need solving.
- Per wayfinder default, consult the grilling and domain-modeling skills for decision tickets; dispatch research subagents for codebase/profiling facts instead of asking the user for anything checkable in the repo.
- A prior memory-reduction effort (`.scratch/apex-performance/`, branch `worktree-perf-tickets`) landed incremental fixes (BindCache cold-file eviction, ReferenceTable/ExternalKey shrinking, Arc<str> text sharing) that are already merged into master — and the tool still sits at 280-300MB. This map deliberately does not build on that effort's remaining backlog; it starts a fresh, data-driven investigation of the current codebase.

## Decisions so far

- [BindState double-residency: real duplication or shared state?](issues/01-bindstate-duplication-research.md): Investigated fresh — `BoundProgram` and `BindCache` share the same `Arc`-wrapped data (texts, parses, symbol table, schema); a prior fix already removed the one shadow copy that existed. Not a memory lever; no action needed here.
- [Fresh dhat breakdown: where does apexls-server's memory actually go?](issues/02-dhat-breakdown-research.md): Peak heap 249MB / retained 188MB for `BoundProgram::from_files_cached` over the NPSP corpus. Dominant costs are almost all Pass 2 (full body/reference binding) machinery: rowan green trees (~23% of peak), ReferenceTable + rayon Symbol/resolve tuples + ID-remapping + ScopeTree (~35-45% combined). Stdlib reference data is a modest ~15-25MB. Binding is confirmed eager and project-wide for every file, including Pass 2, even though the codebase's own `files_to_rebind` machinery already distinguishes "everything changed" from "just dirty files" — scoping Pass 2 to a working set is a real, structurally-supported lever, not a rewrite from scratch.
- [Declaration-only footprint & dependency-closure measurement](issues/03-declaration-only-footprint-research.md): Measured, not estimated: declarations-only (Pass 1+1.5, no Pass 2) over NPSP retains ~104.6MB vs. 188.1MB fully-bound — Pass 2's isolated marginal cost is ~83.5MB across 1044 files (~80KB/file). Projected working set of 15 open files: **~106MB retained, already under the 140-150MB destination target on this axis alone.** Dependency-closure question is settled too: Pass 2 has zero Pass-2-to-Pass-2 cross-file dependency — cross-file resolution only ever reads the project-wide `SymbolTable` (Pass 1 declarations, already resident for everyone), never another file's `ReferenceTable`/`ScopeTree`/bound body. So scoping Pass 2 to open files needs no transitive full-binding of dependencies at all.
- [Working-set scoping architecture for Pass 2 binding](issues/04-pass2-scoping-decision.md): **Locked design — the map's decisive ticket.** Pass 1 stays eager/project-wide/unchanged; Pass 2 is scoped to open files (pinned) plus an on-demand, synchronously-bound, LRU-capped (~50-100 files) set of non-open files touched by cross-file jumps. `files_to_rebind` gains a third mode (Pass 2 only for dirty+in-working-set files). Find-all-references and rename — the only two features needing project-wide `bodies` — trigger a temporary full-project Pass 2 bind on invocation, evicted back down afterward. The CLI (`check`) is untouched; a working set isn't meaningful for its one-shot full-project bind. No salsa redesign needed (Pass 2 isn't salsa-tracked). Projected outcome: ~106MB retained, comfortably under the 140-150MB target with margin — sufficient on its own to reach the destination.

## Not yet specified

- Stdlib reference-data representation tightening (interning/shrinking `apex_reference.json` / `standard_objects.json` in memory) — real but modest (~15-25MB); not needed to hit the destination per ticket 04's outcome, so stays an optional follow-up, not a blocking ticket.
- Salsa incremental-cache growth over a long-running session with many edits (specifically `collect_query`, which has no LRU bound and doesn't shrink as files are removed/renamed within a session) — the dhat profile only measured a single cold `from_files_cached` call, not steady-state after repeated incremental rebuilds. Optional follow-up, not needed to reach the destination.
- `CandidateFile`/discovery-time allocation (~6% of peak in the profile) — unclear whether this is transient (freed after discovery) or retained. Optional follow-up, not needed to reach the destination.

## Out of scope

- Multi-process/sharded architecture (a separate out-of-process indexer, worker sharding): ruled out to keep this a single-process redesign; revisit only if in-process restructuring can't get close to the ~50% target.
