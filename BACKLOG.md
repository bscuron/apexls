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
`apex-binder` at all). `apexls-server` is a minimal but real LSP protocol
shell (see §1) -- it doesn't call into `apex-binder` either yet.

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
- [ ] Workspace folder handling (multi-root support, or an honest
      single-root-only limitation stated up front). Not started --
      `initialize` logs the workspace-folder count and warns on >1, but
      nothing yet actually resolves or uses a project root at all
      (there's no binder integration yet for it to feed into).
- [ ] Configuration: `workspace/didChangeConfiguration`,
      `initializationOptions` (e.g. where to find SFDX metadata, whether
      to bundle standard-library stubs -- see §4).
- [ ] Position encoding: LSP positions are UTF-16 line/character by
      default; our `TextRange`/`TextSize` are byte offsets. Need a
      line-index + UTF-16 conversion layer, or negotiate a different
      `positionEncodingKind` if the client supports it (most do now).
      Not started -- no capability implemented yet actually consumes a
      position, so there's nothing to convert for yet either.
- [ ] Cancellation: long-running requests (a whole-project rebind) need
      to be cancellable when a newer request supersedes them, or the
      server will feel unresponsive under real editing load. `async-lsp`
      has the plumbing for this (its `ConcurrencyLayer`); not yet wired
      to anything, since there's no long-running request to cancel yet.
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

- [ ] Incremental reparse: `apex-parser`/rowan trees support this in
      principle (structural sharing), but nothing wires an edit-delta
      into a reparse that reuses unaffected subtrees -- today every
      change means re-lexing and re-parsing the whole file from scratch.
- [ ] Incremental rebind: re-bind only a changed file's declarations
      plus whatever referenced them, instead of the whole project.
      Needs real dependency tracking (which symbols/scopes/references
      become stale when file X's declarations change) -- meaningfully
      harder than anything built so far in `apex-binder`.
- [ ] A caching/memoization strategy to hang the above off of --
      possibly salsa-style incremental query architecture (what
      rust-analyzer uses), possibly something simpler given this
      project's smaller surface area. Worth a deliberate design pass,
      not an ad hoc bolt-on.
- [ ] Warm background indexing: avoid paying full-project cold-start
      cost synchronously on `initialize` for large orgs/repos.
- [ ] Once a server exists (§1), profile *real* single-edit request
      latency -- bulk throughput numbers already measured (whole-corpus
      bind ~460ms) don't tell us what a single keystroke's round trip
      would cost without incremental rebind.

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
