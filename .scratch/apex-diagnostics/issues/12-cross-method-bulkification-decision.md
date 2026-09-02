Type: grilling
Status: open
Blocked by: 11

## Question

Given [the research ticket](11-cross-method-bulkification-research.md)'s findings, decide whether and how to extend the DML/SOQL-in-loop diagnostic (already shipped for the direct, same-loop-body case via [ticket 10](10-dml-soql-in-loop-implement.md)) across method boundaries. Resolve, with the user:

- Is the real-world payoff (how many additional true positives one-hop or full-transitive tracing would catch on real code, per the research findings) worth the new call-graph infrastructure, or does the direct-only rule already cover enough of the real pattern that this isn't worth building?
- If worth building, one-hop only or full transitive -- the research findings on real call-chain depth should settle this concretely rather than guessing.
- The virtual/interface dispatch ambiguity policy (any-candidate-tainted vs. every-candidate-tainted vs. excluded entirely) -- this directly trades recall against this project's zero-false-positive discipline, so it needs to be decided deliberately, not defaulted.
- Whether this becomes a new pass in `apex-binder`'s three-pass structure or a lazily-computed query, per the research findings' cost estimate.

This ticket's resolution determines whether a follow-on implementation ticket gets created, and in what shape.
