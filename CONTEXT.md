# apexls

A Rust workspace implementing a language server and CLI for Apex (Salesforce's language): lexer, parser, binder, and the `apexls`/`apexls-server` binaries built on top of them.

## Language

**Diagnostic**:
A reported problem in a file, surfaced by `apexls-server::diagnostics_for_file` and printed by `apexls check`. Every diagnostic in this codebase clears a zero-false-positive detection bar -- if it fires, the problem is real, never a heuristic guess.

**Fixable diagnostic**:
A diagnostic for which a specific automatic edit has been independently proven safe to apply without human review. A diagnostic's own zero-false-positive detection bar does not by itself make it fixable -- detection-certainty and edit-safety are separate claims, and a diagnostic only earns this label once a fix ticket argues the edit itself is safe.
_Avoid_: auto-fixable diagnostic, actionable diagnostic

**Candidate fix**:
One concrete, protocol-agnostic edit proposed for a fixable diagnostic, produced by shared logic callable from both `apexls-server`'s `textDocument/codeAction` handlers and the `apexls fix` CLI command. Becomes an applied fix once it survives conflict resolution.
_Avoid_: quick fix, code action (those are the LSP wire-protocol's own realization of a candidate fix, not the shared concept itself)

**Overlap**:
Two candidate fixes in the same file whose text ranges intersect. Splits into two distinct shapes needing different handling -- see Nested overlap and Crossing overlap.

**Nested overlap**:
An overlap where one candidate fix's range fully contains the other's -- e.g. a dead-code deletion spanning an entire declaration that also contains a duplicate-modifier fix's smaller range inside it. Resolved by applying the outer fix and treating the inner one as a subsumed fix.

**Crossing overlap**:
An overlap where two candidate fixes' ranges partially intersect but neither contains the other. Genuinely ambiguous -- both become conflicting fixes.

**Subsumed fix**:
The inner candidate fix in a nested overlap. Dropped silently when its containing fix is applied -- its target text is gone either way, so there is nothing to report.
_Avoid_: dropped fix, redundant fix

**Conflicting fix**:
Either candidate fix in a crossing overlap. Both are skipped and reported, since applying either would change text the other's edit was computed against.
_Avoid_: skipped fix (a conflicting fix is skipped, but "skipped" alone doesn't say why -- a subsumed fix is also skipped, silently, for a different reason)

**Applied fix**:
A candidate fix that was neither subsumed nor conflicting, and was written to disk by `apexls fix`.
