# 03: Quantify the BindCache/BoundProgram memory duplication

**What to build:** apexls-server holds two separately-owned pieces of state alive for the process's whole lifetime: `BindCache` (retaining per-file `Parse` trees and other intermediate caches) and a separately-materialized `BoundProgram` snapshot (`program: RwLock<Option<BoundProgram>>`). `crates/apex-binder/examples/mem_profile.rs`'s own doc comment states this dual-storage is the reason apexls-server's steady-state resident memory measures ~180-200MB against the real NPSP corpus — but that's a named suspect, not a quantified breakdown. This ticket is the measurement step only: run the already-working `cargo run -p apex-binder --release --example mem_profile` harness, inspect the resulting `dhat-heap.json` (e.g. via https://nnethercote.github.io/dh_view/dh_view.html), and write up which specific allocation sites in `BindCache` vs `BoundProgram` account for the overlap and roughly how much memory each holds.

No source code changes are expected from this ticket — it produces a written finding (a short doc under `.scratch/apex-performance/`, e.g. a `dhat-breakdown.md`) that ticket 04 will use to scope its actual fix. Don't attempt to fix or restructure anything here even if the answer looks obvious from the profile.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] `mem_profile` example run against the real NPSP corpus and its `dhat-heap.json` captured
- [ ] Written breakdown identifying which allocation sites/data structures in `BindCache` and `BoundProgram` overlap, with approximate memory attributed to each
- [ ] Breakdown explicitly states whether the overlap is shareable (e.g. via `Arc`) or is independently-owned data that happens to look similar
