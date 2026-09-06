Type: grilling
Status: resolved
Blocked by: 03 (resolved)

## Question

Design the architecture for scoping full body/reference binding (Pass 2: symbol resolution, reference tables, scope trees) to a working set of open files only, instead of eager whole-project binding, while keeping Pass 1 declaration-only data resident project-wide (unchanged) for cross-file name resolution.

Ticket 03 already settled two sub-questions, so this ticket does NOT need to re-decide them:
- **No dependency closure needed.** Pass 2 has zero Pass-2-to-Pass-2 cross-file dependency (only Pass-2-to-Pass-1/declarations, which is already project-wide and free). So the working set is simply "currently-open files" — no transitive expansion to referenced files required for correctness.
- **Sufficiency vs. the target is confirmed.** Measured: declarations-only floor is ~104.6MB retained; a 15-file working set projects to ~106MB — already under the map's 140-150MB destination on this axis alone. This is very likely the map's single decisive ticket; stdlib tightening and other fog items are probably unnecessary unless this design's real-world overhead (see below) eats into the margin.

What this ticket must still decide:
- How and when a file transitions between declaration-only and fully-bound (Pass 2) state: on open, on request (lazy first-touch), and evicted back to declaration-only on close/idle — including what happens to in-flight requests during the transition.
- How `BindCache`'s incremental invalidation adapts: rebuilds currently run Pass 2 for "everything" or "just dirty files" (`files_to_rebind`); this needs a third mode — Pass 2 for "dirty files that are also in the working set," with declaration-only rebuild for dirty files outside it.
- What real-world overhead this design adds beyond the clean measured numbers (e.g. bookkeeping for working-set membership, transition churn if a user rapidly switches files, salsa cache shape changes) and whether that overhead meaningfully erodes the ~106MB projection's margin.
- Whether workspace-wide features (find-all-references, workspace symbol search, rename, project-wide diagnostics) that currently assume every file is fully bound need a fallback path (on-demand Pass 2 for the files they touch) or an accepted behavior/latency change — this is the one item still in the map's "Not yet specified" fog, to be graduated once this ticket's design makes it concrete.

## Answer

### Fact-finding narrowed the scope sharply

Investigated which LSP handlers actually need Pass 2 (`bodies`) data versus Pass 1 (`symbols`) only:

- **hover, goto-definition, document-highlight, call-hierarchy, signature-help, completion, inlay-hint**: all route through `resolution_at`/`references_to_in_file` (`apex-binder/src/lib.rs:1124,1139,1217-1225,1296-1298`), which only ever need the **queried file's own** `bodies` entry — a single-file, on-demand bind covers these with no correctness loss.
- **find-all-references** (`lib.rs:1277` → `capabilities::references` → `BoundProgram::references_to`) and **rename** (`lib.rs:1337,1374`, built on the same primitive): `references_to` iterates **`self.bodies.values()` across every file** (`apex-binder/src/lib.rs:1208-1212`) — genuinely project-wide. These are the *only* two features that break under working-set scoping.
- **workspace symbol search** (`capabilities.rs:550-574`) and **document outline**: `symbols`-only (Pass 1), unaffected.
- **diagnostics**: the LSP push path (`publish_diagnostics`, `lib.rs:709-729`) is *already* scoped to open documents only (doc comment at `lib.rs:692-700`: recomputing for all ~1000+ files "would be wasted work no client displays anyway") — a pre-existing architectural fit, not a new problem. There is no whole-project diagnostics command in the LSP surface; that only exists in the CLI.
- **`apexls` CLI (`check` command)**: always does one full whole-project `BoundProgram::from_files` bind and exits (`crates/apexls/src/check.rs:125`, doc comment `check.rs:9-19` — explicitly "no existing hook for multi-root/partial binding"). A working set is not a meaningful concept here; **this ticket does not touch the CLI**.
- **Salsa**: Pass 2 is a manual loop in `from_files_cached`/`BindCache`, not a memoized salsa query (only `parse_query` (`lru = 256`), `schema_index`/`label_index`/`page_index`/`collect_query` are salsa-tracked, all Pass 1/discovery-level) — so this design requires no salsa schema changes, only filtering which files the existing manual Pass 2 loop covers.

### Locked design

1. **Scope of change.** Pass 1 (declaration collection) stays exactly as-is — eager, project-wide, unchanged, since it's already free and load-bearing for cross-file `extends`/`implements` resolution (ticket 02/03). Only Pass 2 (body/reference binding) becomes working-set-scoped. The CLI is untouched.

2. **Working-set membership.** Open files (`didOpen`/`didClose`, already wired via `Documents` in `apexls-server/src/lib.rs`) are always pinned in the working set. Non-open files are promoted on-demand: any request needing body-level data for a file that isn't yet Pass-2-bound (e.g. goto-definition landing in an unopened file, peek-definition) triggers a **synchronous inline bind of just that one file** before answering — cheap at ~80KB/file (ticket 03). A generous LRU cap (~50-100 files) bounds these on-demand-bound non-open files, evicting least-recently-used first, modeled on the existing `PARSE_EVICTION_WINDOW`/`parse_last_used` precedent (`incremental.rs:245,259-277`) — no new idle-timer mechanism needed (confirmed none exists to piggyback on; none is needed either, since the cap is recency-based, not time-based).

3. **Rebuild/invalidation.** `files_to_rebind` (`lib.rs:639-643`) gains a third mode alongside "everything" and "just dirty files": Pass 2 runs for dirty files that are also in the working set; dirty files outside it only get Pass 1 (declaration) rebuilds.

4. **Find-all-references and rename.** Both trigger a temporary full-project Pass 2 bind on invocation (spiking memory back toward the ~188MB fully-bound baseline for that operation only), then evict back down to the working-set-only state afterward. Correctness is fully preserved — a missed reference during rename would silently corrupt code, which is worse than a one-time latency/memory spike, and since both features share the same `references_to` primitive, treating them identically avoids gratuitous complexity. This matches the map's pre-agreed trade-offs (slower on some operations in exchange for a much lower steady-state floor).

5. **Concurrency/correctness.** No new race conditions: the existing `BindState` snapshot model (`RwLock<Option<BoundProgram>>` handed to readers as `Arc`-shared data) already means an in-flight request holds an immutable snapshot regardless of what the cache evicts next. Synchronous on-demand binds complete before a snapshot is handed to a reader, not concurrently with one in use — no transition-window race to design around.

6. **Overhead assessment.** The only new bookkeeping is LRU-recency tracking for the working set (same shape as existing `parse_last_used`), sized into the ~106MB projection's margin (a 50-100 file cap adds roughly 4-8MB at ~80KB/file — the projection already assumed a 15-file working set, so this is headroom, not an overrun). No salsa cache reshaping is needed since Pass 2 was never salsa-tracked.

### Outcome

This design is sufficient on its own to hit the map's destination: ~106MB projected retained for a realistic working set, versus 188MB fully-bound / 280-300MB real-world apexls-server RSS today — comfortably under the 140-150MB target with margin. Remaining fog items (stdlib representation tightening, Pass 1 salsa cache growth over long sessions, discovery-time allocation) are not needed to reach the destination and stay deferred as optional follow-ups, not blocking tickets.
