Type: task

## Question

Add the `salsa` crate as a real dependency of `apex-binder` and get a trivial smoke query compiling and passing, to hands-on-verify [ticket 21](21-salsa-integration-research.md)'s documentation-only findings (its `Send`/`Sync` bound analysis, its `#[salsa::input]`/`#[salsa::tracked]` shape) before they become load-bearing assumptions in [the architecture decision ticket](25-salsa-migration-architecture-decision.md) or any later implement ticket.

Concretely:

- Add `salsa` to `crates/apex-binder/Cargo.toml`, pinned to a specific version.
- Write a minimal `#[salsa::input]` / `#[salsa::tracked]` pair in a throwaway test module that exercises the exact owned, `Send + Sync` shapes this codebase already uses elsewhere for exactly this reason (e.g. `SyntaxPtr`/`SmolStr`/`FxHashMap`, per `crates/apex-binder/src/ptr.rs`'s own doc comment and ticket 21's findings) -- not a toy `i32` example that proves nothing about this codebase's actual constraints.
- Confirm it builds and the smoke test passes under `cargo test -p apex-binder`.
- Leave the dependency and smoke module in place (don't revert it) so the architecture ticket and future implement tickets have a real, compiling reference point, not only ticket 21's docs-only citations.

Record the exact `salsa` version pinned and any surprises versus ticket 21's documented API (macro syntax drift, an undocumented bound, anything that didn't match the docs) in the Answer below.

This ticket does not block [the architecture decision ticket](25-salsa-migration-architecture-decision.md) -- that can proceed on ticket 21's research alone -- but its result feeds Stage 1's own future implement ticket once that's created.
