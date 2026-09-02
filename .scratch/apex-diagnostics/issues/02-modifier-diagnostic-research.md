Type: research
Status: resolved

## Question

Confirm, against a real Salesforce org via the `sf` CLI oracle (this project's established practice for disputed grammar/semantics questions -- see `apex-grammar-oracle-sf-cli`), the exact real-compiler behavior for duplicate and conflicting Apex modifiers on a declaration, e.g.:

- `private private private void foo() {}` (plain duplication)
- `private public void foo() {}` (conflicting visibility)
- other conflicting combinations worth checking (`static` + `abstract`, duplicate `static`, etc., if time permits)

For each shape tried, record: does the real org reject it at save/deploy time at all, and if so, is the rejection a *syntax*-level error (suggesting this belongs in `apex-parser`'s grammar as a parse-time restriction) or a *semantic* one (suggesting a new `apexls-server` diagnostic, alongside `syntax_error_diagnostics`/`dead_code_diagnostics`/`unresolved_reference_diagnostics`, is the right home)? Today neither layer catches this: `grammar::declarations::modifiers` parses `modifier*` as a plain repetition with no uniqueness constraint (matching the ANTLR reference grammar), and `apex-binder`'s `ModifierSet::from_modifiers` (`crates/apex-binder/src/symbol.rs:123-148`) idempotently re-assigns the same flag per repeated token.

This ticket is fact-finding only -- it does not implement anything. Its answer determines [Implement the duplicate/conflicting-modifier diagnostic](03-modifier-diagnostic-implement.md)'s exact shape.

## Answer

Verified against a real connected Salesforce org (`bscuron19.f9f273f0b808@agentforce.com`, API v62.0) via `sf project deploy start --source-dir ... --json`, deploying one throwaway Apex class at a time from a scratch SFDX project (outside the repo checkout) and inspecting the returned deploy-result JSON. Every shape below deployed as a normal metadata component (no client-side/CLI syntax rejection before reaching the org) and came back as a `componentFailures` entry with `componentType: "ApexClass"`, `problemType: "Error"`, and `numberComponentsDeployed: 0` -- i.e. the org's own Apex compiler rejected each one during deployment, not the CLI/local tooling. No test class was ever actually created in the org (confirmed via `numberComponentsDeployed: 0` on every attempt and a follow-up `SELECT Name FROM ApexClass WHERE Name LIKE 'Modifier%'` returning zero rows), so no `sf project delete source` cleanup was needed.

Shapes tried and exact results:

1. **Plain duplicate modifier on a method** -- `private private void foo() {}` inside a class body:
   `"problem": "Duplicate modifier: private"`, full error string `"Duplicate modifier: private (2:26)"`.

2. **Conflicting visibility modifiers** -- `private public void foo() {}`:
   `"problem": "Declarations can only have one scope"`, full error string `"Declarations can only have one scope (2:25)"`.

3. **Duplicate `static`** -- `public static static void foo() {}`:
   `"problem": "Duplicate modifier: static"`, full error string `"Duplicate modifier: static (2:31)"`.

4. **`static` + `abstract` conflict** -- `public static abstract void foo();` inside an `abstract class`:
   `"problem": "static methods cannot be abstract"`, full error string `"static methods cannot be abstract (2:33)"`.

5. **Plain duplicate modifier on a field** (checked in addition to methods, since the ticket's example generalizes "on a declaration") -- `private private Integer x;`:
   `"problem": "Duplicate modifier: private"`, full error string `"Duplicate modifier: private (2:29)"` -- identical shape/wording to the method case.

Interpretation -- syntax vs. semantic:

This is a **semantic/deploy-time error, not a syntax-level (parse) error**. Evidence:

- The rejection surfaces exclusively through the metadata deploy pipeline's per-component compile result (`componentFailures[].problem`), the exact same channel Apex uses for ordinary semantic errors (unresolved types, invalid overrides, etc.) -- not as a distinct "invalid deployment package" / manifest-level or malformed-metadata-XML failure that would indicate the payload never got past a parser.
- The four distinct, targeted messages ("Duplicate modifier: X", "Declarations can only have one scope", "static methods cannot be abstract") are exactly the kind of named, rule-specific diagnostics a semantic-analysis pass produces once it has a resolved modifier set to reason about -- not the generic "unexpected token" / "mismatched input" phrasing the real org (and apex-parser, per the existing ANTLR-grammar-derived `apex-parser` tests) produces for actual grammar violations. Each message names the specific rule violated (duplicate-ness, scope arity, static/abstract incompatibility) rather than a token/position-oriented parse complaint.
- This is consistent with `apex-parser`'s current grammar, which parses `modifier*` as an unconstrained repetition (matching the ANTLR reference grammar) with no uniqueness or mutual-exclusion constraint at the grammar level -- the real org's grammar apparently accepts the same repetition syntactically and defers the uniqueness/conflict check to a later compiler phase.

Recommendation:

Implement this as a **new `apexls-server` diagnostic** (semantic pass), not a grammar-level restriction in `apex-parser`. Concretely:
- Add a check alongside `syntax_error_diagnostics` / `dead_code_diagnostics` / `unresolved_reference_diagnostics` (naming it e.g. `modifier_diagnostics` or folding into an existing binder-adjacent pass) that walks each declaration's raw modifier token list (before/independent of `ModifierSet::from_modifiers`'s idempotent flag-collapsing in `crates/apex-binder/src/symbol.rs:123-148`) and reports:
  - a duplicate-modifier error per repeated modifier token, matching wording like `Duplicate modifier: <name>` for parity with the real compiler's message;
  - a visibility-scope-conflict error (`Declarations can only have one scope`) when more than one of `private`/`public`/`protected`/`global` appear together;
  - targeted mutual-exclusion errors for specific known-incompatible pairs such as `static`+`abstract` (`static methods cannot be abstract`), and any other such pairs found to matter for the follow-up implementation ticket.
- `apex-parser`'s grammar should NOT be changed to reject these at parse time -- doing so would diverge from the real compiler's own grammar (which accepts the repetition syntactically) and would make the parser's error recovery/AST shape for these cases behave differently than real Apex source, which is undesirable for an LSP that needs to keep offering completions/other features on in-progress, syntactically-tolerant text.

This answers 03's design question: the new diagnostic belongs in `apexls-server`, sourced from the raw modifier-token list per declaration (not just the collapsed `ModifierSet`), and should reuse the real compiler's exact message wording documented above where practical.
