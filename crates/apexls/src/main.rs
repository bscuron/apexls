//! `apexls`: the single binary bundling every non-editor-invoked apexls
//! entry point -- `ast` (parse-and-dump a file), `check` (batch
//! diagnostics report), `fix` (batch-apply fixable diagnostics), `soql`
//! (project-wide SOQL query inventory), and `server` (the LSP server, also reachable as its own `apexls-server`
//! binary -- kept separate so existing editor configs that invoke
//! `apexls-server` directly by name, over stdio, need zero changes; see
//! `apexls_server::run_server`'s own doc comment).

/// `println!`, but buffered, and a closed stdout ends the program quietly.
///
/// Buffered because `println!` writes and flushes each line on its own:
/// 11,000 hits written to a file cost ~250 ms that way, more than the search
/// that found them. `main` flushes once on the way out. And quiet on a
/// broken pipe because `apexls query ... | head` closes it after ten lines,
/// which is the reader being done, not an error -- `println!` panicked.
/// Defined before the `mod` declarations so every subcommand sees it.
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::write_stdout_line(format_args!($($arg)*))
    };
}

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
    ///
    /// A COND is either a pattern the match must contain, or `$NAME ~ GLOB`
    /// / `$NAME ~ /REGEX/` testing a capture's text. Globs are shell-style
    /// and anchored (`*`, `?`, `{add,remove}`, and `$OTHER` for another
    /// capture's text) and case-insensitive. Regexes are unanchored and
    /// case-sensitive unless flagged: `/re/i`, also `x`, `s` and `m`.
    Query {
        pattern: String,
        /// Rewrite matches in place: `TEMPLATE` replaces the whole match,
        /// `$N => TEMPLATE` replaces only the pattern's Nth `${...}` group.
        ///
        /// Repeatable for groups, one per group. Templates may use any
        /// named capture and `$1`, `$2`, ... for the groups' text; an empty
        /// template deletes. Repositories are under version control, so
        /// there is no dry run -- omit this to search.
        #[arg(long = "replace", short = 'r', value_name = "TEMPLATE")]
        replace: Vec<String>,
        /// Name the result of applying a pattern to a capture:
        /// `NAME = $SRC ~ PATTERN => TEMPLATE` rewrites every match inside
        /// $SRC and keeps the rest; `NAME = $SRC * PATTERN => TEMPLATE |
        /// SEP` renders each match and joins them. NAME is then usable
        /// like any capture. Repeatable, evaluated in order.
        #[arg(long = "let", value_name = "SPEC")]
        lets: Vec<String>,
        /// Keep only matches for which this condition holds. Repeatable;
        /// every one must hold.
        #[arg(long = "and", value_name = "COND")]
        and: Vec<String>,
        /// Drop matches for which this condition holds. Repeatable; any one
        /// drops the match.
        #[arg(long = "not", value_name = "COND")]
        not: Vec<String>,
        paths: Vec<PathBuf>,
    },
}

/// The buffer behind [`outln!`], created on first use.
static STDOUT: std::sync::Mutex<Option<std::io::BufWriter<std::io::Stdout>>> =
    std::sync::Mutex::new(None);

/// Write one line for [`outln!`] into the buffer.
pub(crate) fn write_stdout_line(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    let out =
        out.get_or_insert_with(|| std::io::BufWriter::with_capacity(64 * 1024, std::io::stdout()));
    if let Err(e) = writeln!(out, "{args}") {
        output_failed(e);
    }
}

/// Write out whatever [`outln!`] buffered. Called once, as `main` returns.
fn flush_stdout() {
    use std::io::Write;
    let mut out = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Err(e)) = out.as_mut().map(|o| o.flush()) {
        output_failed(e);
    }
}

/// A broken pipe exits 0, since the reader has everything it asked for;
/// any other write failure exits 1.
fn output_failed(e: std::io::Error) -> ! {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    eprintln!("error: writing output: {e}");
    std::process::exit(1);
}

fn main() -> ExitCode {
    let code = run_command();
    flush_stdout();
    code
}

fn run_command() -> ExitCode {
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
            lets,
            and,
            not,
            paths,
        } => query::run(&pattern, &replace, &lets, &and, &not, &paths),
    }
}
