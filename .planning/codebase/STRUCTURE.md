# Codebase Structure

**Analysis Date:** 2026-09-01

## Directory Layout

```
apexls/
├── crates/
│   ├── apex-binder/          # Symbol table, references, scopes, resolution
│   │   ├── src/
│   │   │   ├── lib.rs        # BoundProgram, three binding stages
│   │   │   ├── symbol.rs     # Symbol, SymbolId, SymbolKind
│   │   │   ├── symbol_table.rs # Project-wide symbol storage
│   │   │   ├── reference_table.rs # Per-file references and resolutions
│   │   │   ├── scope.rs      # Lexical scopes, scope trees
│   │   │   ├── collect.rs    # Stage 1: declaration collection
│   │   │   ├── inherit.rs    # Stage 2: extends/implements resolution
│   │   │   ├── resolve.rs    # Stage 3: reference resolution
│   │   │   ├── soql.rs       # SOQL/SOSL specific resolution
│   │   │   ├── schema_index.rs # Custom object/field schema
│   │   │   ├── schema_index.rs # Custom object/field index
│   │   │   ├── stdlib_index.rs # Standard library schema access
│   │   │   ├── completion.rs # Completion candidate generation
│   │   │   ├── dead_code.rs  # Dead code detection
│   │   │   ├── call_hierarchy.rs # Call hierarchy (incoming/outgoing)
│   │   │   ├── incremental.rs # BindCache for incremental rebuilds
│   │   │   ├── file_table.rs # FileId ↔ path mapping
│   │   │   ├── ptr.rs        # AstPtr, SyntaxPtr abstractions
│   │   │   ├── ty.rs         # Type representations
│   │   │   ├── ci_key.rs     # Case-insensitive lookups
│   │   │   ├── generics.rs   # Generic type parameter handling
│   │   │   ├── label_index.rs # Custom label index
│   │   │   ├── page_index.rs # Visualforce page index
│   │   │   ├── conversions.rs # Type conversions
│   │   │   └── file_id.rs    # FileId wrapper type
│   │   ├── tests/
│   │   └── benches/
│   ├── apex-discover/        # File discovery with smart pruning
│   │   ├── src/
│   │   │   ├── lib.rs        # find_apex_files, discover
│   │   │   └── skip.rs       # Directory skip logic
│   │   ├── tests/
│   │   └── benches/
│   ├── apex-lexer/           # Tokenization
│   │   ├── src/
│   │   │   ├── lib.rs        # Lexer iterator
│   │   │   ├── cursor.rs     # Character cursor
│   │   │   ├── token.rs      # Token, TokenKind
│   │   │   ├── keyword.rs    # Keyword classification
│   │   │   ├── literal.rs    # Literal parsing (strings, numbers)
│   │   │   └── punct.rs      # Punctuation and whitespace
│   │   ├── tests/
│   │   └── benches/
│   ├── apex-metadata/        # SFDX XML parsing
│   │   ├── src/
│   │   │   ├── lib.rs        # SObjectSchema, FieldSchema
│   │   │   ├── discover.rs   # discover_sobjects
│   │   │   ├── labels.rs     # Custom label parsing
│   │   │   ├── visualforce.rs # Visualforce page/component info
│   │   │   └── xml.rs        # XML parsing utilities
│   │   ├── tests/
│   │   └── examples/
│   ├── apex-parser/          # Recursive-descent parser
│   │   ├── src/
│   │   │   ├── lib.rs        # Entry points (parse_*, RECOMMENDED_MIN_STACK_SIZE)
│   │   │   ├── parser.rs     # Parser struct, core logic
│   │   │   ├── input.rs      # Token input stream wrapper
│   │   │   ├── event.rs      # Event-based tree building
│   │   │   ├── errors.rs     # ParseError, Parse struct
│   │   │   └── grammar/
│   │   │       ├── mod.rs    # Grammar module exports
│   │   │       ├── declarations.rs # Class, interface, enum, method, field
│   │   │       ├── expressions.rs # Binary ops, calls, literals, casts
│   │   │       ├── statements.rs # If, while, for, switch, DML, try
│   │   │       ├── types.rs  # Type parsing (generics, arrays)
│   │   │       ├── ids.rs    # Identifier and name parsing
│   │   │       └── soql.rs   # SOQL/SOSL queries
│   │   ├── tests/
│   │   │   ├── roundtrip.rs  # Text → parse → text round-trips
│   │   │   ├── ast_roundtrip.rs # AST round-trip tests
│   │   │   ├── golden.rs     # Golden file snapshot tests
│   │   │   ├── expr_fuzz.rs  # Fuzzing for expressions
│   │   │   ├── metamorphic_parens.rs # Paren equivalence tests
│   │   │   └── soql_sosl_grammar.rs # SOQL/SOSL parsing
│   │   ├── benches/
│   │   └── examples/
│   ├── apex-printer/         # Source reconstruction
│   │   ├── src/
│   │   │   └── lib.rs        # Tree → source output
│   │   └── tests/
│   ├── apex-stdlib/          # Bundled standard library schema
│   │   ├── src/
│   │   │   └── lib.rs        # StdlibIndex from embedded JSON
│   │   ├── data/
│   │   │   ├── standard_objects.json # Standard SObject/field schema
│   │   │   └── apex_reference.json # Standard classes/methods/properties
│   │   └── tests/
│   ├── apex-syntax/          # Lossless CST (rowan-based)
│   │   ├── src/
│   │   │   ├── lib.rs        # rowan types re-exports, SyntaxNode
│   │   │   ├── syntax_kind.rs # SyntaxKind enum, ApexLanguage
│   │   │   └── ast/
│   │   │       ├── mod.rs    # AST module structure
│   │   │       ├── decl.rs   # Declarations (class, method, field, etc.)
│   │   │       ├── expr.rs   # Expressions (binary, call, etc.)
│   │   │       ├── stmt.rs   # Statements (if, while, for, etc.)
│   │   │       └── soql.rs   # SOQL/SOSL AST nodes
│   │   └── tests/
│   ├── apexls/               # CLI entry point aggregator
│   │   ├── src/
│   │   │   ├── main.rs       # Subcommand routing (server, ast, dead)
│   │   │   ├── ast.rs        # Parse and dump command
│   │   │   └── dead.rs       # Dead code report command
│   │   ├── tests/
│   │   └── Cargo.toml        # Binary crate
│   ├── apexls-server/        # LSP server implementation
│   │   ├── src/
│   │   │   ├── main.rs       # Binary entry (thin wrapper)
│   │   │   ├── lib.rs        # Backend, BindState, run_server()
│   │   │   ├── capabilities.rs # Position resolution, location formatting
│   │   │   └── line_index.rs # LineIndex, position encoding
│   │   ├── tests/
│   │   └── Cargo.toml        # Library + binary
│   └── Cargo.toml workspace members list
├── tests/
│   ├── corpus/               # End-to-end test corpus
│   │   └── npsp/             # NPSP (Nonprofit Success Pack) test files
│   └── ...
├── docs/
│   └── grammar/
│       └── apex.ebnf         # Grammar reference (EBNF)
├── fuzz/
│   ├── fuzz_targets/         # libFuzzer targets
│   └── Cargo.toml
├── oracle/                   # Grammar oracle/verification infrastructure
│   ├── adapters/             # Adapters to reference parsers (ANTLR, tree-sitter)
│   ├── scratch-org/          # SF CLI test org connection
│   └── ...
├── tools/
│   └── salesforce-doc-scraper/ # Scrapes standard library docs to JSON
├── .github/workflows/        # CI configuration
├── .planning/
│   ├── codebase/             # Generated codebase analysis docs
│   └── ...
├── .claude/                  # Project-specific Claude config
├── Cargo.toml                # Workspace root
├── Cargo.lock                # Dependency lock file
└── README.md                 # Project overview
```

