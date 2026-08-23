# Differential-testing oracle harness

Compares apexls's parse results against reference implementations so that
"correct" has an external check, not just internal self-consistency.

Adapters shell out to each reference parser as a subprocess (Java/Node),
keeping the oracle dependency out of the main Rust crate graph:

- `adapters/antlr-apexdevtools/` — wraps `@apexdevtools/apex-parser`
  (the primary oracle: https://github.com/apex-dev-tools/apex-parser)
- `adapters/antlr-grammars-v4/` — wraps the independent grammar at
  https://github.com/antlr/grammars-v4/blob/master/apex/apex.g4
- `adapters/tree-sitter-sfapex/` — wraps
  https://github.com/aheber/tree-sitter-sfapex for CST/error-recovery
  comparison

`scratch-org/` holds scripts that submit files to a Salesforce scratch
org's deploy-validate endpoint for true compiler ground truth. This is the
highest-authority oracle but slow and rate-limited, so it's used sparingly
(scheduled runs, or to adjudicate disagreements between the other oracles)
rather than on every PR.
