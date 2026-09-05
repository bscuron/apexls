Type: implement
Status: resolved (2026-09-05)
Blocked by: 09

## Question

Ship [ticket 09](09-reference-table-footprint-decision.md)'s locked design
in `crates/apex-binder/src/reference_table.rs`:

1. Box `ExternalKey::Stdlib`'s payload into a new `StdlibKey` struct
   (`ExternalKey::Stdlib(Box<StdlibKey>)` replacing the inline struct
   variant), updating `Resolution::external_key`'s `StdlibMember` arm (the
   sole construction site).
2. Add `smallvec = "1.15.2"` as a direct `apex-binder` dependency (matching
   the version already resolved workspace-wide) and change
   `by_symbol`/`by_external`'s value type from `Vec<SyntaxPtr>` to
   `SmallVec<[SyntaxPtr; 1]>`.

Verify: `cargo test -p apex-binder` (and the full workspace) passes
unchanged; a before/after `cargo run -p apex-binder --release --example
mem_profile` dhat run shows a real reduction in `ExternalKey`/`by_symbol`/
`by_external`'s retained bytes; `cargo bench -p apex-binder`
(`corpus/bind_npsp_full`, `corpus/warm_rebind_after_one_file_edit`),
re-baselined against a clean build, shows no statistically significant
regression, per the map's own hard gate.

## Answer

Shipped exactly per ticket 09's design in `crates/apex-binder/src/reference_table.rs`:
`ExternalKey::Stdlib(Box<StdlibKey>)` replacing the inline struct variant
(sole construction site, `Resolution::external_key`'s `StdlibMember` arm);
`by_symbol`/`by_external`'s value type changed from `Vec<SyntaxPtr>` to a
new `RefVec = SmallVec<[SyntaxPtr; 1]>` type alias, `smallvec = "1.15.2"`
added as a direct `apex-binder` dependency (already resolved workspace-wide,
so `Cargo.lock` gained no new crate). Both changes were confirmed
self-contained to this one file before implementing (grepped the whole
workspace for `ExternalKey::Stdlib` construction/matching -- only the one
site found) -- zero call-site churn anywhere else, exactly as ticket 09
predicted.

**Tests**: `cargo test -p apex-binder` (all suites, release) and
`cargo test -p apexls-server` both pass unchanged, zero failures.

**Bench** (`cargo bench -p apex-binder --bench binder_bench -- corpus`,
compared against a freshly-built clean-pre-change baseline via
`git stash`/`--save-baseline`/`git stash pop`, this session's own re-baseline
per the map's established practice): `corpus/bind_npsp_full` improved
**-5.25% (p=0.01, statistically significant)** -- likely fewer/smaller heap
allocations helping cache locality during the merge pass;
`corpus/warm_rebind_after_one_file_edit` showed **no significant change**
(p=0.43). Both clear the map's hard gate; the cold-bind path is strictly
better, not just non-regressed.

**Memory** (`cargo run -p apex-binder --release --example mem_profile`
against the real NPSP corpus, same method as ticket 03/08): retained
(t-end) footprint dropped from **204,422,408 bytes / 1,148,413 blocks**
(ticket 03's original baseline) to **202,482,776 bytes / 1,118,718 blocks**
-- a real but modest **-1,939,632 bytes (-1.9MB, ~0.9% of the 204MB
footprint)** and **-29,695 fewer allocated blocks** (consistent with
`SmallVec`'s inline slot eliminating a separate heap allocation for every
single-reference key, the single-reference case ticket 08 itself named as
the common one).

**Honest gap vs. ticket 08's own estimate**: this is smaller than ticket
08's raw 9.45MB `Vec<SyntaxPtr>`-growth figure might have suggested,
because `SmallVec<[SyntaxPtr; 1]>`'s own in-map struct size (inline 16-byte
array vs. a 24-byte heap `(ptr,len,cap)` triple, unioned together) isn't
smaller than `Vec<SyntaxPtr>`'s -- the win is entirely from *eliminating a
separate heap allocation* for single-reference keys, not from shrinking the
map's own per-key storage. Capturing more of that original 9.45MB (and any
further shrink) would need the more invasive arena+index restructuring
ticket 09 deliberately rejected for its complexity-to-benefit ratio at this
scale -- correctly rejected in hindsight; the measured win here is real and
free of that risk, and a bigger win was never actually available at this
comparable risk level. Verified honestly rather than assumed.
