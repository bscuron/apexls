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
`apex_binder::BoundProgram` on every edit (§2 Step 1) -- but nothing
consumes that bind through the protocol yet, so it still can't answer a
single real language question over the wire. That's squarely the rest of
§2 (incremental rebind is now a measured, confirmed bottleneck, not a
guess) and §3 next.

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
      not true sub-file incremental reparse.** `apex-binder::ParseCache`
      (`BoundProgram::from_files_cached`) skips re-lexing/re-parsing any
      file whose content is byte-for-byte identical to the previous
      rebuild. True sub-file incremental reparse (edit-delta into a
      single file's tree, reusing unaffected subtrees via rowan's
      structural sharing) is a real `apex-parser`-level feature this
      crate has no hooks for yet (only whole-string `parse_*` entry
      points exist) -- deliberately not built, since the measurement
      below shows it wouldn't be this server's bottleneck yet even if it
      existed (see "Step 3 measurement" below).
- [ ] Incremental rebind: re-bind only a changed file's declarations
      plus whatever referenced them, instead of the whole project.
      Needs real dependency tracking (which symbols/scopes/references
      become stale when file X's declarations change) -- meaningfully
      harder than anything built so far in `apex-binder`, and, per the
      measurement below, now confirmed to be **the real bottleneck**,
      not a hypothetical one.
- [ ] A caching/memoization strategy to hang the above off of --
      possibly salsa-style incremental query architecture (what
      rust-analyzer uses), possibly something simpler given this
      project's smaller surface area. Worth a deliberate design pass,
      not an ad hoc bolt-on. Still not designed in detail -- the
      measurement below justifies doing this next, but doesn't by
      itself dictate its shape.
- [ ] Warm background indexing: partially covered by Step 1's
      background-task rebuild (the main loop never blocks on a bind).
      Not yet covered: answering a request that arrives before the
      *first* bind completes (there's no request that consumes the bind
      yet -- §3 -- so this has nothing real to be tested against until
      one exists).
- [x] Profile *real* single-edit request latency. **Measured** via
      `apex-binder/benches/binder_bench.rs`'s `corpus/warm_rebind_after_one_file_edit`
      case (warms `ParseCache` once, then times a full `from_files_cached`
      call per simulated single-file edit, real NPSP corpus, criterion
      `--save-baseline incremental-step1`):
      - `corpus/bind_npsp_full` (cold, no cache): **~677ms** median.
      - `corpus/warm_rebind_after_one_file_edit` (parse-skip warm, one
        file edited): **~438ms** median -- only ~35% faster than cold,
        not the order-of-magnitude drop "feels good to type in" needs.
      - **Honest conclusion:** file-level parse-skip alone is nowhere
        near enough. Nearly all of the ~438ms warm figure is Pass
        1/1.5/2 re-running project-wide over already-parsed trees, which
        this step deliberately left untouched (see Context in the design
        plan this work followed). This is the real evidence, not a
        guess, that **incremental rebind is the next necessary step**,
        not an optional refinement -- and that a salsa-style/memoized
        query layer is worth designing next specifically to make
        Pass 1.5/Pass 2 skip whatever a single file's edit didn't
        actually affect, rather than being a speculative architecture
        choice made ahead of any evidence it was needed.

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
- [ ] **No real type inference** beyond one-hop chaining (a name's
      *declared* type, not the inferred result of an arbitrary
      subexpression). Limits hover accuracy and overload-resolution
      precision for anything past the simplest expressions.
- [ ] **No generics-aware resolution** -- `List<T>`/`Map<K,V>` type
      argument substitution isn't modeled; flagged as deferred in the
      original binder design.
- [ ] **No override-matching semantics** beyond simple inherited-chain
      member lookup (`virtual`/`override` compatibility isn't checked).
- [ ] **No cross-file visibility enforcement** -- `ModifierSet` captures
      `private`/`protected`/`public`/`global` but `lookup_member` doesn't
      filter by accessibility from the reference site, so a private
      member of another class can still appear as a resolution
      candidate. Not urgent for hover, but relevant before completion
      or quick-fixes should suggest only what's actually callable.

## 5. Smaller, concrete loose ends

- [ ] `apexls-cli` still doesn't call into `apex-binder` at all -- it's
      stuck on the pre-binder `parse_statement`-only debug path. Cheap
      to fix, useful for manually inspecting binder output without a
      full server.
- [ ] Distribution/packaging story (binary releases, editor extension
      packaging) -- out of scope for "LSP implementation" per se, but
      relevant to "featureful" in the sense a user could actually reach.
