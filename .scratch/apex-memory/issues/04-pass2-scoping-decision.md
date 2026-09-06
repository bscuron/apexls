Type: grilling
Status: open
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
