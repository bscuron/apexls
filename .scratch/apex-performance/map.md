Label: wayfinder:map

## Destination

A decided-and-shipped structural redesign of `apex-binder`'s retained data --
rowan syntax trees and Pass-2's `ReferenceTable`/`FileBodies` body-merge
structures, the two categories ticket 03's dhat breakdown already shows
dominate at ~61% combined of the 204MB logical footprint -- cutting
apexls-server's real, mimalloc-backed resident memory on the real NPSP
corpus (currently ~350-580MB per ticket 05) enough that the tool stops being
memory-hungry. No fixed percentage target: each ticket estimates and reports
its own concrete savings, measured the way ticket 05 did (the real binary
against the real corpus, not dhat alone), and the map is done when
re-running ticket 05's own measurement would no longer read as
memory-hungry. `cargo bench -p apex-binder`'s existing corpus benchmarks
(`bind_npsp_full`, `warm_rebind_after_one_file_edit`), re-baselined against a
clean build, must show no statistically significant regression -- a hard
floor, not a tradeable one.

## Notes

- "Structural change" = touches `apex-lexer`/`apex-parser`/`apex-binder`'s
  own data representation, ownership, or algorithm (node/token layout,
  `ReferenceTable` entry shape, interning, arena allocation, eliminating a
  redundant retained copy, retention *policy* like on-demand/LRU vs.
  retain-forever). Tuning an existing subsystem's operating parameter with
  no change to apexls's own data model (mimalloc env vars, thread-pool
  size) is excluded regardless of impact -- ticket 05's own mimalloc-tuning
  lever stays out, by explicit user decision ("a hack").
