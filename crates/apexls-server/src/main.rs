//! Thin compatibility entry point: `apexls-server` is invoked directly
//! by name by existing editor configs (stdio, no arguments) -- kept as
//! its own binary, not folded into `apexls server` only, so those
//! configs need zero changes. All real behavior lives in `apexls_server::run_server`
//! (`src/lib.rs`), shared verbatim with the `apexls server` subcommand.

// The binder's Pass 1/Pass 2 (`crates/apex-binder/src/lib.rs`) are both
// `rayon`-parallelized, so a bind is many threads concurrently
// allocating/freeing short-lived `SmolStr`/`Vec`/`FxHashMap` entries --
// exactly the workload mimalloc's per-thread heaps are designed for,
// unlike the system allocator's more contended global state.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    apexls_server::run_server().await;
}
