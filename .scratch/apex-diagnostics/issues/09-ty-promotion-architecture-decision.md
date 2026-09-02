Type: grilling
Status: resolved
Blocked by: 08, 21

## Question

Given [the research ticket](08-ty-promotion-research.md)'s findings, decide whether and how to promote `apex-binder`'s walker-internal `Ty` type into a real, stored, queryable per-expression type layer on `BoundProgram`. This is the central architectural decision this whole map is named after. Resolve, with the user:

- Is promoting/extending `Ty` worth doing at all given the zero-false-positive bar, or does the research findings suggest the precision gap is too large to close honestly (in which case, what's the honest scope-limited alternative -- e.g. only checking type-mismatches where both sides are already fully resolvable today, without any new inference machinery)?
- If worth doing, what's the minimal viable scope: e.g. just enough for assignment-compatibility and return-type-compatibility checking, vs. also covering real argument-type checking beyond the existing arity-exact/best-effort-narrowed overload resolution?
- How does this fit into the existing three-pass structure (Collect / Inherit / Resolve) -- does it ride entirely inside the existing Pass 2 walk (`resolve.rs`), or does it need its own pass?

This ticket's resolution determines which specific type-checking-flavored diagnostics (currently fog in the map's "Not yet specified" section) become ticketable next, and in what order relative to the non-inference-needed checks already on this map.

**Newly blocked on [ticket 21](21-salsa-integration-research.md) as well as ticket 08**: ticket 19 decided `apex-binder` should adopt real `salsa` as its incremental engine. Promoting `Ty` into a stored, queryable layer is exactly the shape of new persistent fact that decision affects -- deciding this ticket's storage/invalidation architecture before knowing salsa's actual integration constraints (`Send`/`Sync`, tracked-function shape) risks designing something that has to be redone once ticket 21 lands.

## Answer

**Worth building.** The precision gaps ticket 08 found are all addressable by routing through the existing, org-verified `conversions.rs` rather than reinventing type-compatibility rules -- the risk that would have justified caution doesn't actually apply.

**Option B** (checkpoint-only, inline, no general stored `Ty` layer) over Option A (a general `SyntaxPtr`-keyed `Ty` map, every expression) or Option C (a separate duplicate walk). Ticket 21's finding substantially defused the original reason this ticket was blocked on it (a `SyntaxPtr`-keyed map is already the established, salsa-compatible shape this codebase uses everywhere, so building it the ordinary way now doesn't create real redo risk) -- but that finding argues Option A is *safe* to build, not that it's *needed*. Option B directly targets the three highest-value real bug shapes without paying storage cost for a capability (e.g. hover-shows-inferred-type) nothing has asked for yet, matching this map's own established pattern of shipping the smallest slice that delivers real diagnostic value.

**Required companion fix, not optional scope**: `narrow_by_overload`'s `pool.len() == 1` short-circuit must still run `type_compatible`/`is_argument_type_compatible` for diagnostic purposes even when arity alone already resolved the call -- without this, the argument-type checkpoint has nothing real to check, since it's the single biggest real gap ticket 08 found (a wrong-typed argument to a non-overloaded method silently resolves today).

**Severity: `ERROR`** -- a type mismatch at any of the three checkpoints (declared-vs-initializer, declared-return-vs-returned-expression, non-overloaded-call-argument) is a genuine compile-time defect in real Apex, matching every other structural-defect check on this map, not a runtime risk like bulkification.

**Follow-on:** [Implement the three type-mismatch checkpoints](23-type-mismatch-checkpoints-implement.md) (`task`, unblocked) -- one ticket covering all three, since they share the identical underlying mechanism (capture two `Ty`s inline during the existing Pass 2 walk, call `conversions::type_compatible`, emit `ERROR` on positive incompatibility).
