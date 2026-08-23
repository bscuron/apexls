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
parallelized across passes. `apexls-cli` is still a pre-binder debug tool
(parses a single file with `parse_statement`, doesn't call into
`apex-binder` at all). `apexls-server` is a complete, real LSP protocol
shell (§1 is fully checked off) that now background-rebuilds a real
`apex_binder::BoundProgram` on every edit, incrementally (§2, fully
checked off -- a warm single-file-edit rebind measures ~17ms on the real
NPSP corpus, down from ~677ms cold) -- but nothing consumes that bind
through the protocol yet, so it still can't answer a single real language
question over the wire. That's squarely §3 next.

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

## 3. Feature surface

What's reachable *today* from `apex-binder`'s output vs. what a
featureful LSP needs. Ordered roughly by how directly current data
supports each one.

**Buildable now, no new binder work needed:**
- [ ] `textDocument/hover` -- resolved symbol's kind/type/doc comment
      (`HasDocComment` already gives us the doc text).
- [ ] `textDocument/definition` -- `AstPtr`/`SyntaxPtr` already give a
      resolved symbol's declaration location.
- [ ] `textDocument/documentSymbol` -- outline view, from `SymbolTable`
      filtered to one file.
- [ ] `workspace/symbol` -- from `SymbolTable::by_name_ci`.
- [ ] `textDocument/foldingRange`, `textDocument/selectionRange` --
      directly derivable from the existing CST, no semantic info needed.

**Needs new binder-side work first:**
- [ ] `textDocument/references` / `textDocument/documentHighlight` --
      `ReferenceTable` currently only maps reference -> resolution, not
      the reverse (symbol -> every reference to it). Needs a reverse
      index built during or after Pass 2.
- [ ] `textDocument/rename` (+ `prepareRename`) -- needs find-references
      above, plus safe multi-file edit generation; risky to ship before
      resolution precision is higher than "arity + one-hop type" (see
      §4), since a bad rename is worse than a missing feature.
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
- [ ] `textDocument/codeAction` -- quick fixes (implement missing
      interface methods, etc.) -- nothing started; likely low priority
      until diagnostics (§4) exist to attach actions to.
- [ ] `textDocument/publishDiagnostics` -- parse errors already exist
      and are safe to surface now; semantic diagnostics (unresolved
      symbols, type errors) are **not** safe to ship yet -- see §4,
      first bullet.

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

- [ ] `apexls-cli` still doesn't call into `apex-binder` at all -- it's
      stuck on the pre-binder `parse_statement`-only debug path. Cheap
      to fix, useful for manually inspecting binder output without a
      full server.
- [ ] Distribution/packaging story (binary releases, editor extension
      packaging) -- out of scope for "LSP implementation" per se, but
      relevant to "featureful" in the sense a user could actually reach.
