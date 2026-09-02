Type: task
Status: open

## Question

Implement the narrow-slice unreachable-code diagnostic exactly as settled in [ticket 05's Answer](05-unreachable-code-diagnostic.md): a new `apexls-server` diagnostic (e.g. `unreachable_code_diagnostics`), `ERROR` severity, built on a recursive "definitely terminates" predicate scoped to `Block`/`IfStmt`:

- `ReturnStmt`/`ThrowStmt`/`BreakStmt`/`ContinueStmt` always terminate.
- A `Block` flags every statement after the first terminating statement in it as unreachable.
- An `IfStmt` terminates only when it has an `else` and both branches (recursively) terminate.

`TryStmt`/`SwitchStmt`/loop-body reasoning is explicitly out of scope for this ticket (see [ticket 14](14-unreachable-code-try-switch-decision.md)) -- a `TryStmt`/`SwitchStmt` node should simply never be treated as terminating for now (the sound, conservative default), and its own nested `Block`s should still be scanned for unreachable code *within* themselves independently.

Definition of done: wired into the merged `publish_diagnostics` alongside the six existing sources; tests covering a bare statement after `return`/`throw`/`break`/`continue`, an `if`/`else` where both branches terminate (flagging code after the whole `if`), an `if` with no `else` or where only one branch terminates (not flagged), a bare (non-block) `if (x) return;` form, nested blocks, and a real-NPSP-corpus zero-false-positive sanity pass (matching tickets 03/10's own practice).
