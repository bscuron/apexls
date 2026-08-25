//! Thin compatibility entry point: `apexls-server` is invoked directly
//! by name by existing editor configs (stdio, no arguments) -- kept as
//! its own binary, not folded into `apexls server` only, so those
//! configs need zero changes. All real behavior lives in `apexls_server::run_server`
//! (`src/lib.rs`), shared verbatim with the `apexls server` subcommand.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    apexls_server::run_server().await;
}
