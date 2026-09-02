# Coding Conventions

**Analysis Date:** 2026-09-01

## Naming Patterns

**Files:**
- Lowercase snake_case: `scope.rs`, `symbol_table.rs`, `completion.rs`
- Module-level functionality groups: lexer, parser, binder, server layers
- Integration tests named after feature/invariant: `scope_resolution_smoke.rs`, `declaration_grammar_coverage.rs`, `array_initializer_binding.rs`
- Benchmarks follow same naming: `lexer_bench.rs`, `binder_bench.rs`

**Functions:**
- Lowercase snake_case with descriptive names
- Compound verbs for action functions: `resolve_local()`, `bind_name_expr()`, `narrow_by_overload()`, `remap_symbol_ids()`
- Prefix patterns: `is_*` for boolean checks (`is_type()`, `is_system()`, `is_inherited`), `lookup_*` for search operations
- Public API functions fully documented with module-level doc comments explaining semantics
- Private helpers (`fn` only) are lean and focused, with doc comments only when behavior is non-obvious

**Variables:**
- Lowercase snake_case throughout
- Mutable binding names unchanged from immutable: `let x = 5; let mut x = 5;` (not `mut_x`)
- Single-letter names used only in tight loops/tight closures where context is obvious: `for (n, _) in self.bindings.iter().rev()`
- Type names preserved in scope locals: `scope_id`, `symbol_id`, `file_id` (not abbreviated)
- Bool variables prefixed with `is_` when describing state: `is_resolved`, `is_inherited`, `is_static`, `is_final`, `is_override`, `is_abstract`, `is_testmethod`, `is_transient`, `is_webservice`, `is_test_visible`

**Types:**
- PascalCase for struct/enum/trait names: `ScopeId`, `SymbolId`, `FileId`, `Symbol`, `Scope`, `ScopeTree`, `ScopeKind`
- Type aliases spelled out: `pub type ReferencePath = Vec<Symbol>;` (not abbreviated)
- Newtypes common for identity types: `pub struct ScopeId(pub(crate) u32);` wrapping raw u32 for type safety
- Enum variants PascalCase: `Body`, `Block`, `For`, `Catch`, `Switch` (for `ScopeKind`)
- Resolution outcomes capitalized: `Resolved`, `Candidates`, `SchemaObject`, `UnknownSchema`, `StdlibMember`, `Label`, `VisualforcePage`, `Unresolved`

## Code Style

**Formatting:**
- Rustfmt applies automatically via CI (`cargo fmt --all`)
- All code must pass `cargo fmt --all -- --check` in CI
- Default edition: `2021` (workspace-level in `Cargo.toml`)
- Default line width is rustfmt's default (100 characters)

**Linting:**
- Clippy mandatory in CI: `cargo clippy --workspace --all-targets -- -D warnings`
- All lint warnings treated as errors (deny-all approach: `-D warnings`)
- No suppression warnings (`#[allow(...)]`) unless extensively justified in code comment

**Visibility:**
- Public API carefully curated: only `pub` exports appear in `pub use` blocks at module top
- Internal implementation modules private: `mod` (not `pub mod`)
- Internal submodules accessed via crate:: or self:: paths where needed
- Module doc comments (`//!`) at crate and module boundaries explain layer purpose and dependencies

## Import Organization

**Order (top to bottom):**
1. `use crate::...` (local crate modules)
2. `use other_crates::...` (workspace dependencies)
3. `use external::...` (external dependencies like `rowan`, `rayon`, `smol_str`)
4. `use std::...` (standard library) - always last
5. `use super::...` (parent module) - rare, only when needed for sibling access

**Path Aliases:**
- None configured at workspace level; full paths used throughout
- `rowan`, `smol_str`, `rustc_hash` imported explicitly
- No glob imports (`use module::*;`) except in test modules where brevity is acceptable

**Examples:**
- `crates/apex-binder/src/lib.rs` (lines 35-76): Shows complete import structure with crate:: internal imports first, then workspace crates, then external, then std
- `crates/apex-binder/src/completion.rs` (lines 22-32): Local then external organization with detailed imports

## Error Handling

**Strategy:** Infallible parsing with recovery via error collection + best-effort trees

**Patterns:**
- Parser never panics on malformed input: `Parse::errors` is always a `Vec<ParseError>` not a `Result`
- Recovery nodes (`SyntaxKind::ErrorNode`) left in tree where tokens don't match expectations
- `parse.ok()` method: checks `errors.is_empty()` at call sites
- No early returns or `?` operator in parsing: walk completion despite syntax errors
- Binder-level operations that can fail (file I/O, symbol lookup) either panic with clear `expect` messages or use `Option` returning without recovery (file-level failures abort, in-file resolution failures degrade gracefully to `Unresolved`)

