<!-- refreshed: 2026-09-01 -->
# Architecture

**Analysis Date:** 2026-09-01

## System Overview

```text
┌─────────────────────────────────────────────────────────────────────┐
│                      LSP Protocol Layer                              │
│                    `apexls-server/src/lib.rs`                        │
│         (async-lsp, stdio, lifecycle, capabilities)                  │
└──────────────┬─────────────────────────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────────────────────────┐
│                    Semantic Layer / Binder                           │
│                   `apex-binder/src/lib.rs`                           │
│    (BoundProgram, symbol table, references, scopes, indices)         │
└──────────────┬──────────────┬──────────────┬───────────────┬─────────┘
               │              │              │               │
    ┌──────────▼──┐  ┌────────▼──┐  ┌──────▼───┐  ┌────────▼──┐
    │    Parser   │  │   Schema  │  │ Standard │  │  Printer  │
    │             │  │  Metadata │  │  Lib     │  │           │
    │ CST/Parse   │  │  SObjects │  │ Classes  │  │  To Src   │
    └──────────────┘  └───────────┘  └──────────┘  └───────────┘
               │              │              │
┌──────────────▼──────────────▼──────────────▼────────────────────────┐
│                    Syntax Layer (rowan CST)                          │
│                   `apex-syntax/src/lib.rs`                           │
│        (SyntaxNode/Token, red-green trees, AST layer)                │
└──────────────┬─────────────────────────────────────────────────────┘
               │
┌──────────────▼─────────────────────────────────────────────────────┐
│                      Parsing Layer                                   │
│                  `apex-parser/src/lib.rs`                            │
│     (Recursive-descent, error recovery, event stream building)       │
└──────────────┬─────────────────────────────────────────────────────┘
               │
┌──────────────▼─────────────────────────────────────────────────────┐
│                     Lexing Layer                                     │
│                  `apex-lexer/src/lib.rs`                             │
│        (Iterator-based tokenizer, SOQL-aware, zero-copy)             │
└──────────────┬──────────────┬──────────────────────────────────────┘
               │              │
    ┌──────────▼──┐  ┌────────▼──────┐
    │  Apex Src   │  │ File Discovery │
    │  Text       │  │   & Metadata   │
    │             │  │ (apex-discover)│
    └─────────────┘  └────────────────┘
```

## Component Responsibilities

| Component | Responsibility | File |
|-----------|----------------|------|
| LSP Server | Protocol layer: lifecycle, notifications, requests, position encoding | `crates/apexls-server/src/lib.rs` |
| Backend | Maintains LSP server state, schedules rebuilds, manages bind cache | `crates/apexls-server/src/lib.rs:struct Backend` |
| BindState | Arc-shared mutable state for background rebuilds, thread-safe via parking_lot | `crates/apexls-server/src/lib.rs:struct BindState` |
| BoundProgram | Whole project snapshot: files, symbols, references, schema, scopes | `crates/apex-binder/src/lib.rs:struct BoundProgram` |
| Parser | Recursive-descent parser producing CST with error recovery | `crates/apex-parser/src/parser.rs` |
| Lexer | Token stream generation, SOQL/SOSL-aware, zero-copy | `crates/apex-lexer/src/lib.rs:struct Lexer` |
| SyntaxNode/Token | rowan red-green tree nodes/tokens, typed AST traversal | `crates/apex-syntax/src/lib.rs` |
| SymbolTable | Project-wide symbol collection (classes, methods, fields) | `crates/apex-binder/src/symbol_table.rs` |
| ReferenceTable | File-local references and their resolutions | `crates/apex-binder/src/reference_table.rs` |
| ScopeTree | Per-body lexical scoping (locals, parameters) | `crates/apex-binder/src/scope.rs` |
| SchemaIndex | Custom SObject/field metadata from project | `crates/apex-binder/src/schema_index.rs` |
| StdlibIndex | Standard Salesforce classes/methods/objects schema | `crates/apex-stdlib/src/lib.rs` |
| BindCache | Incremental rebuild state across editor sessions | `crates/apex-binder/src/incremental.rs` |

## Pattern Overview

**Overall:** Multi-layer compiler frontend with clear separation: lexing → parsing → AST/CST → semantic binding → LSP capabilities