## Directory Purposes

**crates/apex-binder:**
- Purpose: Symbol table, reference resolution, semantic analysis
- Contains: Three-stage binding (collect/inherit/resolve), scopes, indices
- Key files: `lib.rs` (entry point), `symbol.rs`, `symbol_table.rs`, `reference_table.rs`

**crates/apex-discover:**
- Purpose: Efficient parallel file discovery with smart directory pruning
- Contains: Directory walk, skip logic, file classification
- Key files: `lib.rs` (entry points), `skip.rs` (prune list)

**crates/apex-lexer:**
- Purpose: Tokenization with SOQL/SOSL support
- Contains: Iterator-based lexer, token classification
- Key files: `lib.rs` (Lexer iterator), `cursor.rs` (character position tracking)

**crates/apex-metadata:**
- Purpose: SFDX metadata parsing (objects, fields, labels, Visualforce)
- Contains: XML parsing, schema structures
- Key files: `lib.rs` (SObjectSchema, FieldSchema), `discover.rs` (metadata file discovery)

**crates/apex-parser:**
- Purpose: Recursive-descent parsing with error recovery
- Contains: Grammar modules, parse tree building, error collection
- Key files: `lib.rs` (entry points), `parser.rs` (core), `grammar/` (syntax rules)

**crates/apex-printer:**
- Purpose: Source code reconstruction from syntax trees
- Contains: Tree traversal producing text output
- Key files: `lib.rs` (printing logic)

