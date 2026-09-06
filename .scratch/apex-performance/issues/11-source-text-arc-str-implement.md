Type: implement
Status: resolved (2026-09-05) -- picked up as a prerequisite for ticket 07's
rowan-tree eviction design, which needs a cheap way for `BoundProgram` to
re-derive an evicted file's source text without retaining its full green
tree.

## Question

Ticket 03's own writeup identified a small, already-fully-specified fix:
salsa's `FileTextInput.text` is currently a `String`
(`crates/apex-binder/src/db.rs`), and `apex_parser::Parse::text` (added by
this effort's own ticket 01) is built via `Arc::from(src)` where `src`
borrows from `FileTextInput`'s own `String` -- this copies the source bytes
a second time rather than sharing the same allocation.

Change `FileTextInput.text`'s storage to `Arc<str>` (with whatever
`#[returns(...)]` accessor attribute salsa requires, per the diagnostics
map's ticket 24's own already-discovered mechanical details) so
`Parse::text` can become a cheap `Arc::clone` of the same allocation instead
of a fresh copy.

Verify via a targeted before/after dhat run (`mem_profile.rs`) that the
second copy is gone, and via `cargo bench -p apex-binder` that
`corpus/bind_npsp_full` and `corpus/warm_rebind_after_one_file_edit` show no
statistically significant regression. This is a small, low-risk,
already-decided change -- ship it directly, no separate decision ticket
needed.

## Answer

Shipped as designed: `crates/apex-binder/src/db.rs`'s `FileTextInput.text`
changed from `String` to `Arc<str>`; two new `apex_parser` entry points
(`parse_compilation_unit_with_cache_and_text`/
`parse_trigger_unit_with_cache_and_text`, taking `Arc<str>` directly) let
`db::parse_query` hand that same allocation to `Parse::text` via a cheap
`Arc::clone` instead of the old `Arc::from(src)` byte copy; every existing
`&str`-taking entry point (`parse_compilation_unit_with_cache` etc.) is
unchanged, still doing its own `Arc::from(src)` internally, so no other
caller in the workspace needed to change.

**A real regression was found and fixed during implementation, not assumed
away**: the first version routed `sync_file_text_into_db`'s `text: String`
input through a *late* `Arc::from(String)` conversion at the call site,
which -- for the `overrides`-driven edit path (`warm_rebind_after_one_file_edit`'s
own scenario) -- meant a `content.clone()` (String) *followed by* a second,
separate `Arc::from(String)` conversion: two allocations where the original
code had two as well (a `String` clone plus `parse_query`'s own internal
`Arc::from(str)`), but shaped differently enough to show a real, reproducible
bench delta on first measurement. Fixed by having `CandidateFile.text` build
the `Arc<str>` directly at its own two construction sites (`Arc::from(&str)`
for the override branch, one allocation instead of two), removing the
now-redundant conversion at the `sync_file_text_into_db` call site entirely.

**A methodological pitfall found and worked around**: an early bench
comparison read as a statistically significant *regression* (+3-15%,
inconsistent across repeated runs) purely from system-level drift across a
long session of repeated release rebuilds (confirmed by re-measuring a
frozen, no-code-change baseline against itself minutes apart and finding it
had already drifted 20%+ on its own). Resolved by always pairing a fresh
`--save-baseline` with its comparison run back-to-back, never against a
baseline captured much earlier in the same session.

**Verified** (back-to-back baseline/comparison pairs, per the above):
`cargo test -p apex-binder`/`apexls-server` (including the
`salsa_stage2_dual_run` byte-identical parity test against the full real
NPSP corpus) pass unchanged. `cargo bench -p apex-binder`:
`corpus/warm_rebind_after_one_file_edit` **improved** (-6.2%, -7.1% across
two repeated back-to-back confirmations, both p<0.05); `corpus/bind_npsp_full`
showed no significant change (p=0.11), consistent with the very first,
pre-drift reading (p=0.24). Both clear the map's gate; the warm-edit path is
strictly better, not just non-regressed.

**Memory** (`cargo run -p apex-binder --release --example mem_profile`
against the real NPSP corpus, same method as tickets 03/12): retained
(t-end) footprint dropped from **202,482,776 bytes** (after ticket 12) to
**187,803,980 bytes** -- **-14,678,796 bytes (-14.0MB, ~7.2% of the running
total)**, block count essentially unchanged (-726 blocks, confirming the win
is from eliminated *bytes*, not eliminated allocation *count* -- consistent
with collapsing one whole duplicate copy of the corpus's source text, not
many small allocations). This matches ticket 03's own original attribution
of the duplicate-text-copy cost (~14.7-14.8MB) almost exactly, confirming
that estimate was accurate rather than a dhat-inlining artifact as that
ticket's own caveat worried it might be.

Combined with ticket 12, this map's tickets have now reduced the dhat-measured
steady-state footprint from 204.4MB (ticket 03's original baseline) to
187.8MB -- a real, verified **-16.6MB (~8.1%)** so far, with the two Tier-1
categories (rowan trees, `ReferenceTable`/`FileBodies`) still not yet
touched.
