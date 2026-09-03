Type: grilling

## Question

Decide the salsa migration's engine architecture and staging plan, building on [ticket 19](19-incremental-invalidation-decision.md)'s decision (adopt real `salsa` as `apex-binder`'s incremental-computation engine) and [ticket 21](21-salsa-integration-research.md)'s research (integration mechanics; a staged, non-all-at-once migration is realistic; recommended order: the rowan-free indices -- `SchemaIndex`/`LabelIndex`/`PageIndex`/`vf_referenced_classes`/`discovery` in `crates/apex-binder/src/incremental.rs`'s `BindCache` -- then Pass 1/Collect, then Pass 1.5/Inherit, then Pass 2/Resolve).

This ticket's destination is a **locked, sequenced implementation plan** -- planning only, no code changes here. Scope is strictly `BindCache`'s three-pass incremental-engine replacement; cross-method bulkification (ticket 12, still deferred) stays out of scope and unblocked only in principle, not scheduled. Every implement ticket this plan produces or splits off must define "done" as: full existing test-suite parity (no behavior change) **and** a `cargo bench -p apex-binder` non-regression check -- both already locked as this whole chain's bar, not optional per-stage.

Resolve, with the user:

- **Confirm or revise the staging order.** Lock ticket 21's four-stage sequence (indices -> Collect -> Inherit -> Resolve) as the plan, or change it.
- **Seam/module structure.** Where does the salsa database and its queries actually live -- a new module inside `apex-binder` (mirroring rust-analyzer's `base-db`, per ticket 21's own precedent finding), a new separate crate, or something else? How does `BoundProgram::from_files_cached`'s implementation route to it as each stage lands -- a feature flag, a trait object swapped per stage, a hard branch, something else?
- **Coexistence / invalidation-glue strategy.** Ticket 21 flagged that a partially-migrated state means `BindCache`'s own `Freshness`-based dirty-diffing and salsa's own revision tracking exist side by side for whichever parts haven't migrated yet. What's the concrete mechanism that keeps the two from drifting out of sync during the staged period -- does every `BindCache` invalidation point also need to poke the corresponding salsa input, and where does that glue code live?
- **Validation technique during the transition.** For a stage where both the old `BindCache` path and the new salsa path exist simultaneously, is there a dual-run-and-diff mechanism (run both, assert identical output, before ever trusting salsa's result on its own) before cutover, or does each stage cut over directly once its own test-suite-parity + benchmark-non-regression bar passes?
- **Split off Stage 1's implement ticket.** Given the answers above, split off a concrete implement ticket for Stage 1 (the rowan-free indices) as this ticket's own follow-on -- the same design-ticket -> implement-ticket pattern ticket 09 used when it split off [ticket 23](23-type-mismatch-checkpoints-implement.md). Stages 2-4's own implement tickets stay as map fog (`Not yet specified`) until each one's turn actually comes -- don't pre-create them now.

Explicitly out of this ticket's scope, left as map fog per the user's own charting decision: Pass 2 (Resolve)'s per-body-vs-per-file granularity call. That's deferred to its own future decision, made with real experience from having already migrated Stages 1-3, not locked abstractly here.

This ticket is not blocked by [ticket 24](24-salsa-dependency-smoke-task.md) (the hands-on dependency-smoke task) -- it can proceed on ticket 21's research alone -- but ticket 24's findings (any surprises versus ticket 21's documented API) should be checked before finalizing the seam/module design here, if ticket 24 has landed by the time this is worked.