**Examples:**
- `crates/apex-parser/src/errors.rs`: `Parse` wraps tree + error vec, not `Result`
- `crates/apex-parser/src/parser.rs` (lines 62-82): `expect()` records error without consuming mismatched token; safe unconditional `bump()` at EOF

**Explicit non-panicking declarations:**
- Module doc comments state "Nothing in this crate panics on malformed *input*" (e.g., `apex-parser/src/errors.rs`)

## Logging

**Framework:** `eprintln!` only (no structured logging framework)

**Patterns:**
- Development/diagnostic output to stderr via `eprintln!`
- Used only for non-silent failures or corpus-related development messages
- Example: corpus verification skipped when real NPSP checkout not present: `eprintln!("skipping: real NPSP corpus checkout not present at {root:?}");`
- No production logging (LSP server uses LSP's own window/logMessage protocol for user-facing diagnostics)

## Comments

**When to Comment:**
- Module-level doc comments (`//!`) are mandatory for every public module: explain purpose, invariants, and relationships
- Function doc comments (`///`) mandatory for `pub fn` at crate boundaries; internal helper functions skip unless complex
- Inline comments only when non-obvious:
  - Algorithm/performance trade-offs (e.g., "linear scan beats hashing, Vec supports mid-walk queries")
  - Intentional limitations or future work (marked with `// BACKLOG:` or similar)
  - Semantic intent that isn't obvious from code (e.g., "positioned at the byte right after the last token actually consumed, not at whatever real content happens to follow the gap")

**Examples:**
- `crates/apex-parser/src/lib.rs` (lines 1-30): Comprehensive module doc covering deep-tree stack safety caveats
- `crates/apex-binder/src/lib.rs` (lines 1-33): Full pipeline explanation with links to specific impl locations
- `crates/apex-binder/src/scope.rs` (lines 34-37): Inline justification for Vec over HashMap: "block-local variable counts are tiny (single digits to low tens), so a linear scan beats hashing"

**JSDoc/TSDoc:**
- Not applicable (Rust codebase, uses `///` doc comment syntax)
- Doc comments follow Rust convention: first line is summary, blank line, then detailed explanation
- Link cross-references with backticks: `` [`SyntaxPtr`] ``, `` [`crate::resolve`] ``

## Function Design

**Size:** No explicit limits; prefer modular helpers over deeply nested closures
- Single-pass tree walks common (scope lookup, symbol collection)
- Speculative parsing via checkpoints in `crate::parser::Checkpoint` (save position, try parse, restore)
- Closure captures kept tight: use `&impl Fn` trait bounds rather than large closure types

**Parameters:**
- Explicit types required (no type inference on public APIs)
- Borrows preferred over ownership for read operations: `fn lookup_local(&self, name: &str) -> Option<SymbolId>`
- Mutable references for side effects: `fn bind(&mut self, scope: ScopeId, name: SmolStr, symbol: SymbolId)`
- Lifetime parameters used when holding references: rare in this codebase (most APIs work with owned IDs/indices)

**Return Values:**
- `Option<T>` for single-item lookups: `resolve_local()`, `lookup_local()`
- `Vec<T>` for collections: `all_resolutions()`, `bindings()`
- Tuples for multiple values: `(SyntaxNode, Vec<Error>)` in `Parse::syntax()` + `Parse::errors`
- Result types rare (error handling via recovery/options rather than Result propagation)

## Module Design

**Exports:**
- Explicit public API in `pub use` blocks at module top
- Example: `crates/apex-binder/src/lib.rs` exports ~40 public types/functions, re-exported at crate root
- Private submodules (e.g., `resolve`, `collect`, `inherit`) stay private; higher-level functions are the entry points

**Barrel Files:**
- Re-export pattern used at crate roots only: `crates/apex-binder/src/lib.rs` re-exports public types from submodules
- Tests don't use barrel imports; each test imports exactly what it needs

**Module Hierarchy:**
- Deep (10+ levels in some paths) but justified by functional organization
- Each layer solves one domain problem: lexer → parser → binder passes (collect/inherit/resolve) → LSP server
- Cross-module dependencies documented at module boundaries

## Type System Patterns

**Newtypes:**
- `ScopeId(u32)`, `FileId(u32)`, `SymbolId { file: FileId, local: u32 }` provide type safety over raw indices
- `(SmolStr, SymbolId)` tuples common for name-binding pairs rather than custom struct
- `TextRange` from `rowan` used for source positions (not raw byte offsets)

**Traits:**
- `AstNode` from `rowan` for syntax tree traversal (implemented by generated `apex_syntax` types)
- Minimal custom traits; mostly composition of existing ones

**Generics:**
- Used sparingly: `generic_type_param_map: FxHashMap<GenericId, Ty>` in binder (not in public API surface)
- `pub(crate) fn remap_symbol_ids(&mut self, f: &impl Fn(SymbolId) -> SymbolId)` uses `impl Fn` trait bounds for simple callbacks

---

*Convention analysis: 2026-09-01*
