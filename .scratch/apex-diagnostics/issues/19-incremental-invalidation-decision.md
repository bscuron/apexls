Type: grilling
Status: resolved
Blocked by: 16

## Question

Given [ticket 16](16-incremental-invalidation-research.md)'s findings, decide whether and how to build incremental invalidation for a transitive, body-content-keyed fact in `apex-binder` -- the prerequisite [ticket 12](12-cross-method-bulkification-decision.md) deferred cross-method bulkification detection on. Resolve, with the user:

- **Salsa-style query framework vs. bespoke worklist propagation.** Ticket 16 found both are the same underlying algorithm (demand-driven pull-based memoization with early cutoff, or its hand-rolled push-based worklist equivalent), but adopting real `salsa` would be a large structural rewrite of `apex-binder`'s rowan-CST/three-pass architecture, while the bespoke worklist variant is smaller and architecture-consistent. Which direction, and how much of `apex-binder`'s existing structure would the chosen approach actually touch?
- **General primitive vs. narrow, DML/SOQL-specific fact.** Ticket 16 found the two require solving the identical core problem -- does that mean build the general "transitive fact with incremental invalidation" primitive now (paying it forward for future transitive analyses), or build only the narrow bit this one diagnostic needs and generalize later if a second consumer ever shows up?
- **Worst-case tolerance.** Real NPSP data: median caller fan-in 1, p90 <=5, but a real worst case of 500 callers (shared test-factory methods). Is a design that degrades toward a fuller recompute for that tail case acceptable, or does the worst case need its own explicit handling?
- **Given an answer above, does cross-method bulkification (ticket 12) get reopened now** with a concrete implementation ticket, or does resolving *this* architecture question stop here, with reopening bulkification left as a separate future decision?

## Answer

**Adopt real `salsa`** (the crate itself, not just its algorithm) as `apex-binder`'s incremental-computation engine, restructuring the binder's incremental model around it -- explicitly confirmed as the intended scope, not the lighter-weight bespoke salsa-style worklist pattern this ticket's own drafting had recommended. This is a materially bigger decision than anything else on this map: it would touch *every* existing feature built on `BoundProgram` (all eight shipped diagnostics, hover, completion, rename, ...), not just the future cross-method bulkification need this ticket was originally scoped to unblock. Confirmed explicitly with the user given that scale mismatch before recording it.

**General primitive, no special-casing the 500-caller tail** (Q2/Q3): both stand as recommended -- a reusable "transitive fact with incremental invalidation" abstraction (not narrowly DML/SOQL-shaped), and no bespoke handling for high-fan-in outlier methods beyond whatever salsa's own early-cutoff already provides.

**Scope decision**: kept inside this map as a large ticket chain, rather than spun off into a separate Wayfinder map -- explicitly chosen despite going beyond this map's original "add diagnostics" destination.

**A real technical question this ticket surfaces that ticket 16 never investigated** (that research covered *what salsa is and why it fits the algorithmic shape*, not *how it would actually integrate with this specific codebase*): `apex-binder`'s own `lib.rs:599-601` documents that `SyntaxNode` (rowan-based) is deliberately neither `Send` nor `Sync`, and the existing three-pass pipeline uses real multi-threading (`rayon`) for Pass 1/Pass 2. Salsa's query-database model and this project's existing parallel-bind architecture haven't been checked for compatibility at all -- a real integration risk, not yet derisked.

**Follow-on:** [Research salsa's concrete integration mechanics with apex-binder](21-salsa-integration-research.md) (`research`, unblocked) -- feeding a future migration-plan ticket once landed. Cross-method bulkification (ticket 12) stays deferred, unblocked only in principle: the whole salsa migration would need to land before it could be built on top of it, not decided or scheduled here.
