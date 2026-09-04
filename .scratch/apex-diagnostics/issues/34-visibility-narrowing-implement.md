Type: task
Status: open
Blocked by: 33

## Question

Implement `visibility_narrowing_diagnostics` per [ticket 33](33-visibility-narrowing-decision.md)'s locked architecture, wiring it into `apexls-server`'s merged `publish_diagnostics` as a fifth diagnostic source alongside `syntax_error_diagnostics`, `unresolved_reference_diagnostics`, `dead_code_diagnostics`, and `type_mismatch_diagnostics`. Concrete shape, per ticket 33 (do not redesign, implement exactly):

- New `crates/apex-binder/src/visibility_narrowing.rs`, module-shaped like `dead_code.rs`. Public entry point `narrowing_candidates_in_file(program: &BoundProgram, file: FileId) -> Vec<NarrowingCandidate>`, mirroring `dead_symbols_in_file`'s signature.
- `NarrowingCandidate { ptr: SyntaxPtr, name_range: TextRange, kind: SymbolKind, name: SmolStr, current: Visibility, required: Visibility }`, `required` always strictly narrower than `current`.
- Candidacy: `Method | Field | Property | Constructor` with `Visibility::Public | Visibility::Protected`. Exemptions: `dead_code.rs`'s full three-part check verbatim (`has_platform_invocation_annotation`, `is_visualforce_referenced`, `is_platform_invoked_test_method`) plus a new override/interface-satisfaction exclusion (same shape as `missing_implementation_diagnostics`'s interface-satisfaction check) plus a locally-reimplemented `top_level_container` walk (`dead_code.rs:208-211`'s precedent -- do not expose new `SymbolTable` surface).
- Zero-reference candidates (`Public` or `Protected`) are skipped entirely -- deferred to `dead_code_diagnostics`, the `Protected` gap is accepted, out of scope here.
- New `declaring_type_of_reference(program: &BoundProgram, reference: SyntaxPtr) -> Option<SymbolId>`, generalizing `BoundProgram::enclosing_callable`'s ancestor-walk-then-range-match pattern (`lib.rs:1157-1194`) to `ClassDecl`/`InterfaceDecl`/`EnumDecl` -- exact sketch in ticket 32's Answer §1. Computed lazily per candidate at diagnostic-run time, never threaded into `ReferenceTable::set` at bind time.
- `Protected -> Private`: narrows iff every reference's `top_level_container(declaring_type_of_reference(ref))` equals the candidate's own; an unplaceable reference blocks narrowing.
- `Public -> narrower`: same-family reference contributes nothing; declaring type itself or a `subtypes(declaring)` member contributes "needs `Protected`"; anything else (including unplaceable) keeps it `Public` (not emitted). Result is `Private`, `Protected`, or not-a-candidate.
- `apexls-server/src/capabilities.rs::visibility_narrowing_diagnostics(program, file, encoding) -> Vec<Diagnostic>`. `WARNING` severity, no `DiagnosticTag`. Message: `"{kind} '{name}' is declared '{current}' but could be '{required}'"`.

Definition of done, per this map's own standing practice (see Notes):
- Benchmark against the real NPSP corpus before landing, matching `resolution_regression_baseline.rs`'s existing practice -- confirm zero false positives across the full corpus (1,035+ files), not just a sample. Ticket 32's rough scan (~5,752 raw candidates, 5,106 with >=1 reference) is a starting signal, not a substitute for running the real implementation against the real corpus.
- Any disputed real-Apex-compiler-behavior question hit during implementation gets settled via the `sf` CLI against a connected org, not guessed.
- Follows ticket 33's locked message wording, severity (`WARNING`, no tag), and reference-bucketing algorithm exactly.

## Answer
