Type: task
Status: resolved

## Question

Implement the narrow-slice unreachable-code diagnostic exactly as settled in [ticket 05's Answer](05-unreachable-code-diagnostic.md): a new `apexls-server` diagnostic (e.g. `unreachable_code_diagnostics`), `ERROR` severity, built on a recursive "definitely terminates" predicate scoped to `Block`/`IfStmt`:

- `ReturnStmt`/`ThrowStmt`/`BreakStmt`/`ContinueStmt` always terminate.
- A `Block` flags every statement after the first terminating statement in it as unreachable.
- An `IfStmt` terminates only when it has an `else` and both branches (recursively) terminate.

`TryStmt`/`SwitchStmt`/loop-body reasoning is explicitly out of scope for this ticket (see [ticket 14](14-unreachable-code-try-switch-decision.md)) -- a `TryStmt`/`SwitchStmt` node should simply never be treated as terminating for now (the sound, conservative default), and its own nested `Block`s should still be scanned for unreachable code *within* themselves independently.

Definition of done: wired into the merged `publish_diagnostics` alongside the six existing sources; tests covering a bare statement after `return`/`throw`/`break`/`continue`, an `if`/`else` where both branches terminate (flagging code after the whole `if`), an `if` with no `else` or where only one branch terminates (not flagged), a bare (non-block) `if (x) return;` form, nested blocks, and a real-NPSP-corpus zero-false-positive sanity pass (matching tickets 03/10's own practice).

## Answer

Implemented as `apexls_server::capabilities::unreachable_code_diagnostics`, wired into the merged `publish_diagnostics` alongside the six existing sources, built on a `stmt_terminates` recursive predicate exactly matching ticket 05's settled scope: `Return`/`Throw`/`Break`/`Continue` always terminate; a `Block` terminates if any statement in it does; an `IfStmt` terminates only with an `else` where both branches (recursively) terminate; everything else (`TryStmt`, `SwitchStmt`, every loop kind) conservatively never terminates.

The implementation walks every `Block` node in the file independently (not just top-level method bodies), so a block nested inside a non-terminating container (`try`, `switch`, a loop) still gets its own unreachable code found, while that container itself correctly never makes code *after* it unreachable -- confirmed by a dedicated test pair (`a_try_blocks_own_return_does_not_make_code_after_the_try_unreachable` / `unreachable_code_inside_a_try_block_is_still_reported`).

Tests: `crates/apexls-server/tests/unreachable_code_diagnostics.rs` -- 11 cases covering each terminator kind, the if/else-both-terminate case (including the bare non-block `if (x) return; else return;` form), the two negative if/else cases (no `else`; only one branch terminates), the try-block boundary pair above, and a clean-code negative test. All pass. Zero-false-positive sanity check against the **entire** real NPSP corpus (1,035 `.cls` files, not just a sample) via a throwaway example harness (built, run, deleted -- not committed): zero unreachable-statement diagnostics across the whole corpus. Full `apexls-server` suite passes.
