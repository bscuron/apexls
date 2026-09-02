Type: grilling
Status: resolved
Blocked by: 06

## Question

Given [the research ticket](06-missing-interface-impl-research.md)'s findings on what `inherit.rs` already resolves, design and implement the "missing interface/abstract-method implementation" diagnostic: a concrete (non-abstract) class or a class implementing an interface must provide every abstract/interface method its supertype chain requires. Work through, with the user, whichever real edge cases the research ticket surfaced as open (e.g. generic interface implementations, multiple-interface method-signature collisions, partial-implementation-via-abstract-subclass chains) before implementing, to hold the zero-false-positive bar.

## Answer

**Scope: project-local-only.** Interfaces/abstract classes declared in this project, using only `inherit.rs`'s existing `inherited_chain` -- zero `apex-binder`-core changes needed, and safely zero-false-positive as-is (an unresolved name only ever under-reports, never false-positives, since the check only ever asserts requirements it actually found in the chain). Mirrors how bulkification and unreachable-code both shipped a tractable slice before touching harder ground. Stdlib-interface coverage (the arguably more common real-world case -- `Database.Batchable`, `Comparable`, `Queueable`, ...) is real and valuable but needs genuinely new `apex-binder`-core capability (a stdlib cross-reference lookup, plus recording every failed `implements`/chain-hop resolution, not just the existing single-direct-`extends` signal) -- split into its own ticket pair rather than folded in here.

**Satisfaction rules, confirmed against this project's own existing test fixtures (`dynamic_dispatch_resolution.rs`), not guessed:**
- An interface-declared method is satisfied by any same-name (case-insensitive)/same-arity method anywhere at or below that point in the chain -- Apex never requires (or usually writes) `override` for interface satisfaction.
- An abstract-superclass method is satisfied only by one explicitly carrying `is_override` -- matching Apex's real `extends`-overriding rule, which does require the keyword.

**Follow-on tickets:** [Implement the project-local-only missing-implementation diagnostic](17-missing-interface-impl-implement-task.md) (`task`, unblocked) and [Research the stdlib-interface extension](18-stdlib-interface-extension-research.md) (`research`, unblocked) -- ticketed now rather than left as fog, since the shape is already sharp per ticket 06's own recommendation, even though nobody's decided to build it yet.
