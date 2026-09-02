Type: research
Status: claimed

## Question

Investigate how `apex-binder` could incrementally invalidate a whole-project fact that's defined by a method's *body content* rather than its declaration -- the specific gap [ticket 12](12-cross-method-bulkification-decision.md) found blocking cross-method bulkification detection, but stated generally since the same shape of fact would block any future transitive/interprocedural analysis this binder ever wants to add (not just this one diagnostic).

Context: `apex-binder`'s existing three-pass structure (Collect/Inherit/Resolve) and its incremental rebuild (`crates/apex-binder/src/incremental.rs`) both key invalidation on *declarations* -- Pass 2 conservatively reruns for every file whenever any file's declarations change (`crates/apex-binder/src/lib.rs:228-233`'s own documented tradeoff), and a body-only edit inside one file today only reruns Pass 2 for that one file. A transitive "does this callable's body, directly or via any callee, contain DML/SOQL" fact breaks that model: editing a single leaf utility method's body can flip its own bit, which must then propagate to invalidate every transitive caller project-wide -- a reverse-reachability problem with no existing analog in this codebase.

Establish, concretely:
- Is there a real, standard technique for this (e.g. a memoized/incremental call-graph library or algorithm from another language-server implementation, a well-known "salsa"-style incremental-computation pattern, or a simpler bespoke approach specific to this binder's own shape) that keeps the common case (editing one method body) cheap, rather than reverting to a full-project recompute on every keystroke?
- Does `apex_binder::outgoing_calls`/`incoming_calls` (`call_hierarchy.rs`) already carry (or could cheaply carry) enough reverse-edge information to make "who transitively calls this method" a fast, incremental query rather than a fresh whole-project walk each time?
- What's the actual real-world edit-locality pattern worth optimizing for -- do most real edits in a session touch leaf methods with shallow caller fan-in (cheap to invalidate), or is there a realistic worst case (a widely-called low-level utility method) where even a "smart" incremental approach degrades to something close to a full recompute? Sample the real NPSP corpus's own call graph (already surveyed once in ticket 11, branch since merged) for caller fan-in distribution to ground this rather than guessing.
- Would solving this generally (a reusable "transitive fact with incremental invalidation" primitive) versus solving it narrowly (just for the DML/SOQL-taint bit this one diagnostic needs) change the shape of the answer -- is the general version meaningfully harder, or does the narrow version already require solving the same core problem anyway?

This ticket is fact-finding only -- it does not decide the architecture. Its findings would feed a future architecture-decision ticket, should cross-method bulkification (or any other transitive analysis) be reopened.
