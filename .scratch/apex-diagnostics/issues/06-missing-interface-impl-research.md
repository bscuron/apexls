Type: research
Status: claimed

## Question

Survey exactly what `crates/apex-binder/src/inherit.rs`'s existing `extends`/`implements` chain resolution already gives you toward a "does this concrete class implement everything its supertype chain requires" check, versus what's genuinely missing. Specifically establish:

- Does `inherit.rs` already expose, per project-local class, a fully-resolved flat list of every abstract method/interface method it's obligated to implement (walking through multiple abstract superclasses and multiple implemented interfaces)?
- How are interface default/static methods (if Apex interfaces support anything like them) and generic interface implementations (`implements Comparable<Foo>`-shaped) handled today, if at all?
- What happens today, concretely, for a real concrete class that fails to implement a required abstract/interface method -- does anything already detect this (e.g. as a byproduct of some other resolution), or does it silently pass through?
- Does resolution stop cleanly (as `Unresolved`/some existing signal) when a supertype or interface itself can't be resolved, so the new check can safely skip those cases without guessing (matching this project's zero-false-positive bar)?

This ticket is fact-finding only -- it does not implement anything. Its answer determines [Implement the missing-interface/abstract-method-implementation diagnostic](07-missing-interface-impl-implement.md)'s real scope and edge cases.
