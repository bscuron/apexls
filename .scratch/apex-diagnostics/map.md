Label: wayfinder:map

## Destination

A decision on whether and how to promote `apex-binder`'s walker-internal `Ty` type (`crates/apex-binder/src/ty.rs` -- currently just per-expression type chaining during Pass 2 body resolution, never stored on `BoundProgram` or exposed outside the walker) into a real, queryable, stored type layer, together with a prioritized, zero-false-positive-only spec of new diagnostic checks (both structural/flow checks that don't need it, and type-checking-flavored checks that do) to wire into `apexls-server`'s merged `publish_diagnostics` (`crates/apexls-server/src/lib.rs`), alongside the three existing sources (`syntax_error_diagnostics`, `unresolved_reference_diagnostics`, `dead_code_diagnostics`). Sequencing between the type-layer decision and the no-inference-needed checks is intentionally left open -- discovered from how tickets actually end up blocking each other, not asserted up front.

## Notes

- Every diagnostic must clear the same zero-false-positive bar `dead_code_diagnostics` already established for this codebase -- never flag unless provably correct. No confidence-graded WARNING/ERROR escape valve (unlike `unresolved_reference_diagnostics`'s existing pattern) unless a specific ticket's own resolution decides otherwise.
- Consult `apex-grammar-oracle-sf-cli` practice (the `sf` CLI against a connected org) for any disputed real-Apex-compiler-behavior question, same as this project's established norm.
- Benchmark any new binder pass against the real NPSP corpus before landing, matching `resolution_regression_baseline.rs`'s existing practice -- this is baked into each implementation ticket's own definition of done, not tracked as a separate cross-cutting ticket.
- BACKLOG.md is to be **deleted entirely** once every ticket on this map is resolved -- it's superseded, messy documentation debt once this map is the live planning surface. (BACKLOG.md section 3, "Further diagnostic sources," was this map's starting reference input.)

## Decisions so far

(none yet)

## Not yet specified

- **Type-checking-flavored diagnostics gated by the Ty-promotion decision** (assignment type-mismatch, wrong-argument-type beyond current arity/overload narrowing, etc.) -- can't be scoped until [Decide the Ty-promotion / type-inference-layer architecture](issues/09-ty-promotion-architecture-decision.md) resolves.
- **`Resolution::Candidates` as an error diagnostic** -- genuinely unclear shape (not just "not built yet"): under what conditions an unresolved-overload ambiguity in this binder actually corresponds to a real compiler error is not yet phrased sharply enough to ticket. BACKLOG.md flags real false-positive risk against this project's own no-guessing discipline; needs the `Candidates` cases themselves surveyed before even a research question can be stated.
- **Per-diagnostic opt-out for a specific noisy check** -- e.g. if the bulkification (DML/SOQL-in-loop) check turns out too noisy for some real projects once shipped. Only worth a ticket if and when a specific check's own resolution surfaces this as a real problem; not a general mechanism to build speculatively (see Out of scope).

## Out of scope

- **Salesforce security/best-practice lints** (hardcoded record IDs, SOQL-injection-shaped dynamic query construction, a class missing `with sharing`) -- BACKLOG.md itself calls this a different *kind* of feature (style/security-review, PMD/Salesforce-Code-Analyzer territory) than "the compiler found a bug." This destination is scoped to compiler-error-shaped correctness diagnostics only.
- **A general per-diagnostic configuration/opt-out mechanism** (e.g. `.apexls.toml`, LSP `initializationOptions` severity overrides) -- none of the three existing diagnostic sources have this today; it's an orthogonal config-plumbing feature, not diagnostic *analysis*, and not needed unless a specific shipped check proves it's needed (see Not yet specified).
