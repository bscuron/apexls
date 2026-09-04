Type: grilling
Status: open
Blocked by: 35

## Question

Given [the research ticket](35-nested-type-candidacy-research.md)'s findings, decide whether and how to extend `visibility_narrowing_diagnostics` to flag a nested type (`Class`/`Interface`/`Enum` declared inside another type) whose own declared visibility could be narrowed -- a real, independent gap from member-level narrowing (tickets 32-34, already shipped), with a real correctness hazard the member-level check never had.

Already established by ticket 35 (do not re-investigate, use as given):

- `protected` is flatly illegal on a nested type (confirmed via `sf` CLI: "protected is not allowed on classes") -- the only legal starting visibility for a nested type is `Public` or `Private` (`Global` stays out of scope, matching the member-level check's own already-settled scope, and carries an extra container-visibility precondition -- a `global` nested type requires a `global` outer -- that only matters if `Global` narrowing is ever brought into scope later).
- Narrowing a nested type's own visibility is a real, independently-breaking change distinct from narrowing any of its members: a currently-legal external reference can become a compile error even when every individual member's own visibility stays untouched, specifically when the type's own name leaks into an external declared-type slot (a return type, a variable type) -- confirmed via three real `sf`-CLI deploy probes. Dispatch through a publicly-visible interface/base type (never naming the concrete private type externally) bypasses this entirely and stays reachable.
- `ReferenceTable` already records a reference to a type symbol itself via the exact same mechanism as a member reference (every `new Outer.Inner()`, declared-type-slot use, `instanceof`, and `extends`/`implements` supertype binding) -- confirmed at every real call site, not assumed.
- `declaring_type_of_reference`/`top_level_container` (`crates/apex-binder/src/visibility_narrowing.rs:131-156`, `71-77`) generalize to a type candidate completely unchanged -- neither inspects the candidate's own kind.
- `required_visibility` (`visibility_narrowing.rs:181-215`) does **not** generalize as-is: its `Public` branch can suggest `Protected` for a reference reachable only via `subtypes`, an illegal state for a nested type. A type-level version needs its own disjunction.
- Real NPSP corpus: 684 nested types carry an explicit visibility keyword, 499 already `Public` -- a real opportunity, before any reference-based narrowing filter runs (roughly an order of magnitude smaller than the member-level check's ~5,752 candidates, but not tiny).

Resolve, with the user:

- **Lock the type-level candidate-kind filter.** Research already sketched it: `matches!(symbol.kind, Class | Interface | Enum) && symbol.container.is_some() && symbol.modifiers.visibility == Visibility::Public` -- deliberately `Public`-only, unlike the member-level check's `Public | Protected` (there is no legal `Protected` starting state to narrow from). Confirm/amend.
- **Lock the type-level `required_visibility` algorithm**, given `Protected` can never be a legal suggested target. Worth exploring with the user: does the existing reference-based approach already handle the interface-dispatch-bypass case (§1's finding 3) correctly *for free*, without new interface-aware logic -- since a reference reached only through a public interface never names the concrete private type at all, so `references_to(concrete_type_id)` would already show zero *external* references to the type's own name in that case? If so, the algorithm may collapse to a simple two-way check (every reference same-top-level-family -> `Private` is safe; any reference from outside the family, for any reason -- direct naming, `subtypes`, anything else -- means it stays `Public`, not narrowable, since there's no legal intermediate state to fall back to the way `Protected` served members). Confirm this reasoning concretely (with test fixtures, not just argued abstractly) before locking it.
- **Module/function placement**: same `visibility_narrowing.rs` module (a `narrowing_candidates_in_file`-shaped sibling function, or does `narrowing_candidates_in_file` itself grow a type-level pass folded into its existing output?), or a clearly-separated new function/module? Consider that `NarrowingCandidate`'s existing `kind: SymbolKind` field already supports `Class`/`Interface`/`Enum` as a value without any struct change.
- **Message wording** for a type-level candidate -- does ticket 33's existing message format (`"{kind} '{name}' is declared '{current}' but could be '{required}'"`) read naturally for `"Class 'Foo' is declared 'public' but could be 'private'"`, or does a type-level narrowing suggestion need its own distinct wording given the real breaking-change risk ticket 35 confirmed (arguably worth a stronger warning than the member-level suggestion, which never has this hazard)?
- **Split off this diagnostic's own implement ticket** once the above is resolved, per this map's established design-ticket -> implement-ticket pattern.

Out of scope: nested-type dead-code detection (a separate, differently-shaped gap ticket 35 also researched -- see [ticket 37](37-nested-type-dead-code-decision.md)); re-litigating any of ticket 35's own settled findings above; `Global` nested-type narrowing (out of scope, matching the member-level check's own settled scope).

## Answer
