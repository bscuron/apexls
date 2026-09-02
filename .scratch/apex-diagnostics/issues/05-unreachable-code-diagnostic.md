Type: grilling
Status: open

## Question

Design the exact zero-false-positive rule for an "unreachable code after an unconditional `return`/`throw`" diagnostic. The naive "flag any statement after a `return`/`throw` in the same block" rule is not enough by itself -- work through, with the user, the real edge cases:

- Code after a `throw` inside one `if`/`else if` branch when a sibling branch doesn't throw (only the taken branch's tail is actually unreachable, not the whole enclosing block).
- Interaction with `try`/`catch`/`finally` -- is code after a `try` block's own `return` unreachable if `catch` can still run, or if `finally` always runs regardless?
- Interaction with `switch`/`when` statements and their own fallthrough/exhaustiveness rules.
- Loops (`for`/`while`) whose body unconditionally returns/throws on the first iteration -- does code after the loop count as unreachable, or does the loop's own conditional entry make that unsound?

Once the precise rule is settled, this ticket's resolution should also decide whether implementation is folded into this same ticket or split into a follow-on `task`.