- Measure real memory ticket 05's way (release, mimalloc-linked binary, real
  NPSP corpus, OS-level working-set/private-bytes) for any before/after
  savings claim. dhat stays useful for attributing *where* memory goes
  (ticket 03's own breakdown), never as the final number a savings claim
  rests on.
- Tier 1 (own decision ticket first): rowan syntax trees (~36.6%/74.8MB) and
  Pass-2 reference/body-merge structures -- **revised by ticket 10 to
  ~33.7%/68.9M** (not the originally-reported ~24.6%/50.4M; ticket 03's own
  category total undercounted three large sites that are unambiguously
  `ReferenceTable`/`FileBodies` fields), making the two Tier-1 categories
  nearly equal in size, not one clearly dominant over the other. Tier 2
  (smaller, independently-bounded): per-file symbol vectors (~5.8%), raw
  source text (~7.2%, already has one known unaddressed micro-opt from
  ticket 03: salsa's `FileTextInput.text` as `Arc<str>` instead of `String`),
  schema/stdlib indices (~3.5%). Ticket 10 also found the genuine unattributed
  tail is only ~6.3%/12.9M (not ~21.9%/44.8M as ticket 03 originally reported)
  and confirmed it's ordinary diffuse binding bookkeeping, not a hidden
  category -- closed, no follow-on ticket needed.
- Salsa-migration-stage territory (owned by `apex-diagnostics/map.md`'s own
  not-yet-specified Stage 4) isn't walled off from this map -- if a memory
  design turns out to need or overlap it, it stays in-bounds here; note the
  overlap explicitly (matching `research.md`'s own precedent for
  opportunities 2/5) rather than deferring wholesale.
- Every ticket's own definition of done includes a `cargo bench -p
  apex-binder` re-baseline, per this project's established practice
  (diagnostics map tickets 26/29/31 etc.) -- not a separate cross-cutting
  ticket.
- Consult `apex-grammar-oracle-sf-cli` practice (the `sf` CLI against a
  connected org) if any ticket's design ever turns on real-compiler-behavior
  questions, same as this project's established norm -- unlikely to come up
  for pure memory-layout work, but the norm still applies if it does.

## Decisions so far

- [Research rowan tree retention](issues/06-rowan-tree-retention-research.md):
  Pass 2/`resolve.rs` never needs another file's syntax tree (confirmed by
  direct read, zero cross-file `.syntax()` calls), but several capability
  handlers (hover's doc-comment lookup, parameter-reorder, rename/
  find-references) legitimately fan out across every file referencing a
  symbol. The real blocker to reclaiming any of the 74.8MB isn't
  `parse_query`'s own salsa memoization (which already supports a native
  `lru = N` eviction option in the pinned salsa 0.28.2) but
  `BindCache::file_parses` (`incremental.rs:171`), a second, hand-rolled,
  never-evicted `HashMap<FileId, Parse>` that diagnostics-map ticket 29
  deliberately kept for warm-rebind speed -- so `parse_query`'s `lru` option
  alone reclaims nothing until that separate permanent retention is also
  addressed. An on-demand/LRU model is architecturally sound, but the actual
  reclaimable % depends on how the follow-on decision resolves this
  `file_parses`-vs-eviction tension -- not yet quantified.
- [Research the unattributed 22% dhat slice](issues/10-unattributed-allocations-research.md):
  the reported 44.8MB/21.9% "everything else" figure significantly
  overstated the genuine unattributed tail -- re-grouping all 634 distinct
  call-stack frames (not just ticket 03's top 30) found the real below-top-30
  long tail is only ~12.9MB/6.3%, ordinary per-file binding bookkeeping, no
  hidden category. The other ~18.5MB/9.1% was actually three large top-30
  sites ticket 03 never assigned to a named category, confirmed by reading
  `reference_table.rs`/`lib.rs` directly to be `ReferenceTable`/`FileBodies`
  fields (`by_symbol`/`by_external`'s per-key `Vec<SyntaxPtr>` growth,
  `highlight_ranges`, `FileBodies.scopes` init). **`ReferenceTable`/
  `FileBodies`'s real size is ~68.9M/33.7% of the footprint, not the
  previously-reported 50.4M/24.6%** -- nearly as large as rowan trees
  (~36.6%), not clearly second-tier to them. No new ticket opened for this
  slice; [ticket 08](issues/08-reference-table-footprint-research.md) should
  use the corrected ~68.9M/33.7% figure as its real target.
- [Research ReferenceTable/FileBodies's per-entry footprint](issues/08-reference-table-footprint-research.md):
  confirmed exact type sizes via a temporary `mem_profile.rs` instrumentation
  (added, run, reverted -- working tree left clean) against the real
  412,894-entry corpus-wide `ReferenceTable`. `resolutions`
  (`FxHashMap<SyntaxPtr, Resolution>`, 16.5MB raw KV before hashbrown
  overhead) is already near-optimal (fast hasher, boxed large variants,
  compact 16B key) -- not this map's win. Two real, concrete opportunities
  instead: (1) `ExternalKey` is a flat 64 bytes because of its own `Stdlib`
  variant, even though `Schema`/`Label`/`VisualforcePage` keys only need
  24-48 bytes -- the same oversized-enum-variant problem `Resolution` already
  solved via boxing, never applied here; mechanical, low-risk, ready to
  ticket directly. (2) `by_symbol`/`by_external`'s per-key owned
  `Vec<SyntaxPtr>` (independently confirmed by ticket 10 at 9.45MB/4.62% of
  the whole footprint) pays one heap allocation per distinct key rather than
  a shared buffer -- a shared arena + `(start,len)` index would collapse
  this, confirmed compatible with every `references_to`/
  `references_to_external` call site (rename, find-references,
  document-highlight, hover).
- [Decide ReferenceTable/FileBodies's shrink architecture](issues/09-reference-table-footprint-decision.md):
  locked both of ticket 08's findings, but replaced its "arena + index"
  framing for the `Vec<SyntaxPtr>` finding with a simpler `SmallVec<[SyntaxPtr; 1]>`
  swap (already-resolved workspace dependency, zero call-site changes,
  drop-in for the existing `&[SyntaxPtr]` return type) -- rejected the arena
  approach as real, correctness-sensitive complexity for a proportionally
  small win. See [ticket 12](issues/12-reference-table-shrink-implement.md)
  for the shipped result.
- [Ship the ReferenceTable/ExternalKey shrink](issues/12-reference-table-shrink-implement.md):
  both changes landed in `reference_table.rs` -- zero call-site churn
  anywhere else in the workspace (confirmed by grep before implementing).
  `cargo test -p apex-binder`/`apexls-server` pass unchanged.
  `cargo bench -p apex-binder`: `bind_npsp_full` **improved -5.25%**
  (statistically significant), `warm_rebind_after_one_file_edit` unchanged
  -- both clear the map's gate, cold-bind is strictly better. Real memory
  (`mem_profile` dhat run, same method as ticket 03): retained footprint
  dropped from 204,422,408 to 202,482,776 bytes (**-1.9MB, ~0.9%**),
  -29,695 fewer allocated blocks. Smaller than ticket 08's raw 9.45MB
  estimate -- honestly attributed to `SmallVec`'s in-map slot not actually
  being smaller than `Vec`'s, only avoiding a separate heap allocation for
  single-reference keys; capturing more would need the arena approach this
  decision deliberately rejected. First concrete, verified memory win on
  this map.
- [Ship the FileTextInput.text `Arc<str>` fix](issues/11-source-text-arc-str-implement.md):
  picked up as a prerequisite for ticket 07 (below), which needs a cheap way
  for `BoundProgram` to re-derive an evicted file's text without retaining
  its full green tree. Shipped as designed; a real double-allocation bug was
  found and fixed during implementation (the first version still allocated
  twice on the `overrides`-driven warm-edit path), and a real methodological
  pitfall was found and worked around (system-level drift across a long
  session of repeated rebuilds read as a false "regression" until baseline/
  comparison pairs were run strictly back-to-back). Verified: tests pass,
  `warm_rebind_after_one_file_edit` **improved** (-6-7%, reproducible),
  `bind_npsp_full` unchanged. Real memory: retained footprint dropped
  202.5MB -> 187.8MB (**-14.68MB, ~7.2%**), matching ticket 03's own
  original duplicate-text-copy estimate almost exactly. Combined with ticket
  12: **-16.6MB (~8.1%) verified so far**, Tier 1 (rowan trees,
  `ReferenceTable`/`FileBodies`) still untouched.
- [Decide and ship rowan tree retention](issues/07-rowan-tree-retention-decision.md):
  locked and shipped a coordinated two-layer eviction -- `db::parse_query`
  gets `lru = 256` (a safety net; needed since salsa's own memo table would
  otherwise keep every tree alive regardless of `BindCache`'s own eviction)
  plus a new generation-counter-based eviction on `BindCache::file_parses`
  itself (`PARSE_EVICTION_WINDOW = 8` rebinds), with `parse_by_file`'s
  construction and a new `BoundProgram::texts` field (every file's source
  text, always retained via ticket 11's cheap `Arc<str>` share) making both
  Pass 2 and capability-handler reads miss-tolerant instead of panicking.
  **Empirically verified the one untested scenario before shipping** (two
  throwaway probes, deleted after use): a declarations-changed edit after
  heavy eviction costs ~405ms vs. ~239ms with nothing evicted (+~70%,
  confirming diagnostics-map ticket 29's precedent risk was real) --
  balanced against **-31.8MB (~17%) further steady-state memory** in the
  same scenario (**-48.4MB/~23.7% combined with tickets 09/11** off the
  original 204.4MB baseline, the map's largest win). Put this exact
  tradeoff to the user directly rather than deciding unilaterally --
  **shipped as-is**: the rebuild is background/non-blocking, the cost only
  hits the less-common declarations-changed edit class, and both required
  gate benchmarks are unaffected. `cargo test -p apex-binder`/`apexls-server`
  pass unchanged, including `rapid_edit_burst`'s own 600-edit stress test.

## Not yet specified

- **Tier 2 categories** (per-file declared-symbol vectors ~5.8%, schema/stdlib
  indices ~3.5%) -- real per ticket 03's breakdown, but not yet phrased as a
  sharp question. May turn out not worth their own tickets once Tier 1 work
  lands (a rowan/`ReferenceTable` redesign could shrink these incidentally),
  or may graduate into small tickets of their own later.

## Out of scope

- **mimalloc purge/decommit tuning** (config-only allocator behavior) -- real,
  cheap, already-scoped by ticket 05, but explicitly excluded from this map's
  destination by the user ("I consider this a hack"); a candidate for its own
  standalone ticket outside this map if ever wanted.
- **mmap-backed source text** -- already closed by ticket 05's own research
  as targeting the wrong layer (source text is a small slice; the dominant
  categories are derived structures, not raw bytes).
- **BindCache/BoundProgram double-storage de-duplication** -- already closed
  as not-applicable by ticket 04 (no meaningful duplicated-but-shareable data
  found; ticket 03's own dhat drill-down confirmed the overlap is 0.3% of the
  footprint, already `Arc`-shared).
