Type: task
Status: resolved

## Question

Implement the three extensions to `stmt_terminates` settled in [ticket 14's Answer](14-unreachable-code-try-switch-decision.md):

- `TryStmt` terminates iff its `finally_clause()`'s own block terminates (regardless of the `try` body/`catch` clauses).
- `SwitchStmt` terminates iff it has a `when else` arm (`WhenValue::is_else()`) AND every `when_clause`'s `body()` terminates.
- `DoWhileStmt` terminates iff its `body()` terminates.

Definition of done: each new rule wired into the existing `stmt_terminates` predicate in `crates/apexls-server/src/capabilities.rs`; hand-authored tests for each (the NPSP corpus has no real examples of any of the three, per ticket 14's own findings, so this departs from every other diagnostic ticket's real-corpus-confirmation practice -- explicit, accepted): a `finally` that unconditionally returns/throws making code after the whole `try` unreachable, a `finally` that does *not* terminate leaving it reachable, a `try` with no `finally` at all still never terminating (unchanged from ticket 13), a `switch` with a `when else` where every arm terminates, a `switch` missing `when else` (or where some arm falls through) not terminating, and a `do`-`while` whose body unconditionally terminates vs. one that doesn't.

## Answer

Implemented directly in `stmt_terminates` (`crates/apexls-server/src/capabilities.rs`), exactly per ticket 14's settled rules: `Stmt::Try` terminates iff `finally_clause().body()` terminates; `Stmt::Switch` terminates iff a `when else` arm is present (`WhenValue::is_else()`) AND every arm's `body()` terminates; `Stmt::DoWhile` terminates iff its `body()` terminates. Factored a small `block_terminates(&Block) -> bool` helper (`block.statements().any(stmt_terminates)`) since all three new cases -- plus the existing `Stmt::Block` arm -- reduce to "does this `Option<Block>` terminate," and `FinallyClause`/`WhenClause`/`DoWhileStmt` all expose their body as `Option<Block>`, not `Option<Stmt>`.

Tests: 9 new cases appended to `crates/apexls-server/tests/unreachable_code_diagnostics.rs` -- a terminating and a non-terminating `finally`; an exhaustive all-terminating `switch`, one missing `when else`, and one where an arm falls through; a terminating and a non-terminating `do`-`while`; plus a negative test confirming a `foreach` loop whose body always returns still does *not* make following code unreachable (conditional entry stays unsound regardless of loop kind, unchanged from ticket 13's own scope). All 19 tests in the file pass. As anticipated by ticket 14, the NPSP corpus itself supplied zero real confirming examples for any of the three (still zero `switch`/terminating-`finally`/`do`-`while` occurrences), so confidence here rests on the hand-authored tests plus a full-corpus re-run confirming zero *new* false positives (still 0 unreachable-statement diagnostics across all 1,035 files). Full `apexls-server` suite passes.
