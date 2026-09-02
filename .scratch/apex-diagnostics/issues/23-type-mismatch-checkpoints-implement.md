Type: task
Status: open

## Question

Implement the three type-mismatch checkpoints settled in [ticket 09's Answer](09-ty-promotion-architecture-decision.md), `ERROR` severity, Option B (inline, no general stored `Ty` layer):

- **`Stmt::LocalVarDecl`**: for each declarator with both a declared type and an initializer expression, capture the declared `Ty` (`self.resolve_type_ref(&ty)`) and the initializer's inferred `Ty` (`self.bind_expr(scope, &e)`) and check `conversions::type_compatible`. Also fix the identical discard in the `for`-loop init-declarator arm (`resolve.rs:1649-1671`), the same shape.
- **`Stmt::Return`**: capture the returned expression's `Ty` and compare it against the enclosing method's own declared return type (`self.enclosing_member`/`self.type_of_symbol`).
- **Non-overloaded call arguments**: fix `narrow_by_overload`'s `pool.len() == 1` fast path (`resolve.rs:142-144`) to still run `is_argument_type_compatible`/`type_compatible` against the single candidate for diagnostic purposes (recording a mismatch, not changing the `Resolution` itself -- the fast path's existing resolution behavior must stay unchanged, only a new side-channel finding gets added).

Route every comparison through the existing `conversions::type_compatible`/`is_argument_type_compatible`, never a new nominal-equality check -- this is what inherits the zero-false-positive correctness ticket 08's own NPSP survey confirmed those functions already have. Record each finding as a `(SyntaxPtr, message)` (or emit the `Diagnostic` directly from within `apex-binder`'s own walk, whichever shape fits the existing `Resolution`-recording plumbing better) rather than persisting `Ty` anywhere -- per Option B, "promote the check, not the type."

Definition of done: wired into the merged `publish_diagnostics` alongside the eight existing sources; tests covering each of ticket 08's six real NPSP patterns explicitly as **negative** tests (implicit numeric widening, `Object`-typed dynamic-SObject-access idioms, casts recovering a concrete type, `List<Concrete>`-into-`List<SObject>` covariance with no cast, `Id`/`String` bidirectional compatibility, SObject-to-SObject widening) -- none of these six may ever be flagged; plus positive tests for a genuine mismatch at each of the three checkpoints; a real-NPSP-corpus zero-false-positive sanity pass (matching every other diagnostic on this map, and especially load-bearing here given how many real-world exceptions to naive type equality ticket 08 catalogued).