**Key Characteristics:**
- **Lossless syntax trees**: Rowan red-green trees preserve trivia (whitespace, comments)
- **Cheap AST traversal**: Typed AST layer built as zero-copy views over CST
- **Parallel processing**: rayon used for file discovery and binding phases where work is per-item independent
- **Incremental rebuilding**: BindCache avoids re-parsing/re-binding unchanged files
- **Single-root workspace**: Org namespace is global, no multi-root support by design
- **Thread-safe state sharing**: Arc + parking_lot for background rebuilds without poisoning

## Layers

**Lexer Layer (`apex-lexer`):**
- Purpose: Break source text into tokens with SOQL/SOSL awareness
- Location: `crates/apex-lexer/src/lib.rs`
- Contains: Iterator-based tokenizer, keyword/literal/punct classification
- Depends on: None (std library only)
- Used by: Parser (`apex-parser`)

**Parser Layer (`apex-parser`):**
- Purpose: Recursive-descent parsing, error recovery, CST production
- Location: `crates/apex-parser/src/lib.rs`
- Contains: Grammar modules (declarations, expressions, statements, types, ids, SOQL), event-stream building, parse error tracking
- Depends on: `apex-lexer`, `apex-syntax` (for SyntaxKind)
- Used by: Binder (`apex-binder`), CLI (`apexls`)

**Syntax Layer (`apex-syntax`):**
- Purpose: Lossless CST representation and typed AST traversal
- Location: `crates/apex-syntax/src/lib.rs`
- Contains: rowan language definition, SyntaxKind enum, AST modules (decl, expr, stmt, soql)
- Depends on: rowan crate (red-green tree library)
- Used by: Parser, Binder, Server

