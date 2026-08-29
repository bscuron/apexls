# Backlog: path to a robust, fast, featureful Apex LSP

Everything below is untracked in the day-to-day task list (which only
records completed implementation work). This file is the forward-looking
counterpart: what's actually needed to go from "we can parse, bind, and
resolve Apex in a batch CLI tool" to a real language server. Nothing here
is scheduled or committed to — it's a map of the gap, grouped by concern,
so a piece doesn't get lost until something downstream trips over it.

Current state, for context: `apex-lexer`/`apex-parser`/`apex-syntax`
produce a lossless CST + typed AST for full Apex (declarations,
statements, expressions, SOQL/SOSL). `apex-binder` turns that into a
project-wide symbol table with scope-aware, arity-and-type-narrowed
reference resolution, cross-referenced against local SFDX metadata,
parallelized across passes. `apexls-cli` was renamed to `apexls` and
graduated into a single binary with `server`/`ast`/`dead` subcommands
(`server` delegates to `apexls_server::run_server()`, shared verbatim
with the standalone `apexls-server` compatibility binary editor configs
invoke directly; `ast` parses a whole compilation unit and dumps its
tree, graduated from the old single-statement `parse_statement` debug
path; `dead` is a new batch dead-code reporter over the whole project or
a path-scoped subset, backed by the same `apex_binder::dead_symbols_in_file`
analysis the LSP diagnostics use). `apexls-server` is a complete, real LSP protocol
shell (§1 is fully checked off) that now background-rebuilds a real
`apex_binder::BoundProgram` on every edit, incrementally (§2, fully
checked off -- a warm single-file-edit rebind now measures ~5-7ms on the
real NPSP corpus, down from ~677ms cold, after further memory/perf work
past the original ~17ms). §3's whole "buildable now, no new binder work
needed" list is checked off: `textDocument/hover`, `textDocument/definition`,
`textDocument/documentSymbol`, `workspace/symbol`, `textDocument/foldingRange`,
`textDocument/selectionRange`. `textDocument/references`/`textDocument/documentHighlight`
are also now done, backed by a new incrementally-maintained reverse
index on `ReferenceTable` (`by_symbol`, plus a second `by_external`
index keyed by a new `ExternalKey` so a real stdlib method/property or
Salesforce schema field -- neither of which has a `SymbolId` -- is also
found). `textDocument/rename` is done
too, conservatively scoped (refuses rather than guesses on an ambiguous
target, an override-chain method, or a colliding name) and preceded by
a new `crate::conversions` module that narrows most real-world overload
calls to a single candidate via Apex's actual implicit-conversion rules,
rather than the old arity-only narrowing. `textDocument/codeAction` and
`textDocument/publishDiagnostics` are both started too, via dead-code
detection (`private`/`public` methods/fields/properties/constructors and
plain locals; VF-referenced classes and platform-invocation-annotated
public members are exempted, see below) paired with a "Remove unused
..." quick-fix, and shared with the new `apexls dead` batch CLI report.
`textDocument/prepareCallHierarchy`/`callHierarchy/incomingCalls`/
`callHierarchy/outgoingCalls` are also done now, computed entirely on
demand per request rather than needing any precomputed whole-project
call graph. `textDocument/signatureHelp` is done too, also on demand:
the candidate overload set is recomputed fresh from the resolved call's
`container`/`name` (not read back from the narrowed `Resolution`), and
the active parameter is tracked by counting `Comma` tokens rather than
resolved argument nodes, so a dangling trailing comma with nothing typed
after it yet still advances to the next parameter slot.
`textDocument/inlayHint` is done too (a `paramName:` label per call
argument, same on-demand posture, skipped for any ambiguous or stdlib
call). `textDocument/completion` is done too (member-access and bare-
identifier contexts; see below for the full writeup), including a real
parser-AST-layer bug fix it surfaced (`FieldExpr`/`MethodCallExpr`/
`QualifiedName`'s member-token accessors mishandling a dangling dot with
nothing typed after it). Next up is §3's remaining "needs new binder-side
work first" item (`semanticTokens`), or §4's still-open standard-library/
schema type-model gap.

## 1. Protocol / server layer

- [x] Pick and wire a transport/protocol crate and get a minimal
      `initialize`/`initialized`/`shutdown`/`exit` handshake working
      over stdio. **Done** in `crates/apexls-server`, built on
      [`async-lsp`](https://github.com/oxalica/async-lsp) rather than
      `tower-lsp`/`tower-lsp-server` -- the latter two process
      notifications *asynchronously*, which can reorder e.g. two
      `didChange` notifications relative to each other; async-lsp
      processes them synchronously, matching what the LSP spec actually
      requires. Verified against a real subprocess exchanging framed
      JSON-RPC (see the protocol-test bullet below), not just unit-level
      method calls.
- [x] Document synchronization: `textDocument/didOpen`, `didChange`,
      `didClose`, `didSave`. **Done**, full-document sync (not
      incremental) -- the simplest correct baseline; incremental sync
      is really a facet of the incremental-reparse/-rebind work in §2,
      not a standalone protocol-layer task, so it's tracked there
      instead of here.
- [x] Workspace folder handling. **Decided: single-root only, by
      design, not just an unimplemented gap.** An SFDX org is one flat
      Apex namespace (`SymbolTable::top_level` is already project-wide,
      not per-file, for exactly this reason), so merging two *unrelated*
      workspace folders into one `BoundProgram` would be actively
      wrong -- colliding names, references falsely resolving across
      projects with nothing to do with each other -- and LSP gives no
      signal to tell "these are the same org" apart from "these just
      happen to be open together." `Backend::initialize` resolves the
      first `workspaceFolders` entry (falling back to the deprecated
      `rootUri` for older clients) and ignores the rest, with a logged
      warning. No binder integration consumes the resolved root yet
      (there's no binder integration wired into the server at all
      yet), but the *policy* is settled, not deferred.
- [x] Configuration: `workspace/didChangeConfiguration`,
      `initializationOptions`. **Done** as plumbing -- `Backend` captures
      `initializationOptions` at `initialize` and overwrites it wholesale
      on any later `didChangeConfiguration`, kept as opaque
      `serde_json::Value` (no typed schema yet, since nothing consumes
      specific settings -- e.g. where to find SFDX metadata, whether to
      bundle standard-library stubs, both still §4 gaps with nothing to
      configure). Verified via the protocol test accepting a
      `didChangeConfiguration` notification without erroring or wedging
      the server.
- [x] Position encoding. **Done** -- `crates/apexls-server/src/line_index.rs`
      negotiates `positionEncodingKind` in `initialize` (`ClientCapabilities.general.positionEncodings`,
      preferring UTF-8 whenever offered -- zero conversion needed
      against our own UTF-8 byte-offset `TextRange`/`TextSize` -- else
      falling back to UTF-16, the LSP-mandated default) and always
      states the choice explicitly in `ServerCapabilities.positionEncoding`
      rather than relying on the implicit default. Also built a real,
      unit-tested `LineIndex` (byte offset <-> `Position`, correct for
      UTF-8/UTF-16/UTF-32 including surrogate-pair characters) --
      genuinely useful infrastructure even with no consuming capability
      yet, the same way `apex-binder`'s `AstPtr`/`SyntaxPtr` were built
      ahead of goto-definition. (Caught a real shadowing bug in its
      `\r`/`\n`-stripping logic via its own test suite before it shipped.)
- [x] Cancellation. **Turned out to already be done**, not a gap: read
      `async-lsp`'s own source and confirmed `ConcurrencyLayer` (already
      in our middleware stack since the very first scaffolding)
      intercepts `$/cancelRequest` directly and aborts the matching
      in-flight request's future automatically -- no code needed from
      this project at all. Honest caveat: there's no end-to-end test
      proving it *actually cancels something*, since every request
      handler so far (`initialize`, `shutdown`) completes near-instantly
      and there's nothing slow to meaningfully cancel yet; that becomes
      naturally testable once a real (potentially slow, binder-backed)
      request exists.
- [x] Protocol-level integration test harness: spin up the server,
      drive it over stdio with real LSP JSON-RPC messages, assert
      responses. **Done** --
      `crates/apexls-server/tests/handshake.rs` spawns the real built
      binary and drives it through `initialize` -> `initialized` ->
      `didOpen`/`didChange` -> `shutdown` -> `exit`, asserting on the
      wire-level JSON responses and clean process exit.
- [x] Misuse/robustness coverage: the server must never panic, hang, or
      answer wrongly when driven in ways a well-behaved editor never
      would. **Done** -- `crates/apexls-server/tests/misuse_robustness.rs`,
      8 protocol-level cases through the real binary: no `workspaceFolders`/
      `rootUri` at all (real single-file mode -- `bind.program` stays
      `None` forever, so every capability must degrade to a clean `null`
      rather than hanging on a rebuild that will never happen); a
      workspace root that was never created on disk; a project root with
      zero `.cls`/`.trigger` files that still binds a real file opened
      inside it normally; a non-Apex-extension file (never a bind
      candidate, `apex_discover` only ever looks at extensions) that
      doesn't affect a real file next to it; a `.cls` file with
      completely non-Apex garbage content, hovered at every offset (not
      just one -- an off-by-one is more likely to surface at a specific
      position than uniformly); a `.cls`/`.trigger` extension-vs-content
      mismatch (parsing dispatches purely by extension,
      `crates/apex-binder/src/lib.rs`); a position far past a file's own
      end; and a request against a URI that was never opened and doesn't
      exist. All 8 passed against the already-existing implementation
      with no fixes needed -- this is a coverage gap being closed, not a
      bug being fixed, and it's now a real regression guard: the "single-
      file mode" behavior in particular (`BindState::worker_active`,
      `wait_for_rebuild`'s doc comments) was previously only justified by
      code comments, not pinned by a test that would actually catch it
      breaking.
- [x] **`BindState`'s locks switched from `std::sync` to `parking_lot`,
      closing a real architectural risk an `unwrap()`-safety audit
      surfaced.** Every capability handler holds `bind.program`'s read
      guard across the *whole* request (`hover`/`completion`/
      `references`/...); with `std::sync::RwLock`, a panic anywhere in
      that call graph -- today provably none, per the same audit, but a
      standing risk for anything added later -- would poison the lock,
      and `async_lsp`'s `CatchUnwindLayer` (`main.rs`) turning that one
      panic into a clean per-request error would do nothing to un-poison
      it: every *later* request's own `.read()`/`.write()` would then
      also panic, forever, silently degrading the whole session to
      errors-only until the client restarts the server, with no crash
      and no signal beyond that. `parking_lot`'s locks never poison -- a
      panic under a held guard just unwinds normally and the lock is
      fully usable again on the next request -- closing the whole failure
      class structurally rather than relying on every future capability
      never panicking under a held guard. All four `BindState` locks
      (`program`/`cache`/`documents`/`watcher`), not just `program`,
      for the same reason applied consistently. Verified with a new
      `crates/apexls-server/src/lib.rs` unit test that deliberately
      panics while holding `program`'s write guard (via `catch_unwind`)
      and confirms the lock is still fully usable immediately after --
      would fail (poisoned forever) against the old `std::sync::RwLock`.

## 2. Incremental computation ("fast")

This is the single biggest gap between what exists and a server that
feels good to type in. `BoundProgram::from_files` is a full-project
batch rebuild; nothing about it is incremental.

