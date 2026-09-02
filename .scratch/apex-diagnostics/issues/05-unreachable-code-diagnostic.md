Type: grilling
Status: resolved

## Question

Design the exact zero-false-positive rule for an "unreachable code after an unconditional `return`/`throw`" diagnostic. The naive "flag any statement after a `return`/`throw` in the same block" rule is not enough by itself -- work through, with the user, the real edge cases:

- Code after a `throw` inside one `if`/`else if` branch when a sibling branch doesn't throw (only the taken branch's tail is actually unreachable, not the whole enclosing block).
- Interaction with `try`/`catch`/`finally` -- is code after a `try` block's own `return` unreachable if `catch` can still run, or if `finally` always runs regardless?
- Interaction with `switch`/`when` statements and their own fallthrough/exhaustiveness rules.
- Loops (`for`/`while`) whose body unconditionally returns/throws on the first iteration -- does code after the loop count as unreachable, or does the loop's own conditional entry make that unsound?

Once the precise rule is settled, this ticket's resolution should also decide whether implementation is folded into this same ticket or split into a follow-on `task`.

## Answer

**v1 rule (narrow slice, fully settled, ready to implement):** A recursive "definitely terminates" predicate, scoped to `Block`/`IfStmt` only:
- `ReturnStmt`/`ThrowStmt`/`BreakStmt`/`ContinueStmt`: always terminates (same textual-unreachability bug shape regardless of which of the four terminates the block, so all four are treated uniformly, not just `return`/`throw` despite this ticket's own title).
- `Block`: scanning its statements in order, the first one that terminates makes every statement after it in that same block unreachable -- and that's both the recursive termination fact *and* the flagging trigger in one pass.
- `IfStmt`: terminates only if it has an `else` branch AND both `then_branch`/`else_branch` terminate (recursively). No `else`, or either branch can fall through -> the `if` itself does not terminate.

Severity: **ERROR** -- unlike bulkification's runtime governor-limit risk, this is a certain, provable defect once flagged (the code genuinely cannot execute), matching `unknown_schema_diagnostics`/`modifier_diagnostics`'s confidence level.

**Deliberately deferred, not attempted in v1:** `TryStmt`/`SwitchStmt`/loop-body reasoning. Real, gnarlier cases (Java-style try/catch/finally definite-completion analysis; `switch`/`when` exhaustiveness requiring a `when else` arm; the `ForStmt`/`ForEachStmt`/`WhileStmt`-vs-`DoWhileStmt` conditional-vs-unconditional-entry asymmetry) -- sharp enough to state precisely (see [the follow-on ticket](14-unreachable-code-try-switch-decision.md)) but not worth the added implementation/false-positive risk for this first slice.

**Follow-on tickets:** [Implement the narrow-slice unreachable-code diagnostic](13-unreachable-code-implement.md) (`task`, ready now, unblocked) and [Design the try/switch/loop extension](14-unreachable-code-try-switch-decision.md) (`grilling`, blocked by 13 -- extending the same "terminates" predicate makes more sense once its base implementation exists).
