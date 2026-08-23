//! Local debug tool: parse a file and dump its tree.
//!
//! Phase 2 can only parse a single statement/expression, not a whole
//! compilation unit (no class/method declarations yet), so this attempts
//! `parse_statement` over the *entire* file content -- a real `.cls` file
//! starting with `public class Foo { ... }` will show plenty of recorded
//! errors, and that's expected and informative (a quick, honest picture
//! of how much Phase 2 currently understands), not a bug in the CLI.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: apexls-cli <file.cls>");
        return ExitCode::FAILURE;
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let parse = apex_parser::parse_statement(&src);
    println!("{:#?}", parse.syntax());

    let rendered = apex_printer::render(&parse.syntax());
    let round_trips = rendered == src;

    println!("---");
    println!("{} error(s)", parse.errors.len());
    for e in &parse.errors {
        println!("  {} (byte offset {})", e.message, e.offset);
    }
    println!("round-trips exactly: {round_trips}");

    if round_trips {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