Step 1 (prerequisite, not itself a checklist item): `apexls-server`'s
`Backend` now actually calls into `apex-binder` -- a background
`spawn_blocking` task rebuilds a `BoundProgram` on `initialized` and every
`didOpen`/`didChange`/`didClose`, naively (no debouncing, no cancelling a
rebuild a newer edit already superseded). Nothing consumes the bind
through the protocol yet (that's §3) -- this exists purely so the items
below could be measured against something real instead of guessed at.

- [x] Incremental reparse -- **coarse-grained (file-level) slice done,
      not true sub-file incremental reparse.** `apex_binder::BindCache`
      (`BoundProgram::from_files_cached`) skips re-lexing/re-parsing any
      file whose content is byte-for-byte identical to the previous
      rebuild. True sub-file incremental reparse (edit-delta into a
      single file's tree, reusing unaffected subtrees via rowan's
      structural sharing) is a real `apex-parser`-level feature this
      crate has no hooks for yet (only whole-string `parse_*` entry
      points exist) -- deliberately not built, since the measurement
      below shows it isn't this server's bottleneck even without it.
- [x] Incremental rebind. **Done for the dominant case.** `SymbolId`
      became a stable `{file, local}` identity (not a flat project-wide
      index) and `FileId` became a persistent, session-long identity
      (`FileTable`), instead of "a file's position in this call's
      directory walk" -- together these make it sound to patch
      `SymbolTable`/Pass 2 output file-by-file across calls instead of
      recomputing everything, since an unrelated file's ids never shift.
      `BoundProgram::from_files_cached` now: reruns Pass 1 (collect) only
      for files whose content actually changed; compares each changed
      file's new declarations against its old ones by *shape*
      (kind/name/container/type/modifiers, deliberately not byte ranges,
      which shift on any earlier edit) to decide whether anything
      project-wide could have changed; skips Pass 1.5 (inherit) and every
      `SymbolTable` derived index rebuild entirely when nothing did; and
      reruns Pass 2 (body resolution) only for the changed files in that
      case -- the common "typing inside a method body" edit, which never
      touches a declaration. A signature/field/class-shape change still
      falls back to a full project-wide rebind, identical cost to before
      -- correct, just not sped up (see the honest scope note below).
- [x] A caching/memoization strategy. **Built, not salsa -- a hand-rolled
      per-file cache (`BindCache`) sized to this project's actual shape**
      rather than a general incremental-query engine: `FileTable` (stable
      `FileId`s), a persisted `SymbolTable` patched file-by-file, and
      per-file Pass 2 output (`FileBodies`), all `Arc`-shared per file so
      that assembling one call's independent `BoundProgram` snapshot only
      costs an `Arc` clone per *unaffected* file, not a deep copy of its
      data -- catching a real regression along the way (see the
      measurement below).
- [x] Warm background indexing: covered by Step 1's background-task
      rebuild (the main loop never blocks on a bind) plus this step's
      incremental patching (a rebuild after the first one is now cheap in
      the common case, not just non-blocking). Answering a request that
      arrives before the *first* bind completes is still untested --
      there's no request that consumes the bind yet (§3), so there's
      nothing real to test that against until one exists.
- [x] Profile *real* single-edit request latency. **Measured** via
      `apex-binder/benches/binder_bench.rs`'s `corpus/warm_rebind_after_one_file_edit`
      case (warms the cache once, then times a full `from_files_cached`
      call per simulated single-file edit, real NPSP corpus, criterion):
      - `corpus/bind_npsp_full` (cold, no cache): **~431ms** median (was
        ~677ms originally -- `Arc`-backed `SymbolTable`/`FileBodies`, and
        no longer walking the directory tree twice per call -- see
        below -- both cut real overhead on the cold path too).
      - `corpus/warm_rebind_after_one_file_edit` (warm cache, one file's
        body-only edit): **~17ms** median -- a **~96% reduction** from
        the ~438ms baseline once incremental rebind existed but before
        the discovery/schema-caching fix below, and a **~40x** speedup
        from the original ~677ms fully-cold baseline.
      - **Two real regressions caught and fixed along the way, both by
        the same discipline -- benchmark the actual change, not just the
        feature it's meant to enable:**
        1. The first implementation of incremental rebind made this
           benchmark *worse* (~700ms) than the step before it, not
           better -- `from_files_cached` was skipping recomputation but
           still deep-cloning the *entire* `SymbolTable` and every
           file's cached references/scopes into each call's independent
           snapshot. Fixed by `Arc`-wrapping `SymbolTable`'s per-file
           data/derived-index bundle and Pass 2's per-file output, so
           assembling a snapshot is a handful of pointer clones for an
           unaffected file, not a deep copy -- this got warm-edit
           latency to ~78ms.
        2. `apex_discover::discover` and `apex_metadata`'s SFDX
           object/field walk were still re-walking (and, for metadata,
           re-parsing every XML file in) the whole directory tree on
           *every* call, unconditionally, uncached, and -- a separate,
           real bug this surfaced -- `apex-binder` was doing that walk
           **twice** per call (`apex_metadata::discover_sobjects`
           re-walked internally instead of reusing the walk
           `apex-binder` had just done). Fixed by caching the
           `Discovery`/`SchemaIndex` pair in `BindCache`, only redone
           when there's no cached walk yet or an `overrides` path names
           a file the cached walk has never heard of (a newly created,
           already-open file) -- and by adding
           `apex_metadata::sobjects_from_discovery`/`SchemaIndex::from_discovery`
           so the two walks collapse into one whenever a fresh one is
           actually needed. Fixing this required also fixing a related
           latent bug the caching made load-bearing rather than a rare
           race: a file deleted between calls used to leave stale
           symbols behind forever if `discovery` itself wasn't
           re-walked (it's cached now, so it almost never is) --
           `current_ids`/`current_files` are now derived from which
           candidate files *actually* parsed successfully this call
           (`parsed`), not from the raw candidate list, so a deletion
           self-corrects (the file just fails to read) without needing a
           fresh walk at all. This dropped warm-edit latency from ~78ms
           to ~17ms.
      - **A measurement red herring, for the record:** an intermediate
        reading during work on the discovery-caching fix showed warm-edit
        latency at ~645ms -- direct `Instant`-based timing placed *inside*
        the exact same benchmark run showed the real per-call cost was
        still ~17ms the whole time, so the ~645ms figure was criterion/
        machine-level measurement noise (this project has hit this exact
        false-regression pattern before, from background-process CPU
        contention), not a real regression -- confirmed by re-running
        clean and getting ~17ms consistently across repeated runs.
      - **Honest scope note (still applies):** this is not full
        per-reference dependency tracking (the original wording: "which
        symbols/scopes/references become stale when file X's declarations
        change"). It's one conservative, provably-safe rule -- if *any*
        file's declarations changed, rebind the whole project (identical
        cost to before); otherwise, rebind only the changed files. That
        rule is sized to the dominant real editing pattern (typing inside
        a method body), not the general case, and is honest about not
        being faster yet for a signature-changing edit.
      - **What's left in the ~17ms, honestly:** a file added to disk but
        never opened in the editor (e.g. `git pull`, a build tool) still
        won't be picked up until something else triggers a rediscovery --
        there's no filesystem-watcher integration yet (`workspace/didChangeWatchedFiles`
        isn't wired up), a real, separate, and already-tracked gap, not
        papered over by this fix. Metadata XML edited with no
        corresponding Apex-file signal has the same limit.
- [x] **Function-level breakdown of the cold/warm bind costs, and a
      ranked follow-up plan.** The measurements above only ever timed
      `from_files`/`from_files_cached` from the outside; nothing broke
      down where inside the pipeline the ~431ms cold / ~17ms warm totals
      actually go. Instrumented the real pipeline with the
      [`hotpath`](https://github.com/pawurb/hotpath-rs) crate --
      `#[hotpath::measure]` on `apex_discover::discover`,
      `apex_parser::parse_compilation_unit`/`parse_trigger_unit`,
      `collect::collect_compilation_unit`/`collect_trigger_unit`,
      `inherit::resolve_inheritance`, `resolve::bind_body`/
      `bind_trigger_body`/`bind_initializer`, plus `measure_block!`
      around `from_files_cached`'s named stages -- gated behind
      `hotpath`/`hotpath-cpu`/`hotpath-alloc` Cargo features that are
      true no-ops when off (verified `apexls-server`/`apexls-cli` build
      unaffected), cascaded through `apex-parser`/`apex-discover`'s own
      same-named features. A new
      `crates/apex-binder/examples/cpu_profile.rs` (sibling to
      `mem_profile.rs`, same "ad-hoc, not a permanent fixture" status)
      mirrors `binder_bench.rs`'s two corpus cases via
      `hotpath::HotpathGuardBuilder` and a `hotpath::CountingAllocator`
      global allocator (needed for real, nonzero `hotpath-alloc`
      numbers -- an allocator-tracking library, same as `dhat`, doesn't
      count anything without one). Run with `cargo run -p apex-binder
      --release --features hotpath,hotpath-alloc --example cpu_profile`
      against the real NPSP corpus (`hotpath-cpu`, sampling-based CPU
      attribution, is Linux/macOS only -- unavailable on this Windows
      dev machine, so not used here). One real run (totals: 373.6ms
      cold / 6.4ms warm -- in the same ballpark as the criterion
      baselines above, not identical, since this is a single instrumented
      run rather than criterion's warmed-up statistical sampling):
      - **Cold bind, two previously-invisible costs:**
        1. `discover_and_build_schema` (the `SchemaIndex::from_discovery`
           SFDX metadata XML parse, run every time discovery is
           refreshed) took 78.57ms -- 21% of the whole cold bind --
           of which `apex_discover::discover`'s own directory walk was
           only 9.41ms. The remaining ~69ms is metadata XML parsing that
           was never broken out as its own cost before, and unlike every
           other pipeline stage, isn't parallelized.
        2. `pass2_merge_bodies` -- the **sequential** step that remaps
           each bound body's sentinel ids and merges it into
           `cache.bodies` -- took 37.89ms wall (10%, comparable to the
           *entire parallel* Pass 2 binding stage's 45.77ms) and was the
           single largest individual allocator in the whole cold bind:
           110.0 MB exclusive, 20% of all 544.5 MB allocated project-wide
           -- more than 25x what the parallel binding stage it follows
           allocates directly (4.1 MB). A single-threaded stage that
           allocates a fifth of a whole project bind's memory is a real
           optimization candidate.
      - **Warm single-edit rebind: both dominant costs scale with total
        project size (~1044 files), not edit size**, despite incremental
        rebind already correctly skipping the actual re-parse/re-collect/
        re-resolve work for unchanged files:
        1. `stage_1a_read_and_parse` took 55% of warm-rebind wall time
           (3.55ms of 6.42ms) -- because it still calls `std::fs::metadata`
           (a stat syscall) plus a cache lookup for *every* one of the
           ~1044 candidate files on every single call, just to determine
           which one(s) are dirty, even when only one file changed.
        2. 69% of warm-rebind's allocated bytes (836.3 KB of 1.2 MB) were
           attributed to `from_files_cached`'s own top-level frame, outside
           every instrumented sub-stage -- pointing at the un-instrumented
           "glue" code: the `candidates: Vec<(PathBuf, FileId)>` built by
           cloning every discovered path (`crates/apex-binder/src/lib.rs`
           Stage 0) and, at the end, the "assemble this call's independent
           owned snapshot" step that rebuilds `files`/`file_ids`/`parses`
           -- three project-wide `FxHashMap`s, each keyed by a cloned
           `PathBuf` -- from scratch on *every* call, from all of `parsed`,
           regardless of how many files actually changed
           (`crates/apex-binder/src/lib.rs`, "Assemble this call's
           independent, owned snapshot").
      - **Ranked follow-up -- all four implemented and re-measured with
        the same `cpu_profile` harness. Wall-clock totals are noisy
        run-to-run (this machine has hit that before, see the
        `warm_rebind_after_one_file_edit` bench's own "measurement red
        herring" note above) -- allocation totals aren't, and are the
        more reliable signal below:**
        1. **Persist `files`/`file_ids`/`parses` in `BindCache`, patched
           per-file instead of rebuilt from every parsed file.** Done
           (`cache.paths`/`cache.path_ids`/`cache.file_parses`,
           `crates/apex-binder/src/incremental.rs`). **Smaller payoff
           than hoped, honestly**: the owned-snapshot assembly still
           needs one `.clone()` of each persisted map per call (a
           `BoundProgram` must stay an independent snapshot -- see its
           doc comment on why -- and `apexls-server`'s `BindState` keeps
           the *previous* `BoundProgram` alive for the whole duration of
           the next rebuild, so an `Arc`-sharing scheme with
           `Arc::make_mut` was considered and rejected: its zero-copy
           path only fires when nothing else still holds the old data,
           which isn't true for that caller's real usage pattern -- it
           would only have looked like a win in this synthetic harness,
           which drops its warm-up call's result immediately). What this
           safely achieves: one fewer redundant `PathBuf` clone per
           unchanged file per call (3 down to 2 -- see point 2 below for
           where the third one went). Warm-rebind allocation held flat
           (~1.1-1.2 MB total, both before and after) rather than
           shrinking -- an honest negative result, not a regression.
        2. **Reduce Stage 1a's redundant `PathBuf` clone.** Changed
           `candidates.par_iter()` to `.into_par_iter()` so each
           candidate's already-owned `PathBuf` moves into its `ParsedFile`
           instead of being cloned again (`crates/apex-binder/src/lib.rs`).
           Safe and unconditional (every file, every call), but small in
           practice: Stage 1a's cost is dominated by the `std::fs::metadata`
           stat syscall itself, not this clone. The originally-planned
           "skip re-stat'ing unchanged files entirely" was **investigated
           and deliberately not implemented** -- `crates/apex-binder/tests/incremental_rebind.rs`'s
           `deleting_a_file_leaves_no_stale_symbols_even_with_a_stale_cached_walk`
           test exists precisely because deletion detection depends on
           every candidate actually being re-stated/re-read each call;
           skipping that for cached files would silently break it. Stage
           1a's ~52-66% share of warm-rebind wall time is therefore
           mostly irreducible without a real filesystem-watcher signal
           (an already-tracked, separate gap -- see above), not something
           this round could safely cut further.
        3. **Parallelize `SchemaIndex::from_discovery`'s SFDX XML parse
           with `rayon`.** Done (`apex_metadata::sobjects_from_discovery`,
           `crates/apex-metadata/src/discover.rs`: the `field_meta_files`
           read+parse loop is now a `par_iter().filter_map(...).collect()`
           followed by a cheap sequential grouping merge, the same
           parallel-map-then-merge shape this pipeline already uses
           throughout). **Clear, large, reproducible win**: `discover_and_build_schema`
           dropped from ~79-87ms to a consistent ~21ms across repeated
           runs (roughly a 73-76% reduction in that stage alone), a
           guaranteed ~60ms+ cut to every cold bind regardless of
           machine noise.
        4. **Reduce `pass2_merge_bodies`'s allocation.** Two changes:
           (a) `ReferenceTable::map_ids`+`merge_into` (two separate steps
           -- build a whole new intermediate `ReferenceTable`, then drain
           it into the target) fused into one `map_ids_into` that remaps
           and inserts directly into `target` (`crates/apex-binder/src/reference_table.rs`),
           eliminating one full intermediate `FxHashMap` allocation per
           body, project-wide; (b) `extra_symbols`/`file_bodies.scopes`
           now pre-sized per file instead of growing via repeated
           `.extend()`/`.insert()` calls (`crates/apex-binder/src/lib.rs`).
           **Clear, large, reproducible win, and noise-free** (allocation
           totals don't vary run-to-run the way wall-clock does):
           `pass2_merge_bodies`'s own exclusive allocation dropped from
           110.0 MB to a consistent 75.6 MB (-31%), and a cold bind's
           *total* allocation dropped from 544.5 MB to ~501-502 MB (-8%)
           almost entirely attributable to this one fix.
        - **Net effect**: a cold bind's total allocation is down ~8%
          (544.5 MB -> ~502 MB) and wall time is consistently lower than
          the pre-optimization baseline (373-393ms) across repeated runs
          (293-337ms observed, noisy but never overlapping the old
          range), with the metadata-parse parallelization (#3) as the
          single largest, most confidently-attributable contributor.
          Warm single-edit rebind's wall time and allocation are within
          measurement noise of the baseline (~6-7ms, ~1.1-1.2 MB) --
          honestly, this round's changes mostly targeted cold-bind cost;
          cutting warm-rebind's dominant cost further needs either a
          filesystem-watcher signal (to safely narrow Stage 1a's
          re-stat pass) or a real architectural change to how
          `BoundProgram` snapshots share data with `BindCache` across
          calls (rejected this round as described in #1), not a
          same-shaped incremental tweak.
- [x] **Actually run `examples/mem_profile.rs` (it existed but its output
      was never recorded here) and act on what it found.** `dhat`-profiled
      `apexls-server`'s real steady-state shape (`BindCache` and a
      `BoundProgram` snapshot both alive at once, over the real NPSP
      corpus): **134.7 MB retained across 1,047,791 live heap blocks.**
      Ranking `dhat`'s allocation sites by size, rowan's green-tree node/
      token interning dominated everything else -- roughly 90 MB and
      ~750,000 of those blocks, about two-thirds of retained memory.
      Root cause: `crates/apex-parser/src/event.rs` called
      `GreenNodeBuilder::new()` -- a **fresh, empty `rowan::NodeCache`
      per file**. A `NodeCache` is rowan's structural-sharing interner:
      an identical node/token (a `public` keyword, a `;`, a small
      wrapper node, ...) built twice becomes one `Arc`-shared allocation
      reused via clone, *if* both builds go through the same cache. With
      1,044 independent caches (one per file), that sharing only ever
      happened within one file, never across the project's shared
      keyword/punctuation/short-identifier vocabulary.
      **Fix**: `apex_parser::parse_compilation_unit_with_cache`/
      `parse_trigger_unit_with_cache` (new, alongside the original no-
      cache entry points, which now just delegate through a throwaway
      cache -- every existing caller is unaffected) let a caller thread a
      shared `rowan::NodeCache` (re-exported as `apex_syntax`/
      `apex_parser::NodeCache`) through many parses.
      `BoundProgram::from_files_cached`'s Stage 1a now shares one
      `NodeCache` across every file `rayon` groups into the same `fold`
      segment, rather than one per file.
      **Two honest false starts before landing on `fold`, both measured
      via `cargo bench -p apex-binder`, not guessed:**
      1. First attempt pre-partitioned `candidates` into fixed 64-item
         chunks via `.chunks(64).flat_map(...)`, each chunk building its
         own `NodeCache`. Memory win was real (134.7 MB/1,047,791 blocks
         -> 127.9 MB/908,240 blocks), but `.chunks()` pays for
         materializing a `Vec` per chunk *and* a `Vec` of per-chunk
         results before flattening -- a fixed per-call cost that doesn't
         care how much actual parsing happens inside it. Fine for a cold,
         1,044-file build; **regressed the far more latency-sensitive
         warm single-file-edit rebind by ~47%** (measured, not
         estimated), since nearly all ~1,044 candidates on a warm rebind
         never parse at all (a cache/stat hit) and paid pure grouping
         overhead for zero benefit.
      2. Second attempt replaced the fixed chunk size with `rayon`'s
         `fold` (accumulate `(NodeCache, Vec<ParsedFile>)` per adaptively-
         sized segment, no pre-partitioning `Vec` needed) -- but with no
         other hint, `rayon`'s default splitting for a plain `Vec` source
         favors near-perfect load balance over grouping, and split so
         finely that almost no cross-file sharing happened at all
         (measured: memory barely moved from the no-sharing baseline).
      3. **What actually worked**: `fold`, plus `.with_min_len(64)` so
         `rayon`'s adaptive splitter won't create a segment smaller than
         64 candidates (it can still make segments larger, or split
         differently, entirely at its own discretion -- this only sets a
         floor). Deliberately *not* a chunk count or size derived from
         `rayon::current_num_threads()`/core count: `with_min_len`'s `64`
         describes "how many files need to share a cache for the
         interning to pay off," a property of the workload, not of
         however many cores happen to be on whichever machine runs this
         -- `rayon` still freely decides how many such groups to make,
         scaled to however many threads actually exist.
      **Final measured result** (`examples/mem_profile.rs`, `cargo bench
      -p apex-binder`, each run repeated to separate real signal from
      this machine's already-documented run-to-run noise): retained
      memory **134.7 MB/1,047,791 blocks -> 128.1 MB/912,193 blocks**
      (-4.9% bytes, -12.9% blocks), with cold-bind (~355-370ms) and warm-
      rebind (~5.7-6.1ms) wall time both unchanged within noise -- a real
      but modest win, well short of the ~90 MB the raw green-tree
      allocation total first suggested, because most of that total was
      already being deduplicated *within* each file by its own (still
      per-file-at-minimum) cache; only the slice of it that's genuinely
      shared *across* files (common keyword/punctuation/short-node
      vocabulary) was ever recoverable this way. Smaller, lower-priority
      findings from the same profiling pass, not yet acted on:
      `ReferenceTable::map_ids_into` (`reference_table.rs`) pre-sizes
      `target.resolutions` before its merge loop but not
      `target.by_symbol`, which grows from empty via repeated rehashes;
      `SymbolTable::rebuild_indices`'s derived-index maps
      (`members_by_name`/`by_name_ci`/`members_of`) have the same
      grow-from-empty pattern; `apex_lexer::tokenize`'s `Vec<Token>`
      isn't pre-sized from the source length. Each is a few MB, not the
      dominant cost this item targeted.
- [x] **Bounded LRU eviction of parsed rowan trees -- investigated,
      measured, and deliberately rejected.** A follow-up `mem_profile.rs`
      run (after fixing a `dhat` snapshot-timing bug where `_profiler`
      dropping last meant the JSON's per-site breakdown reflected almost
      nothing -- fixed by `std::mem::forget`-ing `cache`/`program` after
      the stats snapshot instead of letting them drop normally) found
      157.6 MB retained, of which rowan's parsed trees are the single
      largest slice (~59.4 MB / 37.7%, ahead of `reference_table` at
      31.5% and everything else combined). The idea: bound
      `BindCache`'s parse storage with an LRU (the `lru` crate) instead
      of retaining every file's `Parse` forever, reparsing on a miss
      (cheap, and this project's own `NodeCache`-sharing already makes
      cold parsing fast) -- "full access-recency," per an explicit
      choice between that and edit-recency-only, so both
      `BoundProgram::from_files_cached`'s Stage 1a *and* read-only
      navigation (`BoundProgram::syntax`) would count toward what stays
      resident.
      **Two real correctness bugs surfaced during implementation, both
      fixed before the approach was rejected on performance grounds
      alone:**
      1. Sharing one mutable `Arc<LruCache>` between `BindCache` and
         every `BoundProgram` snapshot is unsound, not just an unwanted
         coupling: a snapshot's `SyntaxPtr`s are computed against one
         specific tree's byte offsets, and a later rebind overwriting a
         *shared* cache entry for the same file silently swaps in a
         different tree underneath an older, supposedly-frozen snapshot
         still in use -- corrupting every offset it hands out (a real,
         reproduced rowan `cursor.rs` panic). Fixed by giving each
         snapshot its own frozen copy (taken at construction) plus a
         private, never-shared overflow map for misses during that
         snapshot's own lifetime.
      2. Even with per-snapshot isolation, a snapshot's frozen copy still
         has to force-include every `overrides`-backed (actively edited,
         unsaved) file regardless of the bounded cache's state: with more
         live candidates than the cache's capacity, unrelated files
         processed later in the same Stage 1a round can evict an edited
         file's just-computed entry before the snapshot is even taken,
         leaving `syntax()`'s reparse-on-miss fallback as the only
         source -- and that fallback reads from disk, which is stale
         (not ground truth) for a file whose real content only lives in
         `overrides`. Reproduced as the same rowan panic via a real
         `apexls-server` subprocess test (`rapid_edit_burst.rs`) before
         being fixed.
      **Why rejected despite both bugs being fixable**: Stage 1a's
      warm-rebind fast path (reuse an unchanged file's tree instead of
      reparsing it) depends on that tree still being resident -- bounding
      the cache directly undermines the exact mechanism
      `corpus/warm_rebind_after_one_file_edit` exists to protect,
      whenever a project's live working set exceeds the cache's
      capacity. Measured via `cargo bench -p apex-binder` at a capacity
      of 200 files against the ~1,044-file NPSP corpus: `bind_npsp_full`
      (cold) unchanged (433ms vs. 443ms baseline, within noise -- Stage
      1a never reads back through the cache mid-call, so cold bind was
      never at risk), but `warm_rebind_after_one_file_edit` regressed
      **+5287% (6.02ms -> 322ms, ~54x slower)** -- by the end of the very
      first cold bind, insertion order alone has already evicted most of
      the corpus from a 200-slot cache, so nearly every "unchanged" file
      on the next edit misses and gets fully reparsed instead of reused,
      making a warm rebind cost nearly as much as a cold one. A capacity
      large enough to avoid this for a project NPSP's size would need to
      approach the project's own file count, at which point the memory
      win mostly disappears for exactly the projects large enough to
      need it. No capacity threads that needle for both a small project
      (where eviction would rarely matter anyway) and a large one (where
      it would matter, but any workable capacity undoes the memory
      savings) -- reducing rowan tree memory needs a different approach
      that doesn't trade against Stage 1a's reuse invariant, not a bound
      on the same cache that invariant depends on.

## 3. Feature surface

What's reachable *today* from `apex-binder`'s output vs. what a
featureful LSP needs. Ordered roughly by how directly current data
supports each one.

**Buildable now, no new binder work needed:**
- [x] `textDocument/hover` -- **done.** Two new position-resolution
      primitives on `BoundProgram` (`resolution_at`/`symbol_at`,
      `crates/apex-binder/src/lib.rs`) back it: `resolution_at` finds the
      token at a byte offset and walks up to the nearest ancestor node of
      a kind Pass 2 actually registers a `Resolution` for (confirmed
      exhaustive by reading every `refs.set` call site: `NameExpr`/
      `FieldExpr`/`Type`/`QualifiedName`/`MethodCallExpr`/`CallExpr`/
      `NewExpr`/`SoqlFieldName`); `symbol_at` separately answers "is the
      cursor on a declaration's own name" (never itself a recorded
      reference). `apexls-server/src/capabilities.rs`'s `describe_symbol`
      renders a resolved `Symbol` as a fenced-code signature (modifiers +
      kind + type + name, plus a method/constructor's params via
      `SymbolTable::params`) with its doc comment (`HasDocComment`)
      appended. Deliberately out of scope for this pass: rendering
      `apex_metadata` schema info for a `SchemaObject`/`UnknownSchema`
      resolution (stays no-hover, consistent with §4's still-open
      stdlib/schema gaps) and disambiguating `Candidates` beyond showing
      the first plus an honest "+N more overload(s)" note.
- [x] `textDocument/definition` -- **done**, same `resolution_at`
      primitive, `Resolved` -> `GotoDefinitionResponse::Scalar`,
      `Candidates` -> `Array` of every candidate's location (not a
      silently-picked one). Deliberately *not* wired to `symbol_at` --
      "go to definition" on your own declaration has nowhere useful to
      go, so that stays a no-op rather than a special case.
- [x] `textDocument/documentSymbol` -- **done.** `capabilities::document_symbols`
      filters `SymbolTable::iter()` to one file's declaration-shaped
      symbols (everything except `Parameter`/`LocalVar`/`CatchVar`/
      `ForEachVar`/`SwitchBindingVar` -- an outline has no business
      showing method-local variables) and nests them by `Symbol::container`:
      a type's members and any nested types become its `children`,
      top-level types are the roots. `range` is `Symbol::ptr`'s whole-
      declaration span, `selection_range` is `Symbol::name_range` -- both
      already exactly what `DocumentSymbol` wants, no new binder data.
- [x] `workspace/symbol` -- **done**, but not from `SymbolTable::by_name_ci`
      as originally guessed: that's an *exact* case-insensitive lookup,
      not the substring match a real "type a few letters" workspace-
      symbol UX needs, so `capabilities::workspace_symbols` does a plain
      case-insensitive substring scan over every declaration-shaped
      symbol project-wide instead (same kind-filter as `documentSymbol`).
      Simplest correct baseline -- no fuzzy ranking.
- [x] `textDocument/foldingRange`, `textDocument/selectionRange` --
      **done**, directly off the CST as expected, no semantic info
      needed. `foldingRange` folds every multi-line brace-delimited
      region (`ClassBody`/`InterfaceBody`/`Block`/`TriggerBlock`/collection
      initializers), line-granular only (no `start_character`/`end_character`,
      matching how most editors fold anyway). `selectionRange` walks a
      position's token up through every ancestor node, innermost first,
      chained via `parent` -- rowan's tree already *is* that nesting;
      consecutive levels with an identical range (a single-child wrapper
      node) collapse into one so "expand selection" never appears to do
      nothing.

**Needs new binder-side work first:**
- [x] `textDocument/references` / `textDocument/documentHighlight` --
      **done.** Built the reverse index this item originally called for --
      deliberately *not* a query-time scan over `all_resolutions()`
      (which would've worked with zero binder changes, but pays an
      O(references) cost on every request instead of O(1) per file; this
      project's standing priority is that performance matters everywhere,
      not just the parser/lexer, so the index was built up front rather
      than deferred).
      `ReferenceTable` (`reference_table.rs`) now carries a
      `by_symbol: FxHashMap<SymbolId, Vec<SyntaxPtr>>` alongside
      `resolutions`, maintained incrementally by `set`/`map_ids_into` (the
      same call sites every reference-recording path -- `resolve.rs`'s
      body walk *and* `soql.rs`'s SOQL field-name walk -- already funnels
      through), not rebuilt separately: `Resolution::symbol_ids()` is a
      new helper enumerating the `SymbolId`(s) a resolution touches
      (`Resolved`'s one id, `Candidates`' whole set), and both `set`/
      `map_ids_into` push the reference's `SyntaxPtr` onto each touched
      id's entry in the same pass they already do their existing work --
      no second scan, no intermediate allocation. Safe to build this way
      because a `ReferenceTable` is always populated exactly once, from
      empty, per file rebind (`FileBodies` is replaced wholesale on
      rebind, never patched -- see its doc comment), so there's no
      persistent-across-rebuilds mutation or stale-entry cleanup to
      reason about. `BoundProgram::references_to`/`references_to_in_file`
      expose it (project-wide and per-file respectively -- the latter is
      what `documentHighlight` uses, since it never needs to leave the
      current file). `capabilities::references`/`document_highlights`
      gather target `SymbolId`(s) the same way `hover` already does
      (`symbol_at` first for a declaration, else `resolution_at`'s
      `Resolved`/`Candidates` for a reference -- `Candidates` reaches
      every candidate, same "don't silently pick one" convention
      hover/definition already use).
      **Measured cost, honestly:** this roughly doubles per-reference
      storage (each `SyntaxPtr` now lives once as a `resolutions` key and
      again as a `by_symbol` value). Against the real NPSP corpus:
      `pass2_merge_bodies`'s own exclusive allocation went from 75.6 MB
      to a consistent 92.5 MB (+~17 MB, +22%, reproducible across
      repeated `hotpath` runs) -- the real, bounded, and expected price
      of the index. Wall-clock impact was *not* distinguishable from this
      machine's already-documented run-to-run noise once measured
      properly: `cargo bench`'s criterion numbers (the tool this project
      trusts for real perf decisions, not single-shot `hotpath` runs)
      came back at 322 ms median cold / 5.1 ms median warm -- both
      squarely within, and the warm number better than, the healthy
      ranges already recorded earlier in this section. The warm
      single-edit path (what actually matters for editor responsiveness)
      is essentially unaffected either way, since only one file's few
      bodies get merged on a warm edit, not the whole project.
      Verified two ways: `crates/apex-binder/tests/references_index.rs`
      (binder-level, LSP-independent -- including the tricky case of an
      ambiguous overload call site correctly reaching *every* candidate's
      `by_symbol` entry, not just one) and
      `crates/apexls-server/tests/references_highlight.rs` (full
      protocol-level, spawns the real binary, proves the project-wide
      index actually crosses file boundaries and that
      `includeDeclaration` behaves correctly).

      **Follow-up, closed:** the `by_symbol` index above only ever keys
      by `SymbolId`, so a reference to a real Salesforce field
      (`SchemaObject`/`UnknownSchema`) or a real stdlib class/method/
      enum value (`StdlibMember`) had nowhere to be indexed at all --
      `references`/`documentHighlight` on `String.isBlank(...)` or a
      SOQL `FROM My_Object__c` silently reported zero results, no matter
      how many other call sites existed. Fixed with a second, parallel
      reverse index, `ReferenceTable::by_external`, keyed by a new
      `ExternalKey` (`reference_table.rs`) built from `Resolution::external_key`
      -- `SchemaObject`/`UnknownSchema` share one `ExternalKey::Schema`
      shape (the same real field can resolve as either depending only on
      whether `apex-metadata` has local schema for it, and a lookup
      should find both together), `StdlibMember` gets `ExternalKey::Stdlib`.
      Maintained by the same `set`/`map_ids_into` call sites as
      `by_symbol`, queried via new `BoundProgram::references_to_external`/
      `references_to_external_in_file`. Unlike `by_symbol`'s case-preserving
      `CiKey`, `ExternalKey`'s string components are plain lowercased
      `SmolStr`s -- this index is built/queried at human-interaction
      rates (once per reference at bind time, once per LSP request), not
      the per-reference-during-every-resolution hot path `CiKey` exists
      for, so there's no case to make for that extra complexity here.
      Verified via `crates/apexls-server/tests/references_highlight.rs`
      (a stdlib method call site across two files, and a SOQL object
      reference across two files plus its `.object-meta.xml` declaration
      via `includeDeclaration`).

      **Second follow-up, also closed:** `ExternalKey::Stdlib` originally
      keyed only by `(class_name, member)`, so `references` on
      `System.debug(message)` (the one-arg overload) also pulled in every
      `System.debug(level, message)` (the two-arg overload) call site --
      the opposite mistake from the gap above, over-*widening* rather
      than missing entirely, but still wrong: a project-local overloaded
      method never conflates its overloads this way, since each has its
      own `SymbolId`. `StdlibMemberRef::arg_count` (already carried on
      every `Resolution::StdlibMember`, originally added only to narrow
      *hover* text to the matching-arity overload) is now also part of
      the key, so each overload's call sites stay in their own group --
      the same arity-first best effort `describe_stdlib_member`'s hover
      narrowing already uses, not a new heuristic invented here. Doesn't
      split a same-arity-different-parameter-type overload set (a rarer
      shape in the scraped data), an honest, documented residual gap
      matching hover's own.
- [x] `textDocument/rename` (+ `prepareRename`) -- **done, deliberately
      scoped conservatively.** Two pieces landed together, in this order:
      1. **Overload-narrowing precision, first** (the actual blocker this
         item used to cite): `narrow_by_overload`'s elimination
         (`crate::resolve::is_argument_type_compatible`) only ever
         compared project-local-vs-project-local types before -- any
         system-typed argument or parameter (`String`, `Integer`,
         `List<T>`, a literal, ...) always took the "can't prove wrong"
         path, which is why most real-world overload calls stayed
         `Candidates`. New module `crate::conversions` hand-encodes
         Apex's actual implicit-conversion rules for a small, curated,
         *stable* type set (numeric widening, `Object`, `List`/`Set`/
         `Map`) -- deliberately not the full stdlib method-surface model
         this section's remaining stdlib bullet still tracks, since
         *conversion rules* don't change as Salesforce ships new APIs the
         way a method surface does. Verified against a real connected org
         (`sf apex run` anonymous-Apex probes) before trusting any rule,
         per this project's established oracle practice -- two real
         surprises the verification caught, opposite of the initial
         guess: `List<Object>`/`Map<_, Object>` accept *any* element
         type the same way a bare `Object` parameter does, and that
         permissiveness isn't Object-only -- numeric widening and
         project-local `extends` upcasting both apply *nested* one level
         inside a collection too (`List<Integer>` satisfies a
         `List<Long>`-only overload, `List<Dog>` satisfies a
         `List<Animal>`-only one), i.e. Apex's collection generics aren't
         invariant the way Java's are. `narrow_by_overload` also gained a
         most-specific tiebreak (`crate::conversions::is_more_specific`)
         for when more than one candidate survives elimination (e.g.
         `foo(Integer)` beating `foo(Object)` for an `Integer` argument)
         -- elimination alone isn't enough even for the simplest case,
         since a wider overload is never positively *incompatible*, just
         less specific. `cargo bench -p apex-binder` showed no cold/warm
         regression (`corpus/bind_npsp_full`/`corpus/warm_rebind_after_one_file_edit`
         both within this machine's existing noise band). New tests:
         `crates/apex-binder/src/conversions.rs`'s own unit tests, and
         `crates/apex-binder/tests/overload_narrowing_conversions.rs`
         (real overload calls through `narrow_by_overload`, including a
         confirmation that anything outside the curated set stays
         honestly `Candidates` rather than a guessed elimination).

         **Follow-up, also closed:** the original curated set's gaps
         weren't just theoretical -- `Id`, `Date`/`Datetime`/`Time`,
         `Blob`, and any Salesforce object type (`Account`, custom
         objects, ...) were all real, common types that stayed
         permanently `Candidates` against each other with no way to
         narrow. Widened `is_curated`/`system_type_compatible`/
         `is_more_specific` to cover all of them, each rule verified
         against a real org first, per this project's established
         practice, before trusting it:
         `Id`/`String` are bidirectionally compatible, but -- a genuine
         surprise the verification caught -- a real org resolves *every*
         `describe(Id)`/`describe(String)` overload call to the `String`
         overload, even one whose argument's own declared type is `Id`
         (normally the exact-match winner). Rather than guess whether
         that generalizes, no specificity order was added between them
         at all, so a real project with both overloads stays honestly
         `Candidates` instead of risking a confidently wrong `Resolved`.
         `Date` widens to `Datetime` one-directionally (exact match still
         wins when both apply, confirmed the same way numeric widening
         already was); `Time` has no relationship with either; `Blob` has
         none with `String`. `SObject` needed a real, not heuristic,
         notion of "is this actually a Salesforce object" -- the universe
         of real object names is dynamic, unlike every other curated
         type here -- so `crate::conversions` now also takes a
         `&SchemaIndex` (threaded through `narrow_by_overload`/
         `is_argument_type_compatible`/`most_specific_candidate`/
         `dominates`/`narrow_stdlib_overload_type`/`stdlib_args_compatible`,
         all three real call sites in `resolve.rs` already had `self.schema`
         in scope): a real object type upcasts to `SObject` (never the
         reverse), and two *different* concrete object types are never
         mutually compatible either -- unlike a project-local `extends`
         chain, Apex has no SObject-to-SObject subtyping at all. The
         nested-collection case (`List<Account>` satisfying a
         `List<SObject>`-only parameter) needed no new code at all --
         `collection_args_compatible` already recurses through
         `type_compatible` for exactly this reason. Confirmed on the real
         NPSP corpus: `resolution_regression_baseline.rs`'s `Resolved`
         count rose by 318 (`204,987` -> `205,305`), `Unresolved` dropped
         by 5 more (`83,959` -> `83,954`, a stdlib call's own overloaded
         return type narrowing to one instead of none let a further
         chained reference resolve too). New tests:
         `conversions.rs`'s own unit tests (one per newly-curated type
         pair) and four new `overload_narrowing_conversions.rs` cases
         (`Id` vs `Blob` now disambiguating, `Date`/`Datetime` both
         directions, `Account` vs `SObject`); the original `Id`-vs-`Blob`
         "stays ambiguous" case was repointed at `Exception`/`PageReference`,
         a pairing still genuinely outside the curated set.
      2. **Rename itself**, built entirely on already-shipped, already-
         tested infrastructure (`symbol_at`/`resolution_at`,
         `references_to`, `ReferenceTable::highlight_range`, the
         `RwLock<Option<BoundProgram>>` snapshot model, the single
         sequential rebuild worker) -- no new binder passes or indices.
         `capabilities::rename_target`/`rename_edits`
         (`crates/apexls-server/src/capabilities.rs`) refuse outright
         (a `ResponseError`, never a silent empty/partial edit) rather
         than guess whenever: the target or any of its references
         resolves as `Resolution::Candidates`/`Unresolved`/schema-only;
         it's a `Trigger`; it's a `Method` that's itself `override`,
         implements an interface/base-class method of the same name and
         arity, or is itself overridden by a subclass (a *distinct* risk
         from overload ambiguity -- renaming a virtual method needs to
         cascade across a whole override chain, a feature this doesn't
         build); the new name isn't a legal, non-keyword Apex identifier
         (checked by actually tokenizing it with `apex_lexer::tokenize`,
         not a hand-rolled second keyword list); or the new name collides
         with an existing top-level type or same-container member
         (`SymbolTable::top_level`/`lookup_member`, both already O(1)).
         Honest v1 gap: a `Parameter`/local-variable rename isn't checked
         for a scope-shadowing collision yet (every other kind is).
         Renaming a `Class` also renames its own `Constructor` symbol(s)
         (a separate declaration from the class, sharing its name, never
         itself recorded as a *reference* to the class). `WorkspaceEdit`
         uses the plain `changes` map (not `document_changes`, which
         needs per-document version numbers this server doesn't track).
         Verified two ways: `crates/apexls-server/tests/rename.rs`
         (protocol-level, spawns the real binary -- cross-file edits, the
         class/constructor case, every refusal case) including a real-
         NPSP-corpus case (a real, non-virtual, cross-file-referenced
         method, following this project's own convention of running
         whole-corpus checks as normal tests, not `#[ignore]`d).
      **Real bug found via a user report, fixed the same day:** renaming
      a variable to a *short* name corrupted the file (`Integer x= 0;`
      instead of `Integer x = 0;`) -- root cause was a previously-
      undetected, project-wide range-precision bug, not anything specific
      to rename. This parser attaches trailing trivia (almost always at
      least one space) as a child *inside* a `DeclName`/`NameExpr`/`Type`/
      `QualifiedName` node itself, rather than as leading trivia of
      whatever token follows -- so `Symbol::name_range` (every kind:
      class/interface/enum/field/property/method/constructor/parameter/
      local/enum-constant/trigger, all set via `name.syntax().text_range()`
      in `collect.rs`/`resolve.rs`) and a plain-identifier/type reference's
      node range were both silently one trivia-token too wide. Invisible
      to every existing consumer (hover/definition/documentSymbol only
      ever cared about the *start* of a range; `documentHighlight`'s own
      tests only checked occurrence *counts*, never the exact end
      character) until a `rename` `TextEdit` made the exact end boundary
      load-bearing -- a range one character too wide silently eats the
      next real character instead of just a harmless extra space.
      Two-part fix: `apex_syntax::ast::Name::ident_range()` (the
      identifier token's own range, not the wrapping node's) for every
      `Symbol::name_range` call site, plus `BoundProgram::highlight_range`
      recomputing a tight range **on demand**, per query, for a
      `NameExpr`/`Type`/`QualifiedName` reference instead of pre-storing
      one during binding. That second part isn't a stylistic choice: the
      first attempt (calling `ReferenceTable::set_with_highlight` from
      `bind_name_expr`/`resolve_type_ref`, mirroring the existing
      `FieldExpr`/`MethodCallExpr`/`CallExpr`/`NewExpr` fix) was measured
      via `cargo bench -p apex-binder` at a real ~20-30% cold/warm
      regression, not just the ~22% *allocation* increase that fix's own
      original rollout accepted for the far rarer call/field kinds --
      `NameExpr` in particular is the single most common reference kind
      in real code (every local/field/param *read*), so eagerly doubling
      its storage was a much bigger cost than the original rollout ever
      paid. Moving the computation to query time (only when a
      documentHighlight/references/rename request actually asks)
      eliminated the regression entirely (confirmed back within this
      benchmark's own established noise band across two consecutive
      re-runs) with no correctness cost. New regression coverage:
      `crates/apex-binder/tests/name_range_precision.rs` (every
      declaration kind's `name_range`, a local's reference `highlight_range`,
      a type reference's, and an end-to-end "apply every returned range as
      a literal splice and check the exact resulting text" proof -- the
      specific check that would have caught this before it shipped), plus
      a direct repro in `crates/apexls-server/tests/rename.rs`
      (`renaming_a_local_variable_to_a_short_name_does_not_corrupt_the_file`)
      and exact post-apply text assertions added to the other rename
      tests that previously only checked edit count/`newText`.
- [x] `textDocument/signatureHelp` -- done: reuses `narrow_by_overload`'s
      candidate set (recomputed unnarrowed from the resolved call's
      `container`/`name`), with argument-position tracking (which
      parameter the cursor is currently in) layered on top via a
      `Comma`-token count, not resolved-argument-node count.
- [x] `textDocument/completion` -- done, scoped to two contexts: member-
      access after a `.`/`?.` (`foo.|`) and bare-identifier/fresh-position
      (`de|`, or nothing typed at all). Deliberately out of v1 scope, the
      same way other entries here note honest gaps: SOQL/SOSL completion
      (`FROM`/`WHERE`), snippet completion, auto-import, override-method
      completion, and `completionItem/resolve` (documentation is never
      fetched per candidate at all -- see below). Filtering by whatever
      prefix is already typed is left to the client's own fuzzy matcher
      (standard LSP architecture) rather than done server-side: every
      request returns the *full* candidate set for the resolved context
      (`isIncomplete: false`) plus a computed replace-range, and the
      client narrows the visible list itself as the user keeps typing.

      **The mid-token parser gap this item used to cite turned out to be
      one specific, narrow bug, not a general recovery problem.** The
      parser already produces a usable tree for genuinely incomplete
      input (`foo.` still parses to a `FieldExpr` node,
      `crates/apex-parser/src/grammar/expressions.rs`'s `expr_primary_chain`
      completes the node either way) -- the actual defect was three AST
      accessors (`FieldExpr::member_token`, `MethodCallExpr::method_name_token`,
      `QualifiedName::last_token`) built on a shared `last_non_trivia_token`
      helper that blindly returns a node's last direct token regardless
      of kind, so a dangling dot with nothing typed after it was
      misreported as a member literally named `"."`. Fixed with a new
      `last_member_name_token` (`crates/apex-syntax/src/ast/mod.rs`,
      mirroring an existing `soql.rs::alias_token` precedent) that
      returns `None` when the trailing token turns out to be the
      `Dot`/`QuestionDot` itself. Verified as a true no-op on every
      existing complete-input test (the whole-corpus `ast_smoke.rs` walk
      asserts these accessors `.is_some()` over real files, which never
      exercises a missing identifier) before adding the new dangling-input
      cases; two new golden fixtures
      (`tests/golden/malformed/04_dangling_field_access.cls`,
      `05_dangling_method_call.cls`) pin the parser's recovered tree shape
      for this input class going forward.

      **The completion engine** (`apex_binder::complete_at`, new
      `crates/apex-binder/src/completion.rs`) needed no new binder-side
      index -- every candidate source is already bounded (scope depth, or
      one type's/one stdlib class's/one SObject's member count), matching
      this project's established "on demand, no precomputed index"
      posture for `signatureHelp`/`inlayHint`/`callHierarchy`:
      - Receiver-type inference for member-access reuses `resolve::bind_expr`
        verbatim via a new `resolve::body_binder_for_completion`
        constructor, which builds a throwaway `BodyBinder` sharing the
        real `SymbolTable`/`SchemaIndex`/`StdlibIndex` and a **clone** of
        the real per-body `ScopeTree` (`BoundProgram::scope_tree`, already
        public, already anticipating exactly this use per its own doc
        comment) but a **fresh, discarded** `ReferenceTable` -- safe
        because `bind_expr` never mutates `scopes` and `BodyBinder` is
        already constructed fresh-per-call everywhere else in this crate,
        so nothing new needed inventing for isolation. This reuses the
        real chain-typing logic (arbitrary depth, `this`/`super`, stdlib/
        schema fallthrough) with zero duplication.
      - Locating *which* body/enclosing-type/enclosing-member a cursor
        position belongs to is a single unified ancestor climb matching
        declaration nodes structurally (by `Symbol::ptr` equality) rather
        than duplicating `bind_symbol_body`'s per-kind dispatch --
        correct at any nesting depth (a nested class's method, a
        trigger's top-level body, a bare field initializer with no
        enclosing `Block` at all) without special-casing each one.
      - Member enumeration reuses `SymbolTable::lookup_member` (not a
        second override-shadowing implementation) for project types,
        `SObjectSchema::fields`/`StdlibClass::methods`/`::properties` for
        schema/stdlib types (already public, directly enumerable, no new
        `SchemaIndex`/`StdlibIndex` API needed). Deliberately includes
        `Method`/`Constructor` candidates even though bare-name
        *resolution* excludes them -- completion should still suggest
        `getName()`. A local/param whose own declaration is textually
        *after* the cursor is filtered out (`Scope::bindings` has no
        position-ordering built in, since its one prior consumer,
        `resolve_local`, is only ever asked about an already-valid
        reference) so a not-yet-typed `Integer later = 0;` further down
        the same block isn't offered as already in scope.
      - `CompletionCandidate` deliberately carries no documentation field
        at all -- fetching a doc comment is a tree walk per candidate, per
        keystroke, expensive even done eagerly unlike everything else
        here; a field/method's hover already covers it once a candidate
        is actually inserted.

      **`apexls-server`** (`capabilities::completion`, `lib.rs`'s
      `completion` handler, `completionProvider` with `.` as the only
      trigger character) mirrors `signature_help`'s exact on-demand
      wiring shape. `CompletionItem::detail` reuses existing formatters
      end-to-end rather than a second formatter in the protocol-agnostic
      binder crate -- `describe_symbol` was split into a new
      `symbol_signature` (the plain-text line, no Markdown fence, no doc)
      that both hover and completion now call, a pure refactor confirmed
      behavior-preserving against the existing hover test suite before
      building on it. `sort_text` uses one leading tier digit (locals >
      direct-or-project-wide-types > inherited > stdlib > SObject fields
      > keywords) -- project-wide types collapse into the same tier as
      direct members rather than their own, since `CompletionCandidateKind`
      has no way to tell a nested-class member apart from a top-level
      type scan result and both are equally "directly relevant" for a
      bare identifier anyway.

      Verified three ways: `crates/apex-binder/tests/completion.rs`
      (binder-level -- nested-scope locals with correct shadowing and
      not-yet-declared exclusion, direct vs. inherited members with
      override-shadow correctness, member-access off a project/stdlib/
      SObject-typed receiver, visibility filtering, and the dangling-dot-
      vs-partial-identifier replace-range distinction);
      `crates/apexls-server/tests/completion.rs` (protocol-level, spawns
      the real binary -- capability advertisement, an end-to-end dangling-
      dot request, and a bare-identifier request); and `cargo bench -p
      apex-binder`, confirming no cold/warm regression (both initial
      readings' apparent changes disappeared on repeated runs, matching
      this project's own already-documented noise pattern on this exact
      benchmark).
- [ ] `textDocument/semanticTokens` -- needs a token-classification pass
      over the resolved AST (e.g. distinguishing a field access from a
      local, a resolved type name from an unresolved one).
- [x] `textDocument/callHierarchy` (prepare + incoming/outgoing) -- done
      without needing a precomputed whole-project call graph at all: LSP's
      own call-hierarchy protocol is inherently lazy/expand-on-demand
      (prepare returns an item, the client asks `incomingCalls`/
      `outgoingCalls` per node it drills into), so each request is
      answered on demand from primitives `references`/`rename` already
      needed. `apex_binder::call_hierarchy` (`incoming_calls`,
      `outgoing_calls`, `is_callable`) is the pure analysis layer:
      `incoming_calls` groups `BoundProgram::references_to`'s project-wide
      hits by each site's enclosing callable (a new
      `BoundProgram::enclosing_callable` primitive -- walks a reference's
      ancestors to the nearest `MethodDecl`/`ConstructorDecl`, then
      re-resolves that declaration's own name via the existing
      `symbol_at`); `outgoing_calls` scans a callable's own declaration
      range for call-shaped references (`MethodCallExpr`/`CallExpr`/
      `NewExpr` -- confirmed exhaustive by the same node-kind audit
      `resolution_at`'s doc comment already did) via a new
      `BoundProgram::call_sites_in_range`. `apexls-server::capabilities`
      layers `CallHierarchyItem`/`CallHierarchyIncomingCall`/
      `CallHierarchyOutgoingCall` construction on top, re-resolving each
      request's `CallHierarchyItem` from its own `uri`/`selection_range`
      against whatever bind is live *now* rather than round-tripping a
      `SymbolId` through the `data` field -- the same "always resolve
      against the current program" posture every other capability here
      already takes. Constructors are callable targets too (`new Foo()`,
      `this(...)`/`super(...)`), not just methods.

      An ambiguous overload call (`Resolution::Candidates`) fans out to
      *every* candidate rather than being silently dropped or guessed --
      the same "don't guess, but don't hide it either" convention
      `references`/`documentHighlight` already use for an ambiguous
      target, not rename's stricter refuse-outright. This is the honest,
      unavoidable consequence of not having a full type system: an
      argument whose type comes from an unmodeled stdlib call, a system
      type outside `crate::conversions`'s curated set, or a genuine tie
      between equally-specific overloads all still resolve to
      `Candidates`, not a single guessed answer -- see
      `crate::resolve::narrow_by_overload`'s own doc comment. One honest
      v1 gap: a call site with no enclosing method/constructor at all (a
      field/property initializer, which Apex does allow to contain a
      call) is silently excluded from `incoming_calls`, since there's no
      `CallHierarchyItem` to report it from.

      Tests: `apex_binder::call_hierarchy`'s own module (fast, in-process,
      including the cross-file/field-initializer-exclusion case for
      incoming calls, a constructor call for outgoing calls, the
      ambiguous-overload fan-out, and a real-NPSP-corpus sweep proving
      neither query panics across every callable in a project that size)
      and `crates/apexls-server/tests/call_hierarchy.rs` (protocol-level,
      the full prepare-then-incoming/outgoing round trip against the real
      binary).

      **Known gap, not yet closed:** `outgoing_calls` matches a call
      site's `Resolution` against `Resolved`/`Candidates` only --
      `_ => continue` silently drops a call to a real stdlib method
      (`String.isBlank(...)`) or a schema-backed reference, the exact
      same `SymbolId`-only blind spot `references`/`documentHighlight`
      used to have before `ExternalKey`/`by_external` (see this section's
      `textDocument/references` item above) fixed it for those two.
      Closing this one is a strictly bigger lift than that fix was,
      though, not a copy of it: `OutgoingCall`/`CallHierarchyItem` are
      built around a `SymbolId` end-to-end (a real declaration to build a
      `selection_range`/`uri` from), and a stdlib method has no location
      to build one from at all -- `prepareCallHierarchy` couldn't
      construct an item to *root* a hierarchy at a stdlib method either,
      even before `outgoingCalls`/`incomingCalls` get involved.
      `incoming_calls` doesn't have a symmetric version of this gap:
      nothing project-local ever *calls into* something external in a
      way this analysis could observe (`by_external`'s consumer is
      always the reference itself, not a caller of it).
- [x] `textDocument/inlayHint` -- done for the standard cross-language
      default (a `paramName:` label before each call argument, via
      `BoundProgram::call_sites_in_range`, the same on-demand primitive
      `outgoingCalls` uses), not the "inferred local types" idea
      originally sketched here -- Apex has no `var`/implicit-typed local
      at all, so there's no real analogue of that specific rust-analyzer-
      style hint to build. Only emitted for an unambiguous call -- a
      project call resolved to exactly one `Resolution::Resolved`
      `Method`/`Constructor`, or a stdlib call whose overload set
      narrows to exactly one candidate by arity (an inlay hint is baked
      into the editor's rendering of the line, so a wrong guess would be
      far more visible/misleading than a hover's honest "+N more").
      Stdlib calls get real parameter-name hints too, not just types:
      `apex_stdlib::StdlibMethod::params` was originally scraped as
      type-only (`RawParam` had no `name` field), but the scraper's own
      output already carries a `name` per parameter -- that field was
      just never read. Fixed by adding `StdlibParam{name, type_name}` in
      place of the old `Vec<Option<SmolStr>>`.

**Needs work outside the binder entirely:**
- [ ] `textDocument/formatting` / `rangeFormatting` -- `apex-printer`
      only does verbatim round-trip rendering today, not reformatting.
      A real formatter is a separate, substantial component.
- [x] `textDocument/codeAction` / `textDocument/publishDiagnostics` --
      dead-code detection (`private`/`public` methods/fields/properties/
      constructors, plus plain local variables) paired with a "Remove
      unused ..." quick-fix, not the general diagnostics/codeAction
      plumbing this section used to describe as blocked on each other.
      Semantic diagnostics in general (unresolved symbols, type errors)
      are still **not** safe to ship -- see §4, first bullet -- but
      dead-code detection sidesteps that gap entirely: it's pure
      reference-counting (`BoundProgram::references_to_in_file`/
      `references_to`), never dependent on the unmodeled stdlib/schema
      surface. The analysis itself now lives in `apex_binder::dead_code`
      (`dead_symbols_in_file`, `kind_label`), promoted out of
      `apexls-server` so both the LSP diagnostics and the new `apexls
      dead` batch CLI subcommand share one implementation --
      `apexls-server::capabilities::dead_code_diagnostics`/
      `dead_code_actions` are now thin LSP-coordinate wrappers over it.

      Started `private`-only, widened to `public` in a second pass once
      each platform-reflection channel turned out narrower than "exclude
      all of public": `@InvocableMethod`/`@InvocableVariable` (Flow),
      `@AuraEnabled` (Aura and LWC share the one annotation),
      `@RemoteAction`, and the `@Http*`/`@RestResource` family are all
      annotation-gated and checkable (`has_platform_invocation_annotation`)
      -- a `public` member carrying one is exempted. Visualforce is the
      one channel with no annotation gate at all: a `.page`'s
      `controller`/`extensions` attributes can call any `public` member
      with zero textual Apex call sites, so `apex_metadata::visualforce`
      extracts which classes a project's `.page` files actually name
      (deliberately not a full XML parse -- real VF pages routinely
      aren't well-formed XML -- just the isolated `<apex:page>` root tag,
      parsed with `roxmltree`), and `BoundProgram::vf_referenced_classes`
      exempts a VF-referenced class's public members wholesale (`is_visualforce_referenced`)
      rather than trying to prove which specific member a page's embedded
      `{!expr}` markup calls. `apex_discover::Discovery` gained
      `page_files` to support this (a single-walk addition, not a second
      pass over the tree). Reference counting for a `public` candidate
      switches to the project-wide `BoundProgram::references_to`, not the
      file-scoped `references_to_in_file` a `private`/`LocalVar`
      candidate correctly uses (Apex's own visibility rules make `private`/
      local genuinely file-scoped; `public` can be referenced from any
      file). `Constructor` was originally excluded from candidacy
      entirely on the theory that a private zero-arg constructor's whole
      purpose is having zero call sites (the "block external
      instantiation" idiom) -- corrected: a constructor is just as
      capable of being genuinely, forgettably dead as any other member,
      so it's now a full candidate under the same visibility-scoped rule
      as `Method`/`Field`/`Property` (a VF-referenced class's constructor
      is still covered by the same whole-class VF exemption, since VF's
      page-rendering engine always calls a controller/extension's
      constructor implicitly). The one real gap widening to `public`
      *doesn't* close on its own: `@isTest`/`@TestSetup`/legacy
      `testMethod` methods are routinely `private` and are still invoked
      directly by the platform's test runner --
      `is_platform_invoked_test_method` exempts them explicitly.
      `ForEachVar`/`CatchVar`/`SwitchBindingVar` stay excluded --
      removing one would break the surrounding loop/catch/switch syntax,
      so there's no safe quick-fix to pair a diagnostic with.
      `Protected`/`Global` stay excluded too: `Global` is external
      managed-package API surface, unprovable by local analysis
      regardless; `Protected` is a reasonable, cheap future extension
      (same project-wide reference-counting `Public` already needs, and
      VF/annotation exposure doesn't apply to it) not built yet.

      Diagnostics are pushed proactively after every rebuild (for
      currently-open documents only), not computed lazily behind a
      request the way every other capability is -- new `ClientSocket`
      threading into `spawn_rebuild_worker` was the one piece of
      genuinely new wiring this needed. Tests:
      `crates/apex-binder/src/dead_code.rs`'s own test module (fast,
      in-process, including exact-splice deletion-range checks, VF/
      annotation exemption cases, and a real-NPSP-corpus sweep asserting
      the detector stays conservative in aggregate), `crates/apex-metadata/src/visualforce.rs`'s
      own tests (controller/extensions extraction, including a
      deliberately-malformed page body), and
      `crates/apexls-server/tests/dead_code_diagnostics.rs` (protocol-
      level, the diagnostic-then-codeAction round trip against the real
      binary). Also fixed, as a prerequisite: every existing protocol
      test's `recv()` assumed the next stdout frame was always the
      response it was waiting for -- a proactive `publishDiagnostics`
      notification landing in between broke that assumption across
      every test file that opens a document with any dead code in it;
      `recv()` everywhere now skips past notification frames while
      waiting for a specific response.

      **Follow-up, closed: real syntax-error diagnostics (rust-analyzer-
      style red squiggles), not just the dead-code warning above.** The
      parser's own "never panics, always records a `ParseError` plus a
      best-effort tree" guarantee (`apex_parser::errors`'s module doc
      comment) meant the data already existed; it had just never been
      surfaced to an editor. `BoundProgram::syntax_errors(file)` (new,
      `crates/apex-binder/src/lib.rs`, right next to `syntax()`) exposes
      each file's `Vec<ParseError>`; `capabilities::syntax_error_diagnostics`
      turns each into an `ERROR`-severity `Diagnostic` -- range is
      deliberately just the one byte at `ParseError::offset` (clamped to
      the file's length for an end-of-file error), not extended to the
      nearest token's full span, and the message is the parser's own
      `"expected X, found Y"` text passed through verbatim, both honest v1
      scope lines rather than oversights (see that function's own doc
      comment for why). The one real design point: `textDocument/publishDiagnostics`
      *replaces* a client's whole diagnostic set for a URI on every
      notification rather than merging with the previous one, so this
      couldn't be a second, independent publish call alongside the
      existing dead-code one -- `publish_dead_code_diagnostics` was
      renamed to `publish_diagnostics` and now merges both sources into
      one notification per file, reusing the exact same "proactive after
      every rebuild, unconditional even when empty so a fixed error
      clears its own stale squiggle" mechanism unchanged. Verified via
      `crates/apexls-server/tests/syntax_error_diagnostics.rs`: a real
      syntax error gets an `ERROR` diagnostic with the parser's message, a
      `didChange` that fixes it clears that diagnostic on the next
      publish, and a file with both a syntax error *and* a genuinely dead
      symbol gets both in the same notification -- pinning down the merge
      specifically, since silently clobbering one diagnostic source with
      the other is exactly the mistake the design point above call out.

      **Second follow-up, also closed, from a real user report against the
      feature above: "missing X" diagnostics landed on the wrong line.**
      `Parser::expect` (`crates/apex-parser/src/parser.rs`) recorded a
      failed expectation (a missing `;`/`)`/`}`/`]`/...) at the start of
      whatever real token happened to follow the gap -- for a missing
      semicolon immediately followed by another statement on the next
      line, that put the squiggle on the *next* statement, reading as "this
      is wrong" when it wasn't; the actual problem (no semicolon) sits at
      the end of the *previous* line. Fixed by giving `expect`'s failure
      path its own `error_at_gap`, positioned at the byte right after the
      last significant token actually consumed (`Parser::prev_token_end`,
      skipping any trivia in the gap -- a comment between the two
      shouldn't push the diagnostic past it) -- matching rustc/rust-
      analyzer's own convention for a missing-token diagnostic. `error()`'s
      existing behavior (current-token positioning) is unchanged and still
      used by every other `p.error(...)` call site in `grammar/` -- ~35 of
      them describe more semantically-specific "expected a type"/"expected
      a member name"/... shapes, plus one genuine "unexpected token" case
      (`bump_if_no_progress`) that *should* keep pointing at the bad token,
      not a gap before it; migrating any of those to `error_at_gap` is a
      real, deliberately deferred follow-up, not attempted here since
      `expect` alone already covers the overwhelming majority of what a
      user hits while typing. Blast radius checked, not guessed: grepped
      the whole workspace for hardcoded `ParseError`/offset expectations
      outside `apex-parser`'s own tests -- none exist; only three of the
      five `tests/golden/malformed/*.cls` snapshots actually shifted
      (each manually reviewed, each moving to the correct gap position),
      and `cargo bench -p apex-binder` confirmed no regression (the
      changed code only runs on the already-cold error path, never during
      a successful parse). New test:
      `crates/apexls-server/tests/syntax_error_diagnostics.rs`'s
      `a_missing_semicolon_is_reported_at_the_end_of_the_statement_missing_it_not_the_next_one`,
      built directly from the reported example's shape.
- [x] **Unresolved-reference diagnostics, from `Resolution::Unresolved` --
      shipped, but not the way this backlog originally framed it below
      ("likely the single highest-value addition... cheapest to add").** A
      pre-implementation investigation into every `refs.set(..., Resolution::Unresolved)`
      call site in `resolve.rs`/`soql.rs` found several **structural,
      confirmed, high-volume false-positive sources**, not just typos:
      catch-clause/`whenValue`/`upsert`-external-id lookups
      (`SymbolTable::resolve_dotted_name`, project-local only, no stdlib
      fallback), `super`/`super(...)`/`this(...)` on a type whose own
      supertype is unresolvable (`inherit::resolve_inheritance`'s own doc
      comment: "an unresolvable supertype... is simply dropped" -- true for
      *any* stdlib base, `Exception` included, confirmed empirically while
      writing this feature's own tests), one segment of a namespace-
      qualified type reference (`resolve::record_qualified_segments`,
      always `Unresolved` per-segment even when the whole reference
      resolves fine -- and often a recognized keyword token like
      `SyntaxKind::System`, not a plain identifier, also confirmed
      empirically), and a SOQL `TYPEOF ... ELSE` field (unconditional by
      `soql.rs`'s own design). `resolution_regression_baseline.rs` (pins
      `Unresolved`'s count at `28,172` on the real NPSP corpus) already
      said as much: it explicitly doesn't track what fraction is "a real
      bug" vs. "a known, still-unmodeled gap," and every historical
      reduction in that count turned out to be the latter. Given that, the
      shipped design doesn't filter down to only the safe cases -- it shows
      **every** `Unresolved` reference (full visibility, not hidden
      precision, was the explicit goal), and grades confidence via
      severity instead: `capabilities::classify_unresolved` recognizes the
      specific structural shapes above and reports those `WARNING` with a
      message explaining why (doubling as a live, in-editor discovery feed
      for closing those exact binder gaps later, the same purpose
      `examples/unresolved_clusters.rs` already serves offline); everything
      else is `ERROR` as the higher-confidence default -- though, per
      `classify_unresolved`'s own doc comment, that's a best-effort
      heuristic, not a proof, and this feature's own test suite caught a
      real example of the limitation: `extends Exception`'s own `Type`-kind
      reference lands in the `ERROR` bucket too, because `Exception` itself
      turned out to be unmodeled in this binder's stdlib class index --
      likely a real, separate, still-open gap, not a diagnostic bug. New:
      `BoundProgram::resolutions_in_file` (`crates/apex-binder/src/lib.rs`,
      mirrors `all_resolutions` but per-file), `capabilities::classify_unresolved`/
      `unresolved_reference_diagnostics`, wired into the existing merged
      `publish_diagnostics`. Tests:
      `crates/apexls-server/tests/unresolved_reference_diagnostics.rs`.
- [ ] **Duplicate/conflicting-modifier diagnostic -- a real gap, found via
      a user report.** `private private private private void foo() {`
      produces no error anywhere in the pipeline today: `grammar::declarations::modifiers`
      parses `modifier*` as a plain repetition with no uniqueness
      constraint (matching the ANTLR reference grammar -- this is normal,
      expected *syntax*-level behavior, not a parser bug), and
      `apex-binder`'s `ModifierSet::from_modifiers`
      (`crates/apex-binder/src/symbol.rs:123-148`) just idempotently
      re-assigns the same flag per repeated token, so five `private`s
      produce the identical `ModifierSet` one would. A real Salesforce org
      almost certainly rejects this at save/deploy time -- worth
      confirming the *exact* shape (a syntax error in the real compiler,
      or a semantic one, and whether it's specifically duplicates or any
      conflicting-visibility combination like `private public`) against a
      real org via the `sf` CLI oracle this project already uses for
      disputed grammar/semantics questions, before deciding whether this
      becomes a third `textDocument/publishDiagnostics` source alongside
      syntax errors and dead code, or is better modeled as a parse-time
      restriction instead.
- [ ] **Further diagnostic sources, roughly ranked by value vs. how much
      new analysis each needs.** All would join `syntax_error_diagnostics`/
      `dead_code_diagnostics` in the same merged `publish_diagnostics`
      notification (`crates/apexls-server/src/lib.rs`) -- see that
      function's own doc comment for why they must be merged, not
      published as separate notifications per file.

      **Already computed, just never surfaced -- cheapest to add:**
      - Unknown SOQL/SOSL object or field, from `Resolution::UnknownSchema`
        -- a query referencing an object/field with no matching local or
        standard schema (`FROM Unknown_Object__c`, a bad `WHERE` field).
        Same story: already computed during binding, never surfaced.

      **Needs new, well-scoped binder analysis:**
      - DML or SOQL inside a loop -- the classic Apex governor-limit
        bulkification anti-pattern. Extremely common in real Apex code,
        well-defined as an AST pattern (a DML statement or SOQL query
        whose ancestor is a `for`/`while`/`do` loop body), and Salesforce-
        specific in exactly the way this project's own value proposition
        already is.
      - Unreachable code after an unconditional `return`/`throw`.
      - Missing interface/abstract-method implementation -- needs real
        "does this concrete class implement everything it must" checking,
        not built yet.

      **Riskier, more speculative -- not just "not built yet," genuinely
      needs more thought before committing to it:**
      - `Resolution::Candidates` (an unresolved-overload ambiguity) as an
        error diagnostic -- tempting, but `Candidates` means "this binder
        can't narrow the call further," not necessarily "a real compiler
        would find this ambiguous too" (the whole reason `crate::conversions`
        exists is to keep narrowing that gap without guessing). Surfacing
        it as a red squiggle risks real false positives against this
        project's own established "don't guess" discipline -- would need
        verification against a real org first, the same way every
        `conversions.rs` rule already is, not just an assumption that
        "ambiguous to us" means "ambiguous to the compiler."
      - Salesforce security/best-practice lints (hardcoded record IDs,
        SOQL injection risk in dynamic queries built from unescaped user
        input, a class missing `with sharing`) -- real value, but a
        different *kind* of feature than "the compiler found a bug" (style/
        security-review territory, closer to what Salesforce Code
        Analyzer/PMD already do), not attempted alongside the correctness-
        oriented diagnostics above.

## 4. Correctness gaps that block features, not just refine them

These were flagged as deliberate, documented v1 scope cuts while
building `apex-binder`, but they're not just "nice to have more
precision" -- they directly block shipping certain features honestly.

- [x] **No standard-library type model -- done, via the same bundled-
      snapshot approach as the SObject/field gap below.** `apex_stdlib`
      additionally embeds a class/method/property snapshot scraped from
      Salesforce's own Apex Reference Guide (673 real classes/
      interfaces, 4,638 methods, 919 properties), indexed by
      `crates/apex-binder/src/stdlib_index.rs::StdlibIndex` and consulted
      from `crate::resolve`'s `Ty::System` arms. A new `Resolution`
      variant, `StdlibMember(Box<StdlibMemberRef>)` -- no `SymbolId`
      needed after all, it turned out: it slots in exactly like
      `SchemaObject` already does for schema (boxed, no real declaration
      to jump to, but a real, known outcome distinct from `Unresolved`)
      -- so `String.isBlank(...)`/`Database.query(...)` now resolve
      `StdlibMember` instead of unconditionally `Unresolved`, with real
      hover text (signature + scraped description) via
      `capabilities::describe_stdlib_member`. `crate::generics`'s
      existing `List`/`Map`/`Set` type-argument substitution is
      untouched and still tried first (the one case needing real
      substitution, which no raw scraped signature alone can do); the
      new lookup is purely the fallback for everything else, including
      filling `List`/`Map`/`Set` gaps `generics.rs` never modeled
      (`sort`, `addAll`, ...). Fixed a real, necessary gap found along
      the way: `bind_name_expr` had no fallback at all for a bare class
      name used as a *static-call receiver* (only a project-local-type
      check and an SObject check) -- without also fixing that, `String`
      itself bound straight to `Unresolved` before ever reaching the new
      `Ty::System` lookup, so no static stdlib call could ever resolve.
      Confirmed on the real NPSP corpus: `resolution_regression_baseline.rs`'s
      `Unresolved` count dropped by 65,080 in total (149,289 -> 84,209,
      most of it from the `bind_name_expr` fix specifically -- static
      stdlib calls are extremely common in real Apex), `Resolved` rose
      by 3 as a side effect (a project-local overloaded call's argument
      type, previously unknown, is now known well enough for the
      existing `narrow_by_overload` to disambiguate it). Enum constant
      access (`LoggingLevel.INFO` and similar) is also modeled now, as a
      static property of the enum's own type -- the scraper's
      `apex_reference::parse_enum_values` fills in the 104 real stdlib
      enums that previously came out with zero properties (an `Enum`
      page has neither `nested2` leaves nor a `Signature` section, so
      neither of the two existing `parse_class_page` branches ever fired
      for one), lowering `Unresolved` by a further 250 (`84,209` ->
      `83,959`). **What this doesn't do:** this closes the *blocking*
      gap for semantic diagnostics without itself building a diagnostics
      pass; "flag unresolved symbol" as a real, shippable feature is
      separate, unstarted follow-on work.
- [x] **No standard SObject/field schema -- done, via a bundled
      snapshot.** New `crates/apex-stdlib` crate embeds a schema
      snapshot scraped directly from Salesforce's own Object Reference
      documentation (`tools/salesforce-doc-scraper`, offline, run once
      per Apex release -- never a runtime network call), converted into
      `apex_metadata::SObjectSchema`/`FieldSchema` values and merged
      into `SchemaIndex` (`schema_index::merge_sobjects`) alongside a
      project's own locally-discovered custom objects/fields, local
      taking precedence on a name collision. No `crate::resolve` code
      changed at all: `bind_field_expr`'s existing `schema.field(...)`/
      `schema.object(...)` branch just started finding real data, so
      `Account.Name` now resolves `Resolution::SchemaObject` instead of
      `UnknownSchema`, and a standard lookup field's `reference_to`
      (also newly captured by the scraper) continues the type chain
      through multi-hop field access the same way a local custom
      lookup's already did. Confirmed on the real NPSP corpus:
      `resolution_regression_baseline.rs`'s `Unresolved` count dropped
      by 3,250 (152,539 -> 149,289) with zero change to `Resolved`
      (`SchemaObject` isn't tallied either way). **What this doesn't
      do:** model standard-library *classes/methods* (`String`,
      `Database`, ...) -- that's the separate item below, now also done.
- [x] **Real type inference beyond one-hop chaining -- done, honestly
      bounded.** A new `Ty` value (`crates/apex-binder/src/ty.rs`,
      walker-internal only, never stored on `BoundProgram`/`Resolution`)
      replaces `Option<SymbolId>` as `BodyBinder::bind_expr`'s return
      type: `Ty::Project(SymbolId)` is exactly what `Option<SymbolId>`
      already meant; `Ty::System { name, args }` names a system/library
      type or schema SObject even without a member model for it, so a
      literal, an operator result, or a generic collection's element type
      no longer silently disappears at the first hop the way it used to.
      Literal kinds (`Integer`/`Long`/`Decimal`/`String`/`Boolean`) get
      their real system type; comparison/logical operators always produce
      `Boolean`; arithmetic/bitwise/shift/assignment operators propagate
      an operand's type. **What this doesn't do:** attempt common-
      supertype inference for a ternary's two branches beyond exact `Ty`
      equality (covering the rest of the standard library's real method
      surface was the separate stdlib-model gap above, now also done).
- [x] **Generics-aware resolution for `List`/`Map`/`Set` -- done.** Apex
      has no user-defined generics at all, so a small, hand-written,
      case-insensitive table (`crates/apex-binder/src/generics.rs`)
      genuinely *is* Apex's whole generics story: `List.get`/`size`/
      `isEmpty`/`contains`, `Map.get`/`size`/`isEmpty`/`containsKey`/
      `containsValue`/`keySet`/`values`, `Set.size`/`isEmpty`/`contains`,
      each substituting the collection's own type argument(s) (captured
      at Pass 1 collection time as a new `Symbol.type_args: Vec<String>`
      field, one level deep -- the only shape Apex generics actually
      have). `List<Account> l; l.get(0).Name` now resolves `Name` when
      `Account` is project-local, even though `l.get(0)` itself has no
      real declaration to resolve to (stays `Resolution::Unresolved`,
      correctly -- only the *type* flows onward, not a fabricated
      resolution). Not exhaustive on its own: `sort`/`addAll`/`retainAll`/
      `clone`/`iterator`/... aren't in this hand-written table -- they
      now fall through to the bundled stdlib lookup (the standard-library
      type model gap above, now also done) instead of staying
      `Unresolved`, since none of them need type-argument substitution
      this table exists for in the first place.
- [x] **Override-matching semantics -- done.** `SymbolTable::lookup_member`
      now tracks which arities an `override` candidate has already
      claimed at a more-derived level of the `extends`/`implements` chain
      and excludes any same-arity candidate at a more-ancestral level,
      instead of reporting both as separate overload candidates. Exact,
      not a heuristic: Apex requires an `override` method's signature
      (arity included) to exactly match what it overrides, a compile
      error otherwise -- the same guarantee `narrow_by_overload` already
      relies on for arity-based narrowing.
- [x] **Cross-file visibility enforcement -- done.** New
      `SymbolTable::is_visible_from(candidate, from)`: `public`/`global`
      are always visible (v1 still has no namespace model); `private`
      requires the same *top-level* declaring type (Apex nested classes
      share their outer class's private access, so this walks `container`
      to the root, not just one level); `protected` additionally allows
      any type that is, extends, or implements the member's declaring
      type. Every `lookup_member` call site in `resolve.rs`, plus the
      constructor-lookup branches, now filters through it, so a private
      member of an unrelated class can no longer surface as a resolution
      candidate from outside its own type.
- [x] **Dynamic-SOQL bind-variable resolution -- done, deliberately bounded
      to the same-method case.** `Database.query`/`countQuery`/
      `getQueryLocator`'s string argument (`crate::resolve::bind_dynamic_soql_binds`)
      is scanned for `:identifier` bind variables, each resolved against
      the call site's own local/parameter scope -- fixes both a real
      dead-code false positive (a variable bound only inside a dynamic-SOQL
      string had no reference recorded for it at all, since string
      *content* was never tokenized into anything the binder walked) and
      goto-definition on the bind itself (a new synthetic sub-token
      `SyntaxPtr` plus a small per-token span index in `ReferenceTable`
      let `resolution_at` answer a click landing inside a string literal's
      text, which the architecture couldn't represent before). Traces a
      variable argument one hop back to its own literal/`+`-concatenation
      source -- a local's last straight-line assignment in the same
      enclosing-block chain as the call site, or a field's own declared
      initializer -- deliberately excludes `queryWithBinds`/
      `countQueryWithBinds`/`getQueryLocatorWithBinds`, whose bind names
      are `Map` keys, not lexically-scoped variables at all.
      **What this doesn't do, on purpose:** any interprocedural case --
      a query string assembled in one method and only reaching
      `Database.query` after being returned to (or built by) another
      method, most commonly the fflib-apex-common `QueryFactory` fluent-
      builder idiom (`newQueryFactory().setCondition('id in :idSet').toSOQL()`,
      where `setCondition`'s argument is stored into a field by one
      method and read back by a different one, possibly in another file
      entirely). Closing that gap is architecturally feasible -- Pass 2
      already builds a project-wide `FxHashMap<FileId, Parse>` before
      dispatching bodies (`crate::BoundProgram::from_files_cached`'s
      `parse_by_file`), it just isn't threaded into `BodyBinder` today --
      but real cross-method tracing (parameter/field substitution across
      a call, plus recognizing a fluent-setter's `return this;` shape to
      track builder state across a chain) is a substantially bigger,
      riskier undertaking than the same-method case, so it was
      deliberately deferred rather than built speculatively. The current
      behavior is a false negative only (a missed reference/goto-target),
      never a wrong one -- safe to leave open until a real need justifies
      the added surface.
**Performance, measured (`cargo bench -p apex-binder`, real NPSP corpus,
~1070 files):** `corpus/bind_npsp_full` (cold): ~440-459ms across two
consecutive clean runs, within this machine's already-established noise
band for this benchmark (384-487ms observed across earlier, unrelated
baselines with no code changes between them -- see §2's own noise-caveat
precedent). `corpus/warm_rebind_after_one_file_edit`: **~15.2ms**,
stable across three consecutive runs -- consistent with the ~13.3ms
baseline from just before this work, i.e. no real regression from
threading `Ty` through the hottest path in the crate. Getting there took
two real fixes, not just measurement:
1. `Ty::System`'s `name` field was originally a plain `String`, which
   heap-allocates on every literal `bind_expr` walks -- literals are the
   single most common thing typed in the whole corpus, so this showed up
   as a real, uniform ~13-18% regression across *every* benchmark in the
   suite (not noise -- noise hits one benchmark disproportionately, this
   hit all of them evenly). Fixed by making it `Cow<'static, str>`:
   `Ty::system`/`Ty::system_with_args` (literals, and every hand-written
   name in `crate::generics`) construct a zero-allocation `Cow::Borrowed`
   from a compile-time-known `&'static str`; only a type name captured
   from real, dynamic source text (`Ty::system_owned`, used by
   `type_of_symbol`/`resolve_type_ref`) pays for an owned `String`.
2. A genuine, pre-existing `BindCache` correctness *and* performance bug,
   unrelated to this work but surfaced while benchmarking it:
   `BoundProgram::from_files_cached`'s conservative "some file's
   declarations changed, rebind every file's bodies" fallback calls
   `SymbolTable::append_file_symbols` (which appends each rebind's freshly
   re-collected local-variable symbols onto a file's declared symbols) for
   *every* file being rebound, but only Pass-1-dirty files had their
   declared portion freshly reset first via `set_file_symbols`. For every
   other file swept up by the conservative fallback, each call's locals
   landed on top of the *previous* call's locals instead of replacing
   them -- an unbounded, monotonically growing `SymbolTable` across
   repeated calls. Worse, this was self-sustaining: comparing a fresh
   Pass 1 declared-only count against the now-inflated (declared+stale-
   locals) cached count for the edited file itself always looked like a
   declaration change, which is exactly what triggers the "rebind every
   file" fallback in the first place. Confirmed real (not noise) via a
   standalone `Instant`-timed loop with no criterion involved at all,
   showing per-call cost climbing from ~265ms to ~730ms over 24 calls
   editing the same file, and root-caused by diffing the stuck-`true`
   `declarations_changed` file's before/after symbol counts directly.
   Fixed by giving `SymbolTable` a `declared_len` per file (set whenever
   `set_file_symbols` runs) so `append_file_symbols` can truncate away any
   stale tail before appending regardless of which files were Pass-1-dirty
   this call, and by comparing a fresh Pass 1 collection against
   `declared_symbols_of_file` (declared-only) rather than the full
   declared+locals slice.

## 5. Smaller, concrete loose ends

- [x] **Real bug found via a user report (Emacs/Eglot), fixed the same
      day: an editor's own lock file could crash the server and
      permanently break every later rebuild.** `Path::extension()` splits
      on the *last* `.` in a file name regardless of a leading one, so
      Emacs's `.#Foo.cls` lock file (~30 bytes of plain text, written
      alongside a file while it's open, e.g. `user@host.pid:boot-time`)
      has extension `cls` -- `apex_discover::is_apex_file` (and therefore
      both the initial directory walk and `apexls-server`'s filesystem
      watcher) was treating it as a genuine new Apex class. A *new* file
      appearing forces `apex_binder`'s incremental rebind down its
      conservative "declarations changed somewhere -- rebind everything"
      fallback (§2), which -- for reasons not yet fully root-caused beyond
      this trigger, since excluding the trigger entirely made the crash
      unreproducible in every tested scenario -- panicked inside
      `SymbolTable::get` with an index-out-of-bounds error, poisoning
      `bind.cache`'s `Mutex` and (per the same failure mode `d1d882b`/
      `c106cd6` already document) killing every subsequent rebuild for the
      rest of the session. An initial fix attempt wrapped the rebuild in
      `catch_unwind` with a cache reset and retry -- deliberately reverted
      per explicit user direction: a defensive catch around a crash treats
      the symptom, not the cause, and this project's -- and this user's --
      standing preference is to find and fix the actual bug. The real fix:
      `apex_discover::is_apex_file`/`is_object_meta_file`/`is_field_meta_file`
      now all exclude a hidden file (name starts with `.`) via a new
      `is_hidden` check, shared by both the walk and
      `is_relevant_path` (the watcher's per-event check) -- closing the
      whole class of editor lock/swap-file artifacts (vim's `.foo.cls.swp`
      included), not just this one reporter's specific case. (An
      autosave-style `#Foo.cls#` was never actually affected: its own
      extension is `cls#`, not `cls`.) Verified two ways:
      `crates/apex-discover/tests/hidden_editor_artifact_files.rs`
      (`is_relevant_path` on each artifact-file shape, plus a real
      directory walk proving a lock file sitting next to a real class is
      never discovered) and an end-to-end reproduction against the real
      binary (real filesystem watcher, a real `textDocument/rename`, then
      dropping a real lock file on disk) confirming zero panics and a
      correctly up-to-date bind afterward, where the same scenario
      reliably crashed before the fix.
- [x] `apexls-cli` renamed to `apexls`, a single binary with `server`/
      `ast`/`dead` subcommands (`apexls-server` stays a separate,
      behavior-identical compatibility binary -- editor configs that
      invoke it directly by name over stdio need zero changes). `ast`
      graduated off the pre-binder `parse_statement`-only debug path onto
      `apex_parser::parse_compilation_unit`, but is still parser/printer-
      only, no `apex-binder` involved. `dead` is the first subcommand
      that does call into `apex-binder` (`dead_symbols_in_file`), binding
      the whole project (root auto-detected by walking upward from CWD
      for `sfdx-project.json`, no `--root` override) and treating any
      path arguments as a post-bind report filter, not a partial bind.
      A general "inspect binder output" tool (symbol table dump,
      resolution trace) is still open -- `dead` covers one specific
      binder-backed report, not that broader gap.
- [ ] Distribution/packaging story (binary releases, editor extension
      packaging) -- out of scope for "LSP implementation" per se, but
      relevant to "featureful" in the sense a user could actually reach.
- [ ] Code coverage via `cargo-llvm-cov`, run against the whole workspace
      test suite (unit + integration, including the real-NPSP-corpus and
      protocol-level binary-spawning tests) -- no coverage numbers exist
      anywhere today, so it's currently a guess which real code paths
      (an error-recovery branch, a defensive `else` arm, a rarely-hit
      `Resolution` variant) the existing test suite actually exercises
      versus only looks like it covers. Worth running once as a baseline
      report before deciding whether it's worth wiring into CI as a
      standing gate, rather than assuming a coverage threshold is the
      right bar up front.
- [x] **Generative, syntax-level fuzzer for Apex expressions --**
      `crates/apex-parser/tests/expr_fuzz.rs`, `proptest`-based (already a
      dev-dependency, no new one added). Recursively builds random-but-
      grammatically-valid expression source text mirroring
      `grammar::expressions`'s own production shapes, then checks it (a)
      parses with zero errors, (b) has its top-level expression node cover
      the whole input (not just the tree as a whole -- see below for why
      that distinction matters), (c) round-trips byte-for-byte through
      `apex_printer::render`, and (d) reparses to the same tree shape.
      Complements `roundtrip.rs` (real NPSP-corpus fragments) and
      `metamorphic_parens.rs` (paren-wrapping a fixed hand-picked seed
      corpus, and itself only using `proptest` for combinatorial index
      selection, never generating novel text) -- this is the third,
      distinct lens: genuinely novel *generated* text, exploring operator/
      production combinations neither real code nor a hand-picked corpus
      happens to contain. Deliberately scoped to expressions only for v1
      (matching `metamorphic_parens.rs`'s own scope) -- `new` expressions,
      the `List<Foo>.class` reflection idiom, SOQL/SOSL, and keyword-
      shaped `anyId` member names are all explicitly deferred, the last
      because `grammar::ids`'s `id`/`anyId` classification functions are
      `pub(crate)` and hand-copying that ~140-token exclusion list into a
      test would drift out of sync with the real grammar being tested.

      Finds parser bugs, not binder/semantic bugs (a bare generated
      expression has no surrounding declared-symbol context to resolve
      against): wrong-precedence tree shape on deep mixed-operator
      combinations real code style never produces, multi-token lookahead/
      merge bugs (`<`/`>` into `<=`/`>=`, shift-operator merges), cast-vs-
      paren disambiguation gaps, chained postfix/field/method-call/index
      bugs, and `apex_printer::render` losslessness gaps -- plus, via
      proptest's own shrinking, any panic on structurally-plausible input.

      **Building the generator itself surfaced three real bugs -- not in
      the parser, but in the generator's own understanding of the
      grammar, which is exactly the kind of thing worth shaking out before
      trusting this file's future failures as genuine parser bugs.** All
      three trace back to one fact: `instanceof` sits at its own fixed
      precedence tier (between `equality` and `relational`), and its
      result can only be used where something *at or looser than that
      tier* is expected, never as the receiver of something strictly
      *tighter* without explicit parens:
      1. Chain-suffix operators (`.`/`?.`/`[...]`/postfix `++`/`--`) are
         handled entirely by `expr_primary_chain`/`expr_unary`'s own
         postfix loop, both *below* instanceof in the chain with no way to
         "reach back up" -- `instanceof`'s own right-hand side is a
         `type_ref` (`grammar::types`), not a full expression, so once it
         finishes there's no still-open primary-chain loop left to absorb
         a trailing suffix either (the same reasoning applies to
         `PostfixExpr`, one level up). Fixed with `chain_receiver_strategy`,
         which re-parses each chain-suffix candidate as its own oracle and
         rejects an `InstanceofExpr`/`PostfixExpr`-shaped top level.
      2. The same problem one tier up: operators strictly tighter than
         `instanceof` (relational, shift, additive, multiplicative) can't
         attach to an `InstanceofExpr` either, since `expr_instanceof`
         only ever loops on the literal `instanceof` keyword. First
         misdiagnosed as a `type_ref` generic-argument-list lookahead
         ambiguity (forcing the type to always close with a trailing
         `[ ]` didn't fix it, which is what revealed the real cause).
         Fixed by splitting one flat `binary_op_strategy` into
         `loose_binary_strategy` (assign/coalesce/`||`/`&&`/bitwise/
         equality -- always safe, since those levels' own grammar loops on
         a full instanceof-level operand naturally) and
         `tight_binary_strategy` (relational/shift/additive/
         multiplicative -- both operands filtered the same way a chain-
         suffix receiver is).
      3. `type_strategy` itself originally let array-suffix (`[ ]`) and
         dotted segments (`. Name`) interleave freely, but the real `Type`
         grammar (`TypeName ('.' TypeName)* ('[' ']')*`) requires every
         dotted segment to precede any array suffix -- `A[].a` isn't a
         valid `Type` at all. Found via `cast_strategy` silently orphaning
         its own operand when `try_cast`'s speculative type parse
         correctly failed and rolled back to reinterpreting the whole
         `(type)` as a plain parenthesized expression instead. Fixed by
         building `type_strategy` as two strictly ordered phases (a dotted
         chain, then array suffixes appended after) instead of one flat
         recursive `prop_oneof!`.

      One further, purely mechanical wrinkle, unrelated to the grammar:
      `chain_receiver_strategy`'s `prop_filter`, nested inside
      `prop_recursive`, isn't always honored by proptest's own shrinker --
      a confirmed rough edge in the library (stress-tested the same filter
      in isolation across thousands of fresh, non-shrunk draws with zero
      leaks; the leak only ever appeared via shrinking, and only after all
      three bugs above were already fixed). Since a leak always manifests
      as exactly the top-level-coverage check failing, only that one check
      is a `prop_assume!` rather than a hard assertion -- matching
      `metamorphic_parens.rs`'s own precedent of discarding an unusable
      generated case, and not weakening any of the other three properties.
      **Verified stable**: 25 rounds of 3,000 cases each (75,000 total)
      plus one 20,000-case run, all clean, after the three fixes above;
      the default `with_cases(256)` configuration (matching
      `metamorphic_parens.rs`'s own precedent) also passes cleanly and is
      what `cargo test` runs day to day.
