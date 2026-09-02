Type: grilling
Status: resolved
Blocked by: 11

## Question

Given [the research ticket](11-cross-method-bulkification-research.md)'s findings, decide whether and how to extend the DML/SOQL-in-loop diagnostic (already shipped for the direct, same-loop-body case via [ticket 10](10-dml-soql-in-loop-implement.md)) across method boundaries. Resolve, with the user:

- Is the real-world payoff (how many additional true positives one-hop or full-transitive tracing would catch on real code, per the research findings) worth the new call-graph infrastructure, or does the direct-only rule already cover enough of the real pattern that this isn't worth building?
- If worth building, one-hop only or full transitive -- the research findings on real call-chain depth should settle this concretely rather than guessing.
- The virtual/interface dispatch ambiguity policy (any-candidate-tainted vs. every-candidate-tainted vs. excluded entirely) -- this directly trades recall against this project's zero-false-positive discipline, so it needs to be decided deliberately, not defaulted.
- Whether this becomes a new pass in `apex-binder`'s three-pass structure or a lazily-computed query, per the research findings' cost estimate.

This ticket's resolution determines whether a follow-on implementation ticket gets created, and in what shape.

## Answer

**Defer.** The real payoff ticket 11 found (226 net-new flaggable loops, 7.3% of the NPSP corpus) doesn't clear the bar against the real cost: not just a new Pass 3, but an *unsolved* incremental-invalidation design problem -- a method-body edit must invalidate the transitive "contains DML/SOQL" fact for every caller project-wide, a reverse-reachability propagation `apex-binder`'s existing per-file dirty-tracking has no analog for. This is `WARNING` severity (a real risk, not a certain defect, unlike this project's `ERROR`-tier checks), which lowers the cost of shipping the direct-only rule (ticket 10) as the complete v1 feature rather than building on top of an admittedly shaky incremental story. Every other check on this map shipped only once its design was fully sound; this is the first one where the honest answer is "the incremental-rebuild story doesn't work yet," and building on that risks either stale taint after edits or an unacceptable full-project recompute on every keystroke.

The transitive-depth (full transitive, not one-hop -- one-hop alone misses 62.5% of the real cases) and `Resolution::Candidates` policy questions ticket 11 already gathered strong, real-data-backed answers for are **not decided now** -- deciding them today would be speculative design work for a feature with no committed timeline. They're preserved in ticket 11's own Answer as context for whenever this gets reopened.

**Follow-on:** [Research incremental invalidation for a transitive, body-content-keyed fact](16-incremental-invalidation-research.md) (`research`, unblocked) -- a real, sharp, statable problem even though nobody has solved it yet. Resolving it is the prerequisite for ever reopening cross-method bulkification as a real architecture decision again.
