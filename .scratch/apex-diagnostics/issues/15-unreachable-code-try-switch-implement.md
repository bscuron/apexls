Type: task
Status: open

## Question

Implement the three extensions to `stmt_terminates` settled in [ticket 14's Answer](14-unreachable-code-try-switch-decision.md):

- `TryStmt` terminates iff its `finally_clause()`'s own block terminates (regardless of the `try` body/`catch` clauses).
- `SwitchStmt` terminates iff it has a `when else` arm (`WhenValue::is_else()`) AND every `when_clause`'s `body()` terminates.
- `DoWhileStmt` terminates iff its `body()` terminates.

Definition of done: each new rule wired into the existing `stmt_terminates` predicate in `crates/apexls-server/src/capabilities.rs`; hand-authored tests for each (the NPSP corpus has no real examples of any of the three, per ticket 14's own findings, so this departs from every other diagnostic ticket's real-corpus-confirmation practice -- explicit, accepted): a `finally` that unconditionally returns/throws making code after the whole `try` unreachable, a `finally` that does *not* terminate leaving it reachable, a `try` with no `finally` at all still never terminating (unchanged from ticket 13), a `switch` with a `when else` where every arm terminates, a `switch` missing `when else` (or where some arm falls through) not terminating, and a `do`-`while` whose body unconditionally terminates vs. one that doesn't.
