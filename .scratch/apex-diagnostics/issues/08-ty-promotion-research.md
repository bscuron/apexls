Type: research
Status: claimed

## Question

Survey every call site and usage pattern of `crate::ty::Ty` (`crates/apex-binder/src/ty.rs`) across `resolve.rs`, `generics.rs`, and `conversions.rs` to establish, precisely:

- What per-expression type information is already computed during Pass 2 body walking but currently discarded once the walker moves past that expression (never stored on `BoundProgram`, never exposed outside the walker)?
- What are `Ty`'s current precision gaps for a stored/queryable use case -- e.g. does it distinguish a system type's own possible values precisely enough for real type-mismatch checking, or is `Ty::System { name, args }`'s "not project-local, but at least named" level of detail (per its own doc comment) too coarse?
- Survey a sample of real Apex code from the NPSP corpus for the specific patterns a type-mismatch/wrong-argument-type diagnostic would need to handle correctly to hit zero false positives: implicit numeric widening, `Object`-typed parameters/variables, `List`/`Set`/`Map` generic substitution (already handled by `generics.rs` for some cases), SObject dynamic typing, and any other real-world pattern that would trip up a naive nominal-type-equality check.
- What would the minimal viable promotion of `Ty` into a stored, queryable per-expression type layer look like, structurally (e.g. a new field on `BoundProgram`, keyed how) -- without committing to build it; just scope what "minimal" could mean as input to the architecture decision.

This ticket is fact-finding only -- it does not decide the architecture. Its findings feed [Decide the Ty-promotion / type-inference-layer architecture](09-ty-promotion-architecture-decision.md).
