Type: grilling
Status: open
Blocked by: 13

## Question

Given [the narrow-slice implementation](13-unreachable-code-implement.md) in hand, decide whether and how to extend the "definitely terminates" predicate to `TryStmt` and `SwitchStmt`, and to loop bodies. Resolve, with the user:

- **`TryStmt`**: is the one simple, high-confidence case worth adding -- a `finally` clause that itself unconditionally terminates (sound on its own, since `finally` always runs regardless of what `try`/`catch` do) -- or is even that not worth the complexity given how rare a terminating `finally` is in real code? Full Java-style definite-completion analysis (try body terminates AND every catch clause terminates, when no finally overrides it) is a much bigger, genuinely gnarly undertaking -- confirm whether it's worth attempting at all, or permanently out of scope.
- **`SwitchStmt`**: worth requiring a `when else` arm (true exhaustiveness) plus every arm terminating? Real Apex `switch on` usage patterns should inform whether this is common enough to be worth it.
- **Loops**: `DoWhileStmt` terminating iff its body terminates is sound (body always runs at least once) and low-risk to add; `ForStmt`/`ForEachStmt`/`WhileStmt` can never soundly be treated as terminating without real value-range/dataflow analysis this binder doesn't have -- confirm this stays permanently out of scope rather than attempted.

This ticket's resolution determines whether a follow-on implementation ticket gets created, and for which of the three shapes.