**crates/apex-stdlib:**
- Purpose: Bundled offline Salesforce standard schema
- Contains: Embedded JSON, deserialization, StdlibIndex
- Key files: `lib.rs` (StdlibIndex), `data/` (JSON snapshots)

**crates/apex-syntax:**
- Purpose: Lossless concrete syntax tree representation
- Contains: rowan language definition, SyntaxKind, typed AST
- Key files: `lib.rs` (rowan re-exports), `syntax_kind.rs` (SyntaxKind), `ast/` (typed accessors)

**crates/apexls:**
- Purpose: CLI entry point, subcommand aggregation
- Contains: `server`, `ast`, `dead` subcommands
- Key files: `main.rs` (routing), `ast.rs` (parse-dump), `dead.rs` (dead code report)

**crates/apexls-server:**
- Purpose: LSP protocol server implementation
- Contains: async-lsp middleware, Backend state, request handlers
- Key files: `lib.rs` (Backend, BindState, handlers), `main.rs` (binary wrapper)

**tests/corpus/:**
- Purpose: End-to-end test fixtures and golden files
- Contains: Real Apex code from projects like NPSP
- Key files: `.cls`, `.trigger`, `.object-meta.xml` files

**docs/grammar/:**
- Purpose: Grammar documentation and reference
- Contains: EBNF grammar definition
- Key files: `apex.ebnf` (grammar specification)

**fuzz/:**
- Purpose: Fuzzing harness and targets for parser robustness
- Contains: libFuzzer targets for expressions, statements, etc.

**oracle/:**
- Purpose: Grammar oracle and verification infrastructure
- Contains: Adapters to reference parsers (ANTLR, tree-sitter), scratch org setup
- Purpose: Allows comparison of apexls parser output against reference implementations

**tools/salesforce-doc-scraper/:**
- Purpose: Scrapes Salesforce docs to refresh stdlib data
- Contains: Doc scraping logic, generates `standard_objects.json` and `apex_reference.json`
- Purpose: One-time tool, run per Apex release to refresh bundled schema

## Key File Locations

**Entry Points:**
- `crates/apexls-server/src/main.rs`: Binary entry (delegates to run_server)
- `crates/apexls/src/main.rs`: CLI entry with subcommand routing
- `crates/apexls-server/src/lib.rs:run_server()`: Shared LSP server implementation

**Core Configuration:**
- `Cargo.toml`: Workspace members, shared dependencies
- `.claude/settings.json`: Claude Code settings (if any)
- `docs/grammar/apex.ebnf`: Grammar specification

**Core Binding Logic:**
- `crates/apex-binder/src/lib.rs`: BoundProgram, three-stage binding entry points
- `crates/apex-binder/src/collect.rs`: Stage 1 (symbol collection)
- `crates/apex-binder/src/inherit.rs`: Stage 2 (inheritance resolution)
- `crates/apex-binder/src/resolve.rs`: Stage 3 (reference resolution)

**Core Parsing:**
- `crates/apex-parser/src/lib.rs`: Parse entry points (parse_compilation_unit, parse_trigger_unit)
- `crates/apex-parser/src/parser.rs`: Parser state machine
- `crates/apex-parser/src/grammar/mod.rs`: Grammar module aggregation

**LSP Server:**
- `crates/apexls-server/src/lib.rs:Backend`: Server state and lifecycle
- `crates/apexls-server/src/lib.rs:BindState`: Background rebuild coordination
- `crates/apexls-server/src/capabilities.rs`: LSP capability implementations

**Testing:**
- `tests/corpus/`: Golden files and test fixtures
- `crates/apex-parser/tests/golden.rs`: Snapshot testing infrastructure

## Naming Conventions

**Files:**
- `lib.rs`: Crate public API
- `main.rs`: Binary entry point (apexls, apexls-server)
- Descriptive snake_case module names: `symbol_table.rs`, `reference_table.rs`, `dead_code.rs`
- `*_index.rs`: Index structures (schema_index, stdlib_index, label_index, page_index)
- `grammar/declarations.rs`, `grammar/expressions.rs`, `grammar/statements.rs`: Grammar rules by category

**Directories:**
- `crates/apex-*`: Library crates (lexer, parser, syntax, binder, etc.)
- `crates/apexls*`: Application crates (CLI aggregator, server)
- `src/`: Source code (always present in crates)
- `tests/`: Integration tests
- `benches/`: Benchmark suites
- `ast/`: AST module subtree within apex-syntax

