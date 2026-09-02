Type: task
Status: open

## Question

Fix the nested-interface resolution gap found by [ticket 18](18-stdlib-interface-extension-research.md)'s research: a class that `implements` an interface declared as its *own* nested member currently fails to resolve, because `inherit.rs::resolve_inheritance` calls `resolve_dotted_name_from(name, table.get(type_id).container)` -- for a top-level class, `container` is `None`, so the class's own nested types are never consulted, only its container's (nothing, for a top-level type).

Real, confirmed NPSP evidence (3 distinct interface names, 5 implementer sites): `fflib_Inheritor implements IA, IB, IC` (`tests/corpus/npsp/force-app/infrastructure/apex-mocks/main/classes/fflib_Inheritor.cls:5-9`, where `IA`/`IB`/`IC` are nested interfaces declared inside `fflib_Inheritor` itself), `fflib_Criteria implements Evaluator`, `fflib_MyList implements IList` (same self-referencing-own-nested-interface shape).

Fix: teach `resolve_dotted_name_from` (or a caller-side check in `resolve_inheritance` before falling back to the container-based lookup) to also try the type's own nested types when resolving one of its own `extends`/`implements` names, not just its container's nested types.

Definition of done: a fix in `crates/apex-binder/src/inherit.rs`/`symbol_table.rs`; tests covering a class implementing its own nested interface (mirroring this project's existing `extends_chain_resolution.rs` conventions -- this is ordinary binder resolution, directly testable, not subject to the stdlib-interface diagnostic's own unvalidatable-by-corpus limitation); confirm the fix doesn't regress `resolution_regression_baseline.rs`'s pinned counts (should only improve them, since these 5 real NPSP sites currently resolve incorrectly).