**Discovery Layer (`apex-discover`):**
- Purpose: Efficient parallel file discovery with smart pruning
- Location: `crates/apex-discover/src/lib.rs`
- Contains: Parallel walker using `ignore` crate, skip list for non-Apex directories, file classification
- Depends on: `ignore` (ripgrep's parallel walker)
- Used by: Binder, CLI tools

**Metadata Layer (`apex-metadata`):**
- Purpose: Parse SFDX XML metadata files into schema structures
- Location: `crates/apex-metadata/src/lib.rs`
- Contains: XML parsing, SObjectSchema/FieldSchema structures, label/page discovery
- Depends on: XML parsing, `serde`
- Used by: Binder

**Stdlib Layer (`apex-stdlib`):**
- Purpose: Bundled standard Apex schema (System classes, standard objects/fields)
- Location: `crates/apex-stdlib/src/lib.rs`
- Contains: Deserialization of scraped JSON data (`data/*.json`), StdlibIndex
- Depends on: `serde`, embedded JSON data files
- Used by: Binder (global singleton)

**Binder Layer (`apex-binder`):**
- Purpose: Symbol table building and reference resolution
- Location: `crates/apex-binder/src/lib.rs`
- Contains: Three-stage binding (collect, inherit, resolve), symbol/scope/reference tables, completion, dead code detection
- Depends on: All layers below (lexer, parser, syntax, discover, metadata, stdlib)
- Used by: Server, CLI

**Printer Layer (`apex-printer`):**
- Purpose: Reconstructing source from syntax trees (formatting)
- Location: `crates/apex-printer/src/lib.rs`
- Contains: Tree traversal producing source output
- Depends on: `apex-syntax`
- Used by: CLI commands

**Server Layer (`apexls-server`):**
- Purpose: LSP protocol implementation
- Location: `crates/apexls-server/src/lib.rs`
- Contains: async-lsp middleware stack, Backend state machine, file watcher, request handlers
- Depends on: `apex-binder`, `async-lsp`, `tokio`, `tower`
- Used by: Two entry points (binary, CLI subcommand)

**CLI Layer (`apexls`):**
- Purpose: CLI aggregation and subcommand routing
- Location: `crates/apexls/src/main.rs`
- Contains: `ast` (parse-and-dump), `dead` (dead code report), `server` (LSP)
- Depends on: `apex-parser`, `apex-binder`, `apexls-server`, `clap`

## Data Flow

### Primary Request Path (e.g., completion, hover)

1. LSP client sends request with URI + position (`crates/apexls-server/src/lib.rs:Backend::on_completion`)
2. Position resolved to (FileId, TextSize) via `resolve_position` (`crates/apexls-server/src/capabilities.rs`)
3. Current `BoundProgram` queried for completion/references/etc
4. `BoundProgram` consults symbol table, reference table, scopes
5. Result converted back to LSP format and sent to client

### Build Rebuild Path (file edit)

1. Editor sends `didChange` notification (`Backend::on_did_change`)
2. `Backend::documents` updated with new text (unsaved buffer)
3. `Backend::schedule_rebuild` queues rebuild task via `BindState::rebuild_requested`
4. Background worker (`spawn_rebuild_worker`) pulls from queue
5. Worker calls `BoundProgram::from_files_cached` with current documents override
6. Parser phase: each file tokenized, parsed into `Parse` (CST)
7. Collect phase (parallel): declarations extracted, symbols created
8. Inherit phase: extends/implements chains resolved
9. Resolve phase (parallel): references resolved, scopes built
10. New `BoundProgram` stored in `BindState::program`, ready for next request
11. Rapid edits coalesce: queue deduplication means one rebuild per stable state

### Filesystem Watch Path

1. `Backend::start_watcher` registers OS-level watch on project root
2. Watcher callback (notify thread) detects file add/remove/rename
3. Queues rebuild (same queue as document-sync)
4. Ensures off-disk files picked up even if editor never opened them

**State Management:**
- `Backend::documents` (HashMap<Url, String>): In-memory buffers, unsaved edits
- `BindState::program` (RwLock): Latest `BoundProgram` snapshot, None until first rebuild
- `BindState::cache` (Mutex): `BindCache` tracking parse/bind freshness across rebuilds

## Key Abstractions

**BoundProgram:**
- Purpose: Snapshot of whole project state at a point in time
- Examples: `crates/apex-binder/src/lib.rs:struct BoundProgram`
- Pattern: Owned, independent snapshot; holds Arc-clones of schema/stdlib/labels/pages for cheap sharing

**Symbol & SymbolTable:**
- Purpose: Represent and store project declarations (types, fields, methods, locals)
- Examples: `crates/apex-binder/src/symbol.rs`, `symbol_table.rs`
- Pattern: Symbols are immutable once created; SymbolTable built in parallel collect phase

**ReferenceTable & Resolution:**
- Purpose: Map each reference site to its declaration site(s)
- Examples: `crates/apex-binder/src/reference_table.rs`
- Pattern: Per-file `Arc<FileBodies>` holds references, reduces copy on unchanged files

**ScopeTree:**
- Purpose: Lexical scope hierarchy (locals, parameters, fields)
- Examples: `crates/apex-binder/src/scope.rs`
- Pattern: Rooted at a body (method/constructor/property), parent chain up to class scope

**Parse / SyntaxNode:**
- Purpose: Bridge between text and tree
- Examples: `crates/apex-parser/src/errors.rs:struct Parse`, `apex_syntax::SyntaxNode`
- Pattern: Parse holds green tree (Arc-based), creates SyntaxNode on demand as cheap view

**LineIndex:**
- Purpose: Convert between byte offsets and line:column positions
- Examples: `crates/apexls-server/src/line_index.rs`
- Pattern: Built once per file for position encoding negotiation (UTF-16 vs UTF-8)

## Entry Points

**apexls-server binary:**
- Location: `crates/apexls-server/src/main.rs`
- Triggers: Direct invocation by editor over stdio
- Responsibilities: Delegates to `apexls_server::run_server()` (unchanged behavior for backward compatibility)

**apexls server subcommand:**
- Location: `crates/apexls/src/main.rs` → `apexls_server::run_server()`
- Triggers: `apexls server` CLI invocation
- Responsibilities: Same LSP server implementation as binary, shared code in `apexls-server::lib`

**apexls ast subcommand:**
- Location: `crates/apexls/src/main.rs` → `ast::run(file)`
- Triggers: `apexls ast <file>` CLI invocation
- Responsibilities: Parse file, dump syntax tree to stdout

**apexls dead subcommand:**
- Location: `crates/apexls/src/main.rs` → `dead::run(paths)`
- Triggers: `apexls dead <paths...>` CLI invocation
- Responsibilities: Bind project, detect dead code in scope

## Architectural Constraints

- **Threading:** Single-threaded async event loop (tokio current_thread) for LSP protocol. Background rebuilds on `tokio::task::spawn_blocking` thread pool. Rayon used for discovery/binding parallelism where work is per-item independent. No async inside binder (pure rayon).

- **Global state:** `apex_stdlib::global_stdlib_index()` is process-wide singleton (`OnceLock`). Rayon thread pool built once with large stack. `Backend::documents` holds in-memory unsaved buffers (must override on-disk during rebuild).

- **Circular imports:** None; clean dependency graph: CLI → Server → Binder → (Parser, Metadata, Discover, Stdlib) → Lexer/Syntax.

- **Lock poisoning:** `parking_lot::Mutex`/`RwLock` used instead of `std::sync` to avoid poisoning cascade. A panic in one request doesn't break all later requests.

- **Position encoding:** Negotiated once in `initialize` (default UTF-16 per LSP spec). Stored in `Backend::position_encoding`, used by all position conversions.

- **Single-root by design:** `Backend::root` is Option<Url>, only takes first workspace folder. Multi-root would merge independent orgs' symbols incorrectly.

- **Full-document sync only:** `DidChangeTextDocumentParams::contentChanges` expected to be single full document, not incremental ranges. Deliberate choice for simplicity, tracked as future optimization.

- **Stack safety for tree drops:** Rayon workers built with `RECOMMENDED_MIN_STACK_SIZE` (64 MiB) to handle deep expression trees. Avoids stack overflow when dropping left-nested expressions from generated code.

## Anti-Patterns

### Mutable State Outside Locks

**What happens:** Code holding references to mutable state without synchronization primitives.
**Why it's wrong:** Race conditions in multi-threaded rebuild worker. One edit's rebuild races with another edit queuing changes.
**Do this instead:** Use `BindState` (Arc-shared with Mutex/RwLock). `Backend::documents` protected by interior mutability or arc-wrapping. See `crates/apexls-server/src/lib.rs:struct BindState`.

### Ignoring Incremental Caching Opportunities

**What happens:** Each rebuild re-parses all files regardless of changes.
**Why it's wrong:** Performance regression on large projects; every edit on large codebases becomes slow.
**Do this instead:** Use `BindCache` (from `crates/apex-binder/src/incremental.rs`). It tracks parse/bind freshness per file. Call `BoundProgram::from_files_cached` with cache, not `from_files`.

### Panic Under Held Lock

**What happens:** A request handler panics while holding `program.read()` guard.
**Why it's wrong:** With `std::sync` locks, the lock poisons, all future requests fail forever.
**Do this instead:** Use `parking_lot` locks (already done in `BindState`). `CatchUnwindLayer` from `async-lsp` unwinds panics cleanly per-request. See `crates/apexls-server/src/lib.rs:BindState` field types.

## Error Handling

**Strategy:** Parser produces `Parse { green, errors }` rather than panicking on bad syntax. Binder produces `Resolution::Unresolved` or `Candidates` for ambiguous references. LSP requests catch panics and return error responses.

**Patterns:**
- Parser errors are non-fatal, recovery resynchronizes at statement/member boundary
- Type system uses `Resolution` enum to represent ambiguous/unknown references
- LSP requests wrapped by `CatchUnwindLayer` to convert panics to error responses
- File read errors in discovery silently skip unreadable files
- Metadata parse errors skip files, building partial schema from remaining files

## Cross-Cutting Concerns

**Logging:** `tracing` crate with levels (INFO for lifecycle, WARN for degraded state). No structured logging in request handlers (performance). See `apexls-server/src/lib.rs` imports.

**Validation:** Parser error recovery provides best-effort tree even on syntax errors. Reference resolution reports ambiguity via `Resolution::Candidates` rather than failing. Schema lookups gracefully degrade to unresolved.

**Authentication:** Not applicable; LSP runs over stdio under editor's own privilege context. No network authentication needed.

**Performance:** Pervasive use of Arc, cheap clones, and structural sharing. `NodeCache` deduplicates small syntax nodes across files. Parallel phases leverage rayon for I/O-heavy discovery and CPU-heavy binding.

---

*Architecture analysis: 2026-09-01*
