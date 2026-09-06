Type: decision
Status: resolved (2026-09-05)
Blocked by: 06

## Question

Given [ticket 06](06-rowan-tree-retention-research.md)'s findings on whether
Pass 2/`resolve.rs` and `apexls-server`'s capability handlers need every
file's rowan syntax tree retained in steady state, decide the concrete
architecture for reducing rowan syntax-tree memory: whether to (a) move to
an on-demand/LRU-evicted `GreenNode` retention model, (b) instead or also
shrink rowan's own per-node representation (e.g. more compact tokens,
interned strings), (c) some combination of both, or (d) conclude no safe or
worthwhile change exists here. Lock the exact data-structure changes,
eviction policy (if any), and re-parse trigger points needed, sized for a
follow-on implement ticket. Must account for `cargo bench -p apex-binder`'s
existing `corpus/bind_npsp_full` and `corpus/warm_rebind_after_one_file_edit`
gates (no statistically significant regression) as a hard constraint on any
chosen design.

## Answer

**Locked: (a) an on-demand/LRU-evicted `GreenNode` retention model**, resolving
ticket 06's own "real tension" (needs both `BindCache::file_parses` and
salsa's `parse_query` bounded together, since either alone leaves a live
`Arc`/`Rc` handle keeping the tree resident) as a coordinated, two-layer
eviction:

1. **`db::parse_query` gets `lru = 256`** -- a correctness safety net
   bounding salsa's own worst-case retention over an arbitrarily long
   session, generously sized so it essentially never fires before
   `BindCache`'s own, more precisely-tuned eviction already would have.
2. **`BindCache::file_parses` gets its own generation-counter-based
   eviction** (`parse_generation`/`parse_last_used`, `PARSE_EVICTION_WINDOW
   = 8` rebinds): any file not touched by Pass 2 in the last 8 rebinds is
   dropped. Chosen over ticket 06's own "some hybrid keeping only currently
   open buffers resident" option (needs new cross-crate plumbing apex-binder
   has no notion of "open buffer" for) -- pure recency-by-touch already
   approximates it, since editing is what keeps a file's Pass 2 recency
   fresh.
3. **`parse_by_file`'s construction (`lib.rs`) is now miss-tolerant**: built
   only for `files_to_rebind` (not blanket every current file), a cache miss
   re-fetches via `db::parse_query` and re-populates `file_parses` so a file
   touched again soon doesn't keep missing.
4. **`BoundProgram` gains a `texts: FxHashMap<FileId, Arc<str>>` field**
   (every current file's source text, always retained regardless of
   eviction -- cheap thanks to ticket 11's `Arc<str>` sharing) so
   `BoundProgram::syntax` can re-parse an evicted file on demand for
   capability-handler reads (hover's cross-file doc-comment lookup,
   rename/reorder-params' fan-out, per ticket 06's own finding #2) without
   ever panicking on a miss. Deliberately **not** cached back into the
   snapshot on a repeated read within one snapshot's lifetime -- `BoundProgram`
   is an immutable, `Arc`-shared snapshot read concurrently by many request
   handlers, and caching would need its own interior-mutability story
   (a `Mutex`-guarded per-snapshot cache). Accepted v1 simplification, not a
   correctness gap -- a repeated hover on the same rarely-edited-but-often-
   viewed file re-parses every time rather than once; documented as the
   known upgrade path if this ever measures as a real cost.

**Empirically verified before locking**, per this ticket's own mandate (not
assumed): a throwaway probe (`examples/eviction_burst_probe.rs`, deleted
after use, matching ticket 05's own `rss_probe.rs` precedent) measured the
one scenario ticket 06 flagged as untested by either required benchmark --
a declarations-changed rebind (needs *every* file's `Parse`) after 20
preceding non-declaration-changing edits to a *different* file have evicted
most of the corpus. Result: **~405ms**, vs. **~239ms** for the same edit with
nothing evicted (+~70%, roughly a full cold-bind's worth of cost) --
confirming ticket 06's own worry was real, not hypothetical, and structurally
the same "sweep everyone through `parse_query`" pattern the diagnostics map's
ticket 29 already found expensive once.

A second throwaway probe (`examples/eviction_mem_probe.rs`, also deleted
after use) measured the other half of the tradeoff: steady-state memory
after that same 20-edit scenario dropped from 187.8MB (ticket 11's own
post-fix baseline) to **156.0MB** -- a further **-31.8MB (~17%)**, the
largest single reduction this map has found, combining with tickets 09/11
for **-48.4MB (~23.7%) off the original 204.4MB baseline**.

**Put this exact tradeoff to the user directly** (not decided unilaterally,
given how large both sides are): ship as-is, widen `PARSE_EVICTION_WINDOW`
first to soften the worst case, or defer the whole ticket. **Decision: ship
as-is** -- the rebuild runs in `apexls-server`'s background worker (not a
synchronous LSP request stall), the cost only fires on the less-common
declarations-changed edit class (ordinary body-only typing, the common
case, is unaffected), and both of the map's *required* gate benchmarks
(`bind_npsp_full`, `warm_rebind_after_one_file_edit`) are unaffected by this
scenario either way.

Shipped directly (small enough, and already built+measured as part of
locking this decision) rather than split into a separate implement ticket --
see the commit itself for the full diff (`crates/apex-binder/src/{incremental.rs,db.rs,lib.rs}`).
`cargo test -p apex-binder`/`apexls-server` (including `rapid_edit_burst`'s
own 600-sequential-edit stress test, a real exercise of this exact eviction
path) pass unchanged.

**Two real bugs found and fixed by `/code-review`, not caught by the test
suite**: `BoundProgram::source_text` and `BoundProgram::syntax_errors` both
still indexed/queried `self.parses` directly -- the exact panic-on-miss and
silent-wrong-answer shapes `Self::syntax` was already fixed for, just missed
on the other two accessors. Fixed by factoring a shared `reparse_evicted`
helper (used by all three) and, for `source_text`, reading straight from the
always-retained `texts` field instead (no re-parse needed at all, since it
only needs the text, not a tree). `syntax_errors`' return type changed from
a borrowed `&[ParseError]` to `Cow<'_, [ParseError]>` -- a lazily-reparsed
fallback can't satisfy a borrow from `self`, so *some* signature change was
unavoidable; `Cow::Borrowed` for the common (non-evicted) case avoids a
review-caught follow-up regression (an earlier `Vec<ParseError>`-returning
version cloned on every call, even the hit case). Its one real caller
(`capabilities.rs`'s `syntax_error_diagnostics`) needed no changes -- `.iter()`
works identically on `Cow<[T]>`. Re-verified after the fix: full test suites
green, both required corpus benchmarks re-confirmed clean in one final
back-to-back comparison.
