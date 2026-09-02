Type: task
Status: open
Blocked by: 02

## Question

Implement the duplicate/conflicting-modifier check in the shape [the research ticket](02-modifier-diagnostic-research.md) determined is correct against a real org: either a new `apex-parser` grammar-level restriction (if the real compiler treats it as a syntax error) or a new `apexls-server` diagnostic source alongside the existing three (if semantic), following whichever of this project's existing patterns matches the confirmed shape.

Definition of done: matches the real-org behavior confirmed by the research ticket exactly (not a guess), with tests covering both the duplicate-modifier and conflicting-visibility shapes that ticket investigated.
