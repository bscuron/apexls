Type: task
Status: open

## Question

Implement the project-local-only missing-interface/abstract-method-implementation diagnostic exactly as settled in [ticket 07's Answer](07-missing-interface-impl-implement.md):

- For every concrete (non-abstract) project-local class, walk `inherited_chain(class_id)` and, for each ancestor, collect every method that's "required" (`is_abstract || container.kind == SymbolKind::Interface`, per [ticket 06](06-missing-interface-impl-research.md)'s own finding that interface methods are implicitly abstract without the modifier bit ever being set).
- For each required method, check whether it's satisfied:
  - Declared on an ancestor `Interface`: satisfied by any project-local method at or below that point in the chain with the same name (case-insensitive) and arity, regardless of `override`.
  - Declared on an ancestor abstract `Class`: satisfied only by one carrying `modifiers.is_override == true`.
- A required method with no satisfying override anywhere in the chain -> flag the concrete class itself (not the ancestor) with a diagnostic naming the missing method and which ancestor requires it.
- Method-level dedup across multiple interfaces requiring the same name+arity signature (e.g. two interfaces both requiring `void run()`) should fall out naturally from keying "still missing" by name+arity, not per-ancestor -- satisfying it once satisfies both.

Severity: this is a certain, provable defect once flagged (a concrete class genuinely fails to satisfy its declared contract) -- `ERROR`, matching `unknown_schema_diagnostics`/`modifier_diagnostics`/`unreachable_code_diagnostics`'s confidence level, not `WARNING`.

Definition of done: wired into the merged `publish_diagnostics`; tests covering a class missing a project-local interface method, a class missing a project-local abstract-class method (both with and without an intervening abstract subclass that legitimately doesn't implement it), the interface-vs-abstract-class `override`-requirement asymmetry (a same-name/same-arity method with no `override` correctly satisfies an interface requirement but does *not* satisfy an abstract-class requirement), the multi-interface same-signature dedup case, a class implementing a stdlib interface (`Comparable`, etc. -- must NOT be flagged at all, since it's silently invisible to `inherited_chain` today and flagging it would be a false positive against a case this scope explicitly excludes), and a real-NPSP-corpus zero-false-positive sanity pass (matching every other diagnostic on this map).
