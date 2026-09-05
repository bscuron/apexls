# 04: De-duplicate the shareable overlap between BindCache and BoundProgram

**What to build:** Using ticket 03's breakdown of what actually overlaps between `BindCache` and the long-lived `BoundProgram` snapshot in `apexls-server`, restructure the two to share rather than independently materialize the data ticket 03 identified as duplicated (most likely via `Arc`-sharing per-file data that's currently cloned/re-owned by both sides). The end result is a measurable drop in apexls-server's steady-state resident memory against the real NPSP corpus (currently ~180-200MB), re-measured with the same `mem_profile` harness ticket 03 used, without changing observable LSP behavior.

This ticket's exact shape depends entirely on ticket 03's findings — don't pre-design the restructuring before that data exists. If ticket 03 finds the overlap isn't actually shareable (independently-owned data that only looks similar), this ticket should instead report that finding and close as not-applicable rather than force a fix.

**Blocked by:** 03 (Quantify the BindCache/BoundProgram memory duplication)

**Status:** ready-for-agent

- [ ] The specific duplication ticket 03 identified is eliminated (shared via `Arc` or equivalent, not independently materialized twice)
- [ ] `mem_profile` re-run against the real NPSP corpus shows a measurable reduction in steady-state resident memory
- [ ] Existing `apex-binder`/`apexls-server` test suite and benches (`bind_npsp_full`, `warm_rebind_after_one_file_edit`) still pass with no regression
- [ ] No observable change in LSP behavior for any capability handler
