Type: grilling
Status: resolved
Blocked by: 13

## Question

Given [the narrow-slice implementation](13-unreachable-code-implement.md) in hand, decide whether and how to extend the "definitely terminates" predicate to `TryStmt` and `SwitchStmt`, and to loop bodies. Resolve, with the user:

- **`TryStmt`**: is the one simple, high-confidence case worth adding -- a `finally` clause that itself unconditionally terminates (sound on its own, since `finally` always runs regardless of what `try`/`catch` do) -- or is even that not worth the complexity given how rare a terminating `finally` is in real code? Full Java-style definite-completion analysis (try body terminates AND every catch clause terminates, when no finally overrides it) is a much bigger, genuinely gnarly undertaking -- confirm whether it's worth attempting at all, or permanently out of scope.
- **`SwitchStmt`**: worth requiring a `when else` arm (true exhaustiveness) plus every arm terminating? Real Apex `switch on` usage patterns should inform whether this is common enough to be worth it.
- **Loops**: `DoWhileStmt` terminating iff its body terminates is sound (body always runs at least once) and low-risk to add; `ForStmt`/`ForEachStmt`/`WhileStmt` can never soundly be treated as terminating without real value-range/dataflow analysis this binder doesn't have -- confirm this stays permanently out of scope rather than attempted.

This ticket's resolution determines whether a follow-on implementation ticket gets created, and for which of the three shapes.

## Answer

Build all three, despite near-zero real-world frequency in the NPSP reference corpus (zero `switch` statements, zero terminating-`finally` blocks in the only 3 files that use `finally` at all, only 2 `do-while` loops project-wide): each is individually sound (no false-positive risk introduced by any of them), and completeness matters for other real Apex codebases that may lean on these constructs more than NPSP does. Since the corpus can't supply real validation examples for `finally`/`switch`, the follow-on implementation ticket's tests are hand-authored fixtures rather than real-corpus spot-checks -- an explicit, accepted departure from every other check on this map, which all had real corpus confirmation.

Exact rules to add to the existing `stmt_terminates` predicate:
- `TryStmt`: terminates iff it has a `finally_clause()` whose own block terminates (recursively, via the existing `Block` rule) -- regardless of what the `try` body or any `catch` clause does, since `finally` always runs. No attempt at full try/catch definite-completion analysis.
- `SwitchStmt`: terminates iff at least one `when_clause` has `WhenValue::is_else()` (a `when else` arm is present, i.e. genuinely exhaustive) AND every `when_clause`'s own `body()` terminates.
- `DoWhileStmt`: terminates iff its `body()` terminates (sound, since a `do`-`while` body unconditionally executes at least once, unlike `ForStmt`/`ForEachStmt`/`WhileStmt`'s conditional entry, which stays permanently out of scope).

**Follow-on ticket:** [Implement the try/switch/do-while unreachable-code extensions](15-unreachable-code-try-switch-implement.md) (`task`, unblocked).
