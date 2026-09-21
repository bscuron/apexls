//! `apexls`: the single binary bundling every non-editor-invoked apexls
//! entry point -- `ast` (parse-and-dump a file), `check` (batch
//! diagnostics report), `fix` (batch-apply fixable diagnostics), `soql`
//! (project-wide SOQL query inventory), and `server` (the LSP server, also reachable as its own `apexls-server`
//! binary -- kept separate so existing editor configs that invoke
//! `apexls-server` directly by name, over stdio, need zero changes; see
//! `apexls_server::run_server`'s own doc comment).

mod ast;
mod check;
mod fix;
mod project;
mod query;
mod soql;

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
    /// List every SOQL query site in the project, ripgrep --vimgrep-style.
    Soql { paths: Vec<PathBuf> },
    /// Search the project for a structural pattern, ripgrep --vimgrep-style.
    ///
    /// PATTERN is Apex code with holes: `...` matches any code in that
    /// position, `$NAME` matches one construct and captures it.
    Query {
        pattern: String,
        /// Exclude any match that itself contains a match of this pattern.
        /// Repeatable; a match is dropped if any of them hits.
        /// Rewrite every match with this template, in place.
        ///
        /// Only named captures from PATTERN may appear in it; an empty
        /// template deletes the match. Repositories are under version
        /// control, so there is no dry run -- omit this to search.
        #[arg(long = "replace", short = 'r', value_name = "TEMPLATE")]
        replace: Option<String>,
        #[arg(long = "not", value_name = "PATTERN")]
        not: Vec<String>,
        /// Keep only matches that themselves contain a match of this
        /// pattern. Repeatable; every one of them must hit.
        #[arg(long = "containing", value_name = "PATTERN")]
        containing: Vec<String>,
        paths: Vec<PathBuf>,
    },
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
        Command::Soql { paths } => soql::run(&paths),
        Command::Query {
            pattern,
            replace,
            not,
            containing,
            paths,
        } => query::run(&pattern, replace.as_deref(), &not, &containing, &paths),
    }
}
