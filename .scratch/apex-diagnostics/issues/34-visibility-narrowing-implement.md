Type: task
Status: open
Blocked by: 33

## Question

Implement `visibility_narrowing_diagnostics` per [ticket 33](33-visibility-narrowing-decision.md)'s locked architecture: wire it into `apexls-server`'s merged `publish_diagnostics` as a fifth diagnostic source alongside `syntax_error_diagnostics`, `unresolved_reference_diagnostics`, `dead_code_diagnostics`, and `type_mismatch_diagnostics`.

Definition of done, per this map's own standing practice (see Notes):
- Benchmark against the real NPSP corpus before landing, matching `resolution_regression_baseline.rs`'s existing practice -- confirm zero false positives across the full corpus (1,035+ files), not just a sample.
- Any disputed real-Apex-compiler-behavior question hit during implementation gets settled via the `sf` CLI against a connected org, not guessed.
- Follows ticket 33's locked message wording, severity (`WARNING`, no tag), and reference-bucketing algorithm exactly.

## Answer