**Types (PascalCase):**
- `BoundProgram`, `SymbolTable`, `ReferenceTable`, `ScopeTree`
- `Backend`, `BindState`, `BindCache`
- `Parser`, `Lexer`
- `Symbol`, `SymbolId`, `FileId`
- `Parse`, `ParseError`
- `SObjectSchema`, `FieldSchema`

**Functions (snake_case):**
- Module-level: `parse_compilation_unit`, `parse_trigger_unit`, `tokenize`
- Methods: `from_files`, `from_files_cached`, `resolve_position`, `complete_at`
- Lifecycle: `run_server`, `initialize`, `shutdown`

**Modules (snake_case):**
- `collect`, `inherit`, `resolve` (binding stages)
- `symbol_table`, `reference_table`, `scope` (indices/tables)
- `declarations`, `expressions`, `statements`, `types` (grammar)

## Where to Add New Code

**New LSP Capability (e.g., formatting, refactoring):**
- Primary code: `crates/apexls-server/src/lib.rs` (add request handler method to `Backend`)
- Capability registration: `crates/apexls-server/src/capabilities.rs` (add capability to ServerCapabilities)
- Tests: `crates/apexls-server/tests/` (new integration test file)
- Related binder code: `crates/apex-binder/src/lib.rs` if new semantic queries needed

**New Binding Query (e.g., new completion category):**
- Implementation: `crates/apex-binder/src/completion.rs` (add CompletionCandidate generation)
- Reference: `crates/apex-binder/src/lib.rs` (expose new public query function if needed)
- Tests: `crates/apex-binder/tests/` (new integration test file)

**New Diagnostic (e.g., unused variable):**
- Implementation: `crates/apex-binder/src/dead_code.rs` (extends existing dead code detection)
- Or new file: `crates/apex-binder/src/diagnostics_*.rs` for new categories
- Tests: `crates/apex-binder/tests/` (test the diagnostic rules)

**New Grammar Rule (new Apex syntax support):**
- Grammar: `crates/apex-parser/src/grammar/statements.rs` or `expressions.rs` (add parsing function)
- AST: `crates/apex-syntax/src/ast/stmt.rs` or `expr.rs` (add SyntaxNode wrapper)
- Parser tests: `crates/apex-parser/tests/golden.rs` (add golden file)
- Binder impacts: `crates/apex-binder/src/resolve.rs` (if new bindings needed)

**New Utility/Helper:**
- Shared parsing helper: `crates/apex-parser/src/parser.rs` (if used across grammar modules)
- Shared binding helper: `crates/apex-binder/src/` (new module if significant)
- Shared type: `crates/apex-syntax/src/lib.rs` (if used across layers)

**CLI Command:**
- Entry: `crates/apexls/src/main.rs` (add Command variant)
- Implementation: `crates/apexls/src/{command_name}.rs` (new module)
- Tests: `crates/apexls/tests/` (CLI integration tests)

## Special Directories

**crates/apex-stdlib/data/:**
- Purpose: Embedded JSON snapshots of standard schema
- Generated: No (manually refreshed from scraper)
- Committed: Yes (bundled with binary)
- Contents: `standard_objects.json` (SObjects/fields), `apex_reference.json` (classes/methods)
- Update: Re-run `tools/salesforce-doc-scraper` per Apex release, copy output here

**tests/corpus/npsp/:**
- Purpose: Real NPSP codebase for end-to-end testing
- Generated: No
- Committed: Yes (test fixture)
- Contents: `.cls`, `.trigger`, `.object-meta.xml` files from NPSP
- Use: Grammar oracle, binding tests, performance benchmarks

**fuzz/fuzz_targets/:**
- Purpose: libFuzzer targets for parser robustness
- Generated: Yes (fuzz harness builds its own binaries)
- Committed: Yes (target definitions)
- Use: Continuous fuzzing against parser to find crashes/panics

**oracle/adapters/:**
- Purpose: Adapters to reference parser implementations
- Generated: No (submodules or external repos)
- Committed: Yes (adapter code)
- Use: Compare apexls parser output against ANTLR, tree-sitter

**target/:**
- Purpose: Cargo build artifacts
- Generated: Yes (during `cargo build`)
- Committed: No (in .gitignore)

**.planning/codebase/:**
- Purpose: Generated codebase analysis documents
- Generated: Yes (by `/gsd-map-codebase` skill)
- Committed: Yes (shared reference for team)
- Contents: ARCHITECTURE.md, STRUCTURE.md, CONVENTIONS.md, TESTING.md, STACK.md, INTEGRATIONS.md, CONCERNS.md

---

*Structure analysis: 2026-09-01*
