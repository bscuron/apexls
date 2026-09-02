Type: research
Status: claimed

## Question

Confirm, against a real Salesforce org via the `sf` CLI oracle (this project's established practice for disputed grammar/semantics questions -- see `apex-grammar-oracle-sf-cli`), the exact real-compiler behavior for duplicate and conflicting Apex modifiers on a declaration, e.g.:

- `private private private void foo() {}` (plain duplication)
- `private public void foo() {}` (conflicting visibility)
- other conflicting combinations worth checking (`static` + `abstract`, duplicate `static`, etc., if time permits)

For each shape tried, record: does the real org reject it at save/deploy time at all, and if so, is the rejection a *syntax*-level error (suggesting this belongs in `apex-parser`'s grammar as a parse-time restriction) or a *semantic* one (suggesting a new `apexls-server` diagnostic, alongside `syntax_error_diagnostics`/`dead_code_diagnostics`/`unresolved_reference_diagnostics`, is the right home)? Today neither layer catches this: `grammar::declarations::modifiers` parses `modifier*` as a plain repetition with no uniqueness constraint (matching the ANTLR reference grammar), and `apex-binder`'s `ModifierSet::from_modifiers` (`crates/apex-binder/src/symbol.rs:123-148`) idempotently re-assigns the same flag per repeated token.

This ticket is fact-finding only -- it does not implement anything. Its answer determines [Implement the duplicate/conflicting-modifier diagnostic](03-modifier-diagnostic-implement.md)'s exact shape.
