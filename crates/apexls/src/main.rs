//! `apexls`: the single binary bundling every non-editor-invoked apexls
//! entry point -- `ast` (parse-and-dump a file), `check` (batch
//! diagnostics report), `fix` (batch-apply fixable diagnostics), and
//! `server` (the LSP server, also reachable as its own `apexls-server`
//! binary -- kept separate so existing editor configs that invoke
//! `apexls-server` directly by name, over stdio, need zero changes; see
//! `apexls_server::run_server`'s own doc comment).

mod ast;
mod check;
mod fix;
mod project;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

// See `apexls-server/src/main.rs`'s matching allocator for why: the
// binder's rayon-parallelized passes are a concurrent, many-small-
// allocations workload mimalloc handles better than the system
// allocator.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "apexls")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the LSP server over stdio.
    Server,
    /// Parse a file and dump its syntax tree.
    Ast { file: PathBuf },
    /// Report every diagnostic across the project, cargo-check-style.
    Check { paths: Vec<PathBuf> },
    /// Batch-apply every fixable diagnostic across the project, cargo-fix-style.
    Fix { paths: Vec<PathBuf> },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Server => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build the Tokio runtime")
                .block_on(apexls_server::run_server());
            ExitCode::SUCCESS
        }
        Command::Ast { file } => ast::run(&file),
        Command::Check { paths } => check::run(&paths),
        Command::Fix { paths } => fix::run(&paths),
    }
}
