Type: grilling
Status: open
Blocked by: 06

## Question

Given [the research ticket](06-missing-interface-impl-research.md)'s findings on what `inherit.rs` already resolves, design and implement the "missing interface/abstract-method implementation" diagnostic: a concrete (non-abstract) class or a class implementing an interface must provide every abstract/interface method its supertype chain requires. Work through, with the user, whichever real edge cases the research ticket surfaced as open (e.g. generic interface implementations, multiple-interface method-signature collisions, partial-implementation-via-abstract-subclass chains) before implementing, to hold the zero-false-positive bar.
