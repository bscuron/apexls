Type: research
Status: resolved (2026-09-05)

## Question

apex-binder's steady-state ~204MB dhat footprint (ticket 03) is ~36.6%/74.8MB
rowan syntax trees (`GreenNode`/`NodeCache`-built once per file during
parsing) retained for the whole project's lifetime, for every file, whether
or not it is ever touched by an LSP request after the initial bind.
Determine, concretely (reading `apex-binder`'s own Pass 2 (`resolve.rs`) and
`apexls-server`'s capability handlers, not guessing):

1. Does Pass 2/`resolve.rs`'s per-file body-binding walk ever need *another*
   file's own syntax tree (`GreenNode`/`SyntaxNode`), or only the current
   file's own tree plus the global `SymbolTable`/declared-signature data of
   other files?
2. Do any `apexls-server` capability handlers (hover, goto-definition,
   completion, semantic tokens, references, rename, etc.) need a *different*
   file's syntax tree beyond the one the request targets, and how often in
   practice (e.g. does goto-definition on file A ever need to render/query
   file B's tree, or just resolve to a location without materializing B's
   tree)?
3. What would it concretely cost -- in re-parse time, using the existing
   thread-local `NodeCache`-reuse pattern (diagnostics map ticket 28) -- to
   evict a file's `GreenNode` from steady-state retention once Pass 2 no
   longer needs it, and re-parse on demand (LRU-cached, sized to the LSP's
   actual working set of recently-touched files) whenever a later capability
   request needs that file's tree again?
4. Survey rowan's own crate docs/source, and rust-analyzer's own architecture
   (the closest real precedent this project already treats as authoritative
   per `research.md`), for guidance or precedent on this retention pattern --
   does rust-analyzer retain every file's tree forever, or something closer
   to on-demand/LRU?

Report whether an on-demand/LRU retention model is architecturally sound
(doesn't break any existing capability handler or Pass 2/Pass 1.5's own
re-bind correctness) and roughly how much of the 74.8MB it could plausibly
reclaim, as grounding for the follow-on decision ticket. This ticket only
investigates and reports -- it doesn't decide or build anything.

## Answer

### 1. Pass 2/`resolve.rs` never needs another file's own syntax tree

Confirmed by reading `resolve.rs` end to end (3,848 lines) and `BodyBinder`'s
own struct definition (`resolve.rs:800-822`): every `.syntax()` call inside
`resolve.rs` (grepped exhaustively, ~40 call sites) operates on an AST node
already reached by walking `block: &Block`/`&TriggerBlock` -- the *current*
file's own tree, handed in as a parameter -- never on a node fetched via
`program.syntax(some_other_file)`. `BodyBinder`'s cross-file dependencies are
exclusively symbol-level, never tree-level: `&SymbolTable`, `&SchemaIndex`,
`&StdlibIndex`, `&LabelIndex`, `&PageIndex`, plus a single `file: FileId`
(`resolve.rs:801-810`). `bind_body`/`bind_trigger_body` (`resolve.rs:829-862`,
`871-899`) are called once per method/constructor/trigger body, each call
scoped to exactly one file's own `Block`. `lib.rs:621-625`'s own doc comment
independently confirms the same shape from the caller's side: Pass 2's
parallel rebind is "keyed by `&Parse`... each parallel closure calls
`.syntax()` itself to build its own thread-local node from the shared
`Parse`" -- one `Parse`, one file, per closure. **This settles sub-question 1
cleanly: no, Pass 2 has zero need for any file's tree but the one it's
currently binding.**

### 2. Several `apexls-server` capability handlers *do* need other files' trees, and it can fan out to many files at once

Not as clean as Pass 2. Grepping `program.syntax(...)` across
`capabilities.rs` (17 call sites) shows most callers pass the *request's own*
target `file`/`ptr.file()` -- but several explicitly resolve a *symbol's
declaring file*, which is routinely a *different* file than the one the
request was made against:

- `doc_comment_for` (`capabilities.rs:1441-1454`, used by hover) calls
  `program.syntax(symbol.file)` where `symbol` is whatever declaration a
  hovered reference resolved to -- e.g. hovering a call to a method declared
  in a different file needs that *other* file's tree to extract its doc
  comment.
- `parameter_op_workspace_edit` (`capabilities.rs:2625-2654`, a
  reorder-parameters refactor) is the sharpest case: it calls
  `program.syntax(decl_symbol.file)` for the declaration, then loops
  `program.references_to(method_id)` -- *every call site project-wide* -- and
  calls `program.syntax(ptr.file())` for each one. A single request against a
  widely-called method can legitimately need dozens or hundreds of distinct
  files' trees materialized in one go.
- The same `program.references_to(id)`-driven fan-out pattern is used
  elsewhere for rename/find-references-shaped handlers (same file, similar
  loop shape at `capabilities.rs:2646` and comparable call sites).

**This means an LRU model can't assume "one file per request."** It's still
architecturally sound (each of these is still a `program.syntax(file)` call
that could transparently re-derive its `GreenNode` on a cache miss), but a
single rename/parameter-reorder request against a hot, widely-referenced
symbol would need to re-parse every file it touches that isn't already
LRU-resident -- a real, bursty cost concentrated in already-rare, already-more-
expensive-than-hover request types, not a steady per-keystroke cost. Worth
sizing the LRU capacity generously enough that this doesn't become the common
case for typical hover/completion/goto-definition traffic.

### 3. The real retention blocker isn't `parse_query`'s memoization -- it's `BindCache::file_parses`, a second, hand-rolled, never-evicted map

This is the most important, non-obvious finding of this research pass, and it
changes what the decision ticket actually needs to design against.

`crates/apex-binder/src/db.rs:208-216` already has `parse_query` as a real
`#[salsa::tracked(no_eq, returns(clone))]` function, keyed per-file by
`FileTextInput` (Stage 2, ticket 27/28's own shipped design). Salsa 0.28.2 --
already this project's pinned version (confirmed by reading
`~/.cargo/registry/.../salsa-0.28.2` directly, a primary source, not
inferred) -- natively supports exactly the mechanism this ticket asked
about: `#[salsa::tracked(lru = N)]` "bounds the number of memoized values
retained by the function" (`salsa-macros-0.28.2/src/lib.rs:445-446`), with a
real per-key LRU eviction policy (`salsa-0.28.2/src/function/eviction.rs`'s
`EvictionPolicy` trait: `record_use(id)` / `for_each_evicted(cb)`, keyed by
individual `Id`, i.e. individual files -- not an all-or-nothing global
switch). One caveat found directly in that source: eviction is "called once
per revision during `reset_for_new_revision`" -- it doesn't fire continuously
within a single revision, only when a *new* revision starts (i.e. after the
next edit lands), so a fresh cold bind's very first steady state wouldn't
shrink until at least one edit has occurred and triggered an eviction pass.

But adding `lru` to `parse_query` alone **would not reclaim any memory at
all**, because of a second, independent retention path: `BindCache` (the
pre-Stage-4, still-in-production cache `apexls-server` actually runs) has its
own plain `file_parses: FxHashMap<FileId, Parse>` field
(`crates/apex-binder/src/incremental.rs:171`), populated by
`cache.file_parses.insert(file, parse)` (`lib.rs:558`) for every file on every
bind, and never evicted for any reason except a file's outright deletion
(`lib.rs:478`). `BoundProgram::from_files_cached`'s own `parses =
cache.file_parses.clone()` (`lib.rs:776`) -- already confirmed cheap by ticket
03 (an `Rc`-bump on rowan's `GreenNode`, not a deep copy) -- means
`BindState`'s long-lived `BoundProgram` snapshot holds yet another `Rc` handle
into the exact same tree. Since `GreenNode` is `Rc`-based, evicting salsa's
own internal memo entry drops only *salsa's* reference; the tree stays fully
resident as long as `BindCache::file_parses`'s own permanent `HashMap` entry
(and/or `BoundProgram`'s cloned handle) is still alive -- which today is
forever, for every file, regardless of whether `parse_query` itself has an
`lru` cap.

`file_parses` is deliberately kept, not an oversight: the diagnostics map's
ticket 29 (already in this project's own resolved history) measured that
routing every file through `parse_query` on Pass 2's conservative
full-rebind path was *slower* for the warm-edit case than reusing this
persisted map's near-free `.clone()`, and chose to keep `file_parses` for
exactly that reason. **This is the real tension a decision ticket has to
resolve**: the existing warm-rebind-speed optimization (ticket 29's own
persisted `file_parses` map) is in direct conflict with the memory-reduction
goal (evicting cold files' trees) -- they can't both be true of the same data
structure unmodified. Candidate resolutions worth putting to that ticket,
not decided here: (a) re-measure whether routing through `parse_query`
directly is still slower now that the thread-local `PARSE_NODE_CACHE`
(shipped after ticket 29's own measurement) changes the cost profile; (b)
turn `file_parses` itself into a bounded/LRU-capped map independent of
salsa's own mechanism, sized to the actual dirty-file working set Pass 2's
full-rebind path touches; (c) some hybrid keeping only *currently open*
buffers' `Parse` values permanently resident (matching
`BindState::documents`'s existing open-buffer bookkeeping) and letting
everything else flow through a capped cache.

### 4. rust-analyzer/rowan precedent: inconclusive from docs alone, salsa's own source is the real answer

Rowan's own docs and rust-analyzer's architecture doc (both fetched) don't
state a specific retention policy for parsed trees in enough detail to cite
as precedent -- rust-analyzer's architecture doc only says source text
(not parsed trees specifically) is fully retained since it's small, and
describes computation as "lazy (on-demand)" without detailing its own
salsa query configuration. Rather than guess, this pass went to salsa's own
source directly (see point 3) and confirmed the `lru` mechanism exists and
is exactly shaped for this use case (per-key, tracked-function-scoped) --
that's a stronger, primary-source answer than an inferred rust-analyzer
precedent would have been.

### Answers to the ticket's own checklist

- **Is an on-demand/LRU retention model architecturally sound?** Yes for
  Pass 2 (zero cross-file tree dependency, confirmed directly). Yes in
  principle for capability handlers too, but sized generously -- some
  requests (parameter reorder, rename, find-references) legitimately fan out
  across every file referencing a symbol, not just the request's own target
  file.
- **How much of the 74.8MB could it plausibly reclaim?** Potentially most of
  it over a real editing session's lifetime (most of a large project's files
  are never touched by an open buffer or an LSP request in any given
  session), *but only if* `BindCache::file_parses`'s own separate permanent
  retention is also addressed -- adding salsa's `lru` to `parse_query` alone
  reclaims nothing, since `file_parses`'s own `Rc` handle keeps every tree
  alive regardless. This is the concrete design question ticket 07 needs to
  resolve, not a number this research pass can respons­ibly commit to before
  that architecture is chosen.
