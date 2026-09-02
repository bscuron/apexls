Type: grilling
Status: open
Blocked by: 08

## Question

Given [the research ticket](08-ty-promotion-research.md)'s findings, decide whether and how to promote `apex-binder`'s walker-internal `Ty` type into a real, stored, queryable per-expression type layer on `BoundProgram`. This is the central architectural decision this whole map is named after. Resolve, with the user:

- Is promoting/extending `Ty` worth doing at all given the zero-false-positive bar, or does the research findings suggest the precision gap is too large to close honestly (in which case, what's the honest scope-limited alternative -- e.g. only checking type-mismatches where both sides are already fully resolvable today, without any new inference machinery)?
- If worth doing, what's the minimal viable scope: e.g. just enough for assignment-compatibility and return-type-compatibility checking, vs. also covering real argument-type checking beyond the existing arity-exact/best-effort-narrowed overload resolution?
- How does this fit into the existing three-pass structure (Collect / Inherit / Resolve) -- does it ride entirely inside the existing Pass 2 walk (`resolve.rs`), or does it need its own pass?

This ticket's resolution determines which specific type-checking-flavored diagnostics (currently fog in the map's "Not yet specified" section) become ticketable next, and in what order relative to the non-inference-needed checks already on this map.
