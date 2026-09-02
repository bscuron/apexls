Type: grilling
Status: open

## Question

Design the exact zero-false-positive rule for a DML-or-SOQL-inside-a-loop (governor-limit bulkification anti-pattern) diagnostic. "Textually inside a `for`/`while`/`do` loop body" alone is not precise enough to ship without false positives -- work through, with the user, which shapes should and shouldn't fire, e.g.:

- A loop provably guaranteed to run at most once (unusual, but does it need an exemption or is that not a real pattern worth guarding)?
- A query/DML statement inside a loop that's already querying/writing a bulk collection rather than a single record per iteration -- can this binder tell the difference, or does *any* DML/SOQL textually inside a loop body count regardless?
- Nested loops, loops inside a method called from a loop (cross-method bulkification -- almost certainly out of reach without interprocedural analysis; confirm it's out of scope for v1 rather than silently assumed).
- `Database.query`/dynamic SOQL vs. static SOQL -- same rule, or does dynamic SOQL need separate handling?

Once the precise rule is settled, this ticket's resolution should also decide whether implementation is folded into this same ticket or split into a follow-on `task`.
