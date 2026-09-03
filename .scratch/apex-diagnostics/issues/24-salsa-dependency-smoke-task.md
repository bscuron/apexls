Type: task
Status: resolved

## Question

Add the `salsa` crate as a real dependency of `apex-binder` and get a trivial smoke query compiling and passing, to hands-on-verify [ticket 21](21-salsa-integration-research.md)'s documentation-only findings (its `Send`/`Sync` bound analysis, its `#[salsa::input]`/`#[salsa::tracked]` shape) before they become load-bearing assumptions in [the architecture decision ticket](25-salsa-migration-architecture-decision.md) or any later implement ticket.

Concretely:

- Add `salsa` to `crates/apex-binder/Cargo.toml`, pinned to a specific version.
- Write a minimal `#[salsa::input]` / `#[salsa::tracked]` pair in a throwaway test module that exercises the exact owned, `Send + Sync` shapes this codebase already uses elsewhere for exactly this reason (e.g. `SyntaxPtr`/`SmolStr`/`FxHashMap`, per `crates/apex-binder/src/ptr.rs`'s own doc comment and ticket 21's findings) -- not a toy `i32` example that proves nothing about this codebase's actual constraints.
- Confirm it builds and the smoke test passes under `cargo test -p apex-binder`.
- Leave the dependency and smoke module in place (don't revert it) so the architecture ticket and future implement tickets have a real, compiling reference point, not only ticket 21's docs-only citations.

Record the exact `salsa` version pinned and any surprises versus ticket 21's documented API (macro syntax drift, an undocumented bound, anything that didn't match the docs) in the Answer below.

This ticket does not block [the architecture decision ticket](25-salsa-migration-architecture-decision.md) -- that can proceed on ticket 21's research alone -- but its result feeds Stage 1's own future implement ticket once that's created.

## Answer

Pinned `salsa = "0.28.2"` (crates.io, current latest at the time of this ticket) in `crates/apex-binder/Cargo.toml`'s `[dependencies]` -- a real, non-dev dependency, since Stage 1's eventual implement ticket will need it load-bearing, not just for tests.

Added `crates/apex-binder/src/salsa_smoke.rs` (registered via `#[cfg(test)] mod salsa_smoke;` in `lib.rs`, between `resolve` and `schema_index`), containing:

- A minimal concrete database (`#[salsa::db] #[derive(Default)] struct SmokeDatabase { storage: salsa::Storage<Self> }` + `#[salsa::db] impl salsa::Database for SmokeDatabase {}`), matching the canonical boilerplate from salsa's own `examples/calc/db.rs`.
- `#[salsa::input] struct SourceFile { #[returns(deref)] text: SmolStr }` -- one file's source text as a salsa input.
- `#[salsa::tracked] fn method_pointers(db: &dyn salsa::Database, file: SourceFile) -> FxHashMap<SmolStr, SyntaxPtr>`, a stand-in for `crate::collect`'s real Pass 1 work: parses the file's text with the real `apex_parser::parse_compilation_unit`, walks the real `ClassDecl`/`ClassBody`/`Member::Method` AST, and returns every method name mapped to a real `crate::ptr::SyntaxPtr` built via `SyntaxPtr::new(FileId(0), method.syntax())` -- not a toy `i32`/`String` return.
- One test that builds the database, runs the tracked query, re-parses the same source independently to get a *fresh* `SyntaxNode` root (a separate, still-`!Send`/`!Sync` live tree), and calls `ptr.to_node(&root)` on every salsa-memoized pointer to confirm it re-resolves to the right `MethodDecl` -- exercising the exact "owned pointer in salsa, live node only at the edge" round-trip ticket 21 concluded would work. Also asserts a second call to the tracked fn with the same input returns an equal (memoized) result.

`cargo test -p apex-binder salsa_smoke` (and the full `cargo test -p apex-binder`, 94 lib tests + all integration suites) passes; `cargo check --workspace` stays clean.

**Versus ticket 21's documented API**: no real surprises. Two minor mechanical details ticket 21's docs-only research didn't need to spell out, both discovered by just compiling: a `#[salsa::tracked]` fn's return type defaults to a *borrowed* memoized value (`&T`, not `T`) unless `returns(copy)`/`returns(clone)` is specified -- iterating a `for (k, v) in methods` binding (already `&FxHashMap`) needed the `&methods` I first wrote removed, not added. And `#[salsa::input]` struct fields need an explicit `#[returns(deref)]` (or `ref`/`clone`/`copy`) attribute to avoid an always-cloning default accessor, mirrored from salsa's own `examples/calc/ir.rs`. Neither contradicts ticket 21's `Send`/`Sync` analysis, which held exactly as predicted -- the live `SyntaxNode` built inside `method_pointers`' body never needed to cross a salsa boundary, and `SyntaxPtr`/`SmolStr`/`FxHashMap` needed no changes to satisfy salsa's macro-enforced bounds.
