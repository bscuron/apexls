Type: grilling
Status: open
Blocked by: 16

## Question

Given [ticket 16](16-incremental-invalidation-research.md)'s findings, decide whether and how to build incremental invalidation for a transitive, body-content-keyed fact in `apex-binder` -- the prerequisite [ticket 12](12-cross-method-bulkification-decision.md) deferred cross-method bulkification detection on. Resolve, with the user:

- **Salsa-style query framework vs. bespoke worklist propagation.** Ticket 16 found both are the same underlying algorithm (demand-driven pull-based memoization with early cutoff, or its hand-rolled push-based worklist equivalent), but adopting real `salsa` would be a large structural rewrite of `apex-binder`'s rowan-CST/three-pass architecture, while the bespoke worklist variant is smaller and architecture-consistent. Which direction, and how much of `apex-binder`'s existing structure would the chosen approach actually touch?
- **General primitive vs. narrow, DML/SOQL-specific fact.** Ticket 16 found the two require solving the identical core problem -- does that mean build the general "transitive fact with incremental invalidation" primitive now (paying it forward for future transitive analyses), or build only the narrow bit this one diagnostic needs and generalize later if a second consumer ever shows up?
- **Worst-case tolerance.** Real NPSP data: median caller fan-in 1, p90 <=5, but a real worst case of 500 callers (shared test-factory methods). Is a design that degrades toward a fuller recompute for that tail case acceptable, or does the worst case need its own explicit handling?
- **Given an answer above, does cross-method bulkification (ticket 12) get reopened now** with a concrete implementation ticket, or does resolving *this* architecture question stop here, with reopening bulkification left as a separate future decision?
