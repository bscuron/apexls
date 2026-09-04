Type: grilling
Status: open
Blocked by: 32

## Question

Given [the research ticket](32-visibility-narrowing-research.md)'s findings, decide `visibility_narrowing_diagnostics`'s concrete architecture: how a candidate's **required visibility** actually gets computed and how the diagnostic wires into `apexls-server`'s merged `publish_diagnostics`, alongside the existing four sources (`syntax_error_diagnostics`, `unresolved_reference_diagnostics`, `dead_code_diagnostics`, `type_mismatch_diagnostics`).

Already settled (do not re-litigate): in-scope transitions (`Public` -> narrower, `Protected` -> `Private`; `Global` out of scope), member kinds (fields/properties/methods/constructors; nested classes deferred), the `dead_code_diagnostics`-verbatim annotation/Visualforce exemption list, the override/interface-implementation exclusion, and `WARNING` severity with no tag.

Resolve, with the user:

- **The reference-bucketing algorithm**, using ticket 32's findings on how to map a reference back to its enclosing declaration and on same-file nested-type private access: for a given candidate, how exactly do references get sorted to arrive at "narrowest safe target" -- e.g. is it a simple three-way classification (same declaring scope / reachable via `subtypes` / anything else), or does it need to be more precise than that?
- **Where the zero-reference / `dead_code_diagnostics` overlap gets resolved** -- lock the actual rule ticket 32 investigated (skip zero-reference candidates, or some other resolution if the research surfaced a real double-flagging scenario).
- **Module/function placement** -- new function in `crates/apexls-server/src/capabilities.rs` alongside its siblings, or does the required-visibility computation itself belong in `apex-binder` (as a `BoundProgram` method, the way `dead_symbols_in_file` lives in `apex_binder` and `capabilities.rs` just formats its output as LSP diagnostics)? The existing pattern splits "prove it" (binder) from "format it" (server) -- confirm this follows the same split.
- **Message wording and the narrower-target's phrasing** -- e.g. `"Method 'foo' is declared 'public' but could be 'private'"` vs. some other phrasing; check against how `dead_code_diagnostics`'s own message (`"{kind} '{name}' is never used"`) and `modifier_diagnostics`'s messages are worded, for consistency.
- **Split off this diagnostic's own implement ticket** once the above is resolved, per this map's established design-ticket -> implement-ticket pattern (e.g. ticket 09 -> 23, ticket 27 -> 29).

Out of scope: re-litigating anything already settled above; nested/inner-class visibility narrowing (a real, separate follow-on); any general per-diagnostic configuration/opt-out mechanism (already out of scope for this whole map).

## Answer
