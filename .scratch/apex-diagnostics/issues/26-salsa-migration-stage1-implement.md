Type: task

## Question

Implement Stage 1 of [ticket 25](25-salsa-migration-architecture-decision.md)'s locked salsa migration plan: convert `BindCache`'s rowan-free directory-walk-derived data (`discovery`/`schema`/`labels`/`pages`/`vf_referenced_classes` in `crates/apex-binder/src/incremental.rs`) into real `salsa` inputs, per the seam, coexistence, and validation strategy ticket 25 already decided. This is the lowest-risk stage (no rowan `SyntaxNode`/`SyntaxPtr` involvement at all) and the first real cutover, not just the smoke-test ticket 24 already did.

Concretely, per ticket 25's own answer:

- Add `crates/apex-binder/src/db.rs`, a new module (not a new crate) holding the salsa database type and its `#[salsa::input]`s for `discovery`/`schema`/`labels`/`pages`/`vf_referenced_classes`, plus the `#[salsa::tracked]` query functions that reproduce today's `SchemaIndex::from_discovery`/`LabelIndex::from_discovery`/`PageIndex::from_discovery`/`apex_metadata::visualforce::referenced_controller_classes` computations.
- Wire adapter glue (living in `db.rs`, called from `BoundProgram::from_files_cached` in `crates/apex-binder/src/lib.rs`) at the existing `need_fresh_discovery` write-site (`lib.rs` Stage -1, around line 269) so every time `BindCache` decides a fresh walk is needed and writes `cache.discovery`/`cache.schema`/`cache.labels`/`cache.pages`/`cache.vf_referenced_classes`, the same fresh values are pushed into the matching salsa inputs. `BindCache`'s own `need_fresh_discovery` check stays the sole staleness authority for this stage -- salsa is purely a memoization layer downstream of it.
- Replace `from_files_cached`'s reads of `cache.schema`/`cache.labels`/`cache.pages`/`cache.vf_referenced_classes` with reads from the salsa database's tracked queries -- a hard branch/replacement in place, not a runtime feature flag or trait-object toggle.
- Add a temporary dual-run-and-diff corpus test (same shape as `resolution_regression_baseline.rs`), running both the old `BindCache`-computed values and the new salsa-computed values over the full real NPSP corpus (1,035 files) and asserting byte-identical output, before ever trusting the salsa path alone. Delete this test once cutover is confirmed and `BindCache`'s own hand-rolled computation for this data is removed.
- Once the dual-run test confirms parity, delete `BindCache`'s own `schema`/`labels`/`pages`/`vf_referenced_classes` fields and their computation in `from_files_cached` -- Stage 1 data is salsa-owned only after cutover, not kept as a permanent shadow copy. `BindCache::discovery`'s own field/`invalidate_discovery` hook stays (it's still the staleness trigger salsa's input is synced from), but the four derived indices move to salsa-only storage.

**Done bar** (per ticket 25's chain-wide lock, not optional for this stage): full existing test-suite parity (no behavior change) and a `cargo bench -p apex-binder` non-regression check, in addition to the dual-run-and-diff corpus parity above.

Out of scope: Stage 1 touches only the rowan-free indices. Pass 1/Collect (Stage 2), Pass 1.5/Inherit (Stage 3), and Pass 2/Resolve (Stage 4) stay unspecified until their own turn comes, per ticket 25.
