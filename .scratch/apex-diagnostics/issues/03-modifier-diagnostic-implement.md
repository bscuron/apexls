Type: task
Status: resolved
Blocked by: 02

## Question

Implement the duplicate/conflicting-modifier check in the shape [the research ticket](02-modifier-diagnostic-research.md) determined is correct against a real org: either a new `apex-parser` grammar-level restriction (if the real compiler treats it as a syntax error) or a new `apexls-server` diagnostic source alongside the existing three (if semantic), following whichever of this project's existing patterns matches the confirmed shape.

Definition of done: matches the real-org behavior confirmed by the research ticket exactly (not a guess), with tests covering both the duplicate-modifier and conflicting-visibility shapes that ticket investigated.

## Answer

Implemented as `apexls_server::capabilities::modifier_diagnostics` (`crates/apexls-server/src/capabilities.rs`), wired into the merged `publish_diagnostics` alongside the four existing sources. Purely syntax-tree-based -- doesn't need the binder's symbol table at all, since it walks every `MODIFIER_BEARING_KINDS` node (every `HasModifiers` impl: class/interface/enum/method/constructor/field/property/parameter) directly via `apex_syntax::ast::support::children::<Modifier>`, reading the raw, pre-collapse modifier-token list rather than `apex_binder::symbol::ModifierSet` (which idempotently discards duplicates by design).

Two general rules, plus one narrowly-scoped one, per the research ticket's recommendation:
- **Duplicate modifier**: any modifier keyword repeated on one declaration -> `Duplicate modifier: <keyword>` on each repeat past the first. Generalized from the two org-verified cases (`private`/`static`) to any keyword, since the underlying rule isn't keyword-specific.
- **Conflicting visibility**: more than one distinct `public`/`private`/`protected`/`global` on one declaration -> `Declarations can only have one scope`, verbatim real-org wording.
- **`static`+`abstract`**: scoped to `MethodDecl` only (the one shape actually verified against a real org) -> `static methods cannot be abstract`, verbatim. Not generalized to fields/properties/parameters, since `abstract` isn't otherwise meaningful there.

Tests: `crates/apexls-server/tests/modifier_diagnostics.rs` -- all five real-org-confirmed shapes (duplicate on a method, duplicate on a field, conflicting visibility, duplicate `static`, `static`+`abstract`) plus a clean-code negative test. Zero-false-positive check against the real NPSP corpus: grepped the whole corpus for every pattern this diagnostic could fire on (adjacent visibility keywords, any repeated modifier keyword, `static abstract`/`abstract static`) -- zero matches, as expected for real, compiling Apex (these are genuine compile errors; valid shipped code structurally can't contain them). Full `apexls-server` suite passes (25 test binaries, no regressions).
