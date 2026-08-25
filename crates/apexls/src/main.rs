//! `apexls`: the single binary bundling every non-editor-invoked apexls
//! entry point -- `ast` (parse-and-dump a file), `dead` (batch dead-code
//! report), and `server` (the LSP server, also reachable as its own
//! `apexls-server` binary -- kept separate so existing editor configs
//! that invoke `apexls-server` directly by name, over stdio, need zero
//! changes; see `apexls_server::run_server`'s own doc comment).

mod ast;
mod dead;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// Report provably-dead declarations across the project.
    Dead { paths: Vec<PathBuf> },
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
        Command::Dead { paths } => dead::run(&paths),
    }
}
