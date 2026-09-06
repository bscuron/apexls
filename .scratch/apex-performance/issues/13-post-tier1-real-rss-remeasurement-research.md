Type: research
Status: resolved (2026-09-06)

## Question

The map's destination is gated on ticket 05's real-memory measurement
(real `mimalloc`-backed `apexls` binary, real NPSP corpus, OS-level
working-set/private-bytes -- not `dhat`) reading as no-longer-memory-hungry.
Tier 1 has since fully shipped (ticket 07's rowan-tree eviction, tickets
09/11/12's `ReferenceTable`/`FileBodies`/source-text shrink), with a
combined **-48.4MB (~23.7%)** `dhat`-measured drop off the original 204.4MB
logical baseline. Does re-running ticket 05's own measurement against
current `HEAD` show a comparable real-memory improvement, and is the map's
destination now met?

## Method

Identical to ticket 05: a throwaway `crates/apexls/examples/rss_probe.rs`
(added, run, deleted after use, never committed -- confirmed by `git
status` showing a clean `crates/apexls/` afterward), building
`apex_binder::BoundProgram::from_files` against the real NPSP corpus
(`tests/corpus/npsp`, submodule already checked out) with the same
`mimalloc` global allocator `apexls`/`apexls-server` ship with, built via
`cargo build -p apexls --release --example rss_probe` (same
`[profile.release]`: `lto = "thin"`, `codegen-units = 1`). The process
printed its PID and slept 60s; `Get-Process -Id <pid> | select
WorkingSet64,PrivateMemorySize64` sampled it twice, ~20s apart, to confirm
the number had stabilized (not still climbing) before treating it as a
result. Run twice independently (two separate process launches) to check
run-to-run noise, matching the reproducibility concern ticket 11 already
flagged for this method.

## Results

| | Working Set (RSS) | Private bytes |
|---|---:|---:|
| Ticket 05, before ticket 01 (`a80fad5`) | 345.8 MB | 549.3 MB |
| Ticket 05, after ticket 01 | 355.8 MB | 577.7 MB |
| This ticket, current `HEAD`, run 1 (two samples 20s apart) | 359.4 MB (stable) | 558.0 / 557.9 MB (stable) |
| This ticket, current `HEAD`, run 2 | 359.7 MB | 570.6 MB |

Both current-`HEAD` runs land within noise of each other and, critically,
within noise of ticket 05's own "after ticket 01" numbers from before any
of Tier 1's work existed. **Real working-set memory has not measurably
improved**: 359.4-359.7MB now vs. 355.8MB then, i.e. flat to slightly
higher, not lower. Private bytes are directionally slightly better in one
run (558MB) and slightly worse in the other (570.6MB) than the 577.7MB
prior figure -- both inside the ~12-30MB run-to-run noise band this same
method already showed between ticket 05's own two runs.

**This despite Tier 1's `dhat`-measured -48.4MB/~23.7% logical reduction.**
The result reproduces ticket 05's own core finding from before Tier 1 ever
shipped: `dhat` counts logical `alloc`/`dealloc` bytes, but the real
`mimalloc`-backed binary's resident memory is dominated by allocator-level
overhead (per-thread segment/arena reservation, size-class rounding, pages
not eagerly returned to the OS) that doesn't move in lockstep with logical
byte reductions. Freeing 48MB of logical allocations mid-run doesn't
shrink the segments `mimalloc` already reserved to hold them, so it never
shows up in `WorkingSet64`/`PrivateMemorySize64` the way it shows up in
`dhat`'s byte-accounting.

## Conclusion

**The map's destination is not met.** Real, user-facing memory (as
opposed to `dhat`'s logical count) is unchanged by all of Tier 1's shipped
work -- still ~355-360MB working set / ~550-580MB private bytes, the same
range ticket 05 already measured before any of this map's tickets landed.
Tier 1 was real, verified, structurally-motivated work (smaller
`ExternalKey`, `SmallVec` single-ref keys, `Arc<str>` text sharing, rowan
tree eviction) and none of it was wasted -- but none of it was ever going
to move this number, because the number is dominated by `mimalloc`'s own
retained-segment behavior, not by how many logical bytes apexls's own data
structures request.

This reopens the map's own explicitly-out-of-scope lever: `mimalloc`
purge/decommit tuning (`MIMALLOC_PURGE_DELAY`/equivalent `mi_option`
settings), previously excluded by explicit user decision ("I consider this
a hack") as not a structural change. That decision was made before this
ticket's finding existed -- that no amount of further structural,
logical-byte-shrinking work (Tier 2's per-file symbol vectors, schema/
stdlib indices, or a hypothetical rowan/`ReferenceTable` redesign) will
touch real RSS the way the map's destination requires, because the gap
lives at the allocator layer, not the data-structure layer. Whether to
revisit that exclusion, given it may be the only lever left that actually
reaches the map's stated destination, is the user's call, not this
ticket's to make -- flagged here rather than acted on.

Tier 2 (not yet opened) would still reduce `dhat`'s logical count further,
but per this ticket's finding, doing so is very unlikely to move real RSS
either, for the same allocator-layer reason. Opening Tier 2 tickets before
that's resolved risks repeating Tier 1's outcome: real, verified,
zero-real-world-impact work.
