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
index on `ReferenceTable` (`by_symbol`). `textDocument/rename` is done
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
Next up is §3's remaining "needs
new binder-side work first" list (`signatureHelp`, `completion`,
`semanticTokens`, `callHierarchy`, `inlayHint`), or §4's still-open
standard-library/schema type-model gap.

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
         confirmation that anything outside the curated set -- `Id` vs
         `Blob`, say -- still stays honestly `Candidates` rather than a
         guessed elimination).
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
- [ ] `textDocument/signatureHelp` -- have `narrow_by_overload`'s
      candidate set; needs argument-position tracking (which parameter
      is the cursor currently in) layered on top.
- [ ] `textDocument/completion` -- needs scope-aware +
      member-aware suggestion (have the data via `ScopeTree`/
      `SymbolTable`), but also needs the parser's error recovery to
      behave well for *mid-token* input (completion fires while the
      user is still typing an identifier, a different failure mode than
      recovering from a finished-but-wrong file).
- [ ] `textDocument/semanticTokens` -- needs a token-classification pass
      over the resolved AST (e.g. distinguishing a field access from a
      local, a resolved type name from an unresolved one).
- [ ] `textDocument/callHierarchy` (prepare + incoming/outgoing) --
      needs a real call graph, which the current per-reference
      `Candidates`/`Resolved` model doesn't build as a first-class
      structure yet.
- [ ] `textDocument/inlayHint` -- e.g. inferred local types; blocked on
      the same type-inference limits as §4.

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

## 4. Correctness gaps that block features, not just refine them

These were flagged as deliberate, documented v1 scope cuts while
building `apex-binder`, but they're not just "nice to have more
precision" -- they directly block shipping certain features honestly.

- [ ] **No standard-library type model** (`String`, `List`/`Map`/`Set`
      built-in methods, `Database`, `Test`, `System`, `Schema`,
      `Exception` hierarchy, ...). This is why a large share of
      real-world references currently resolve to `Unresolved` -- and
      why semantic diagnostics can't ship yet: flagging "unresolved
      symbol" on every `String.isBlank(...)` call would drown any real
      signal in false positives. Options: hand-author a stub library
      (large, ongoing maintenance burden as Salesforce ships new
      System-namespace APIs 3x/year), or generate one from a connected
      org's Tooling API/Apex reflection.
- [ ] **No standard SObject/field schema** (`Account`, `Contact`,
      `Opportunity`, ...) locally -- `apex-metadata`'s documented gap.
      Same two options as above: bundled snapshot (goes stale) or a
      live org describe call (the same `sf`/Tooling API oracle already
      used to verify grammar questions against a real org earlier in
      this project). This is the more tractable of the two "unmodeled
      Salesforce surface" gaps, since describe calls are a solved,
      already-integrated pattern for this project.
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
      an operand's type. **What this doesn't do:** cover the rest of the
      standard library's real method surface (still the separate,
      still-unmodeled stdlib-model gap below) or attempt common-supertype
      inference for a ternary's two branches beyond exact `Ty` equality.
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
      resolution). Not exhaustive: `sort`/`addAll`/`retainAll`/`clone`/
      `iterator`/... fall through to today's `Unresolved`, same as any
      other unmodeled system method, no regression.
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
- [ ] **No standard-library type model** carries forward unchanged (see
      above) -- `Ty::System` can *name* an unmodeled type now, but still
      can't resolve members of one beyond the `List`/`Map`/`Set` table
      just added.

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
