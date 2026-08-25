//! `apexls ast <file>`: parse a file and dump its tree -- the graduated
//! form of what `apexls-cli` used to do standalone. Now parses a whole
//! compilation unit (`apex_parser::parse_compilation_unit`), not just a
//! single statement -- `apex_parser::parse_statement` was a Phase-2-era
//! leftover from before whole-file parsing existed, with no remaining
//! reason to keep now that `apex-binder` exists and this file is already
//! being touched.

use std::path::Path;
use std::process::ExitCode;

pub fn run(path: &Path) -> ExitCode {
    // On a thread with a generously large stack, then joined, rather than
    // doing the work directly on `main`'s (typically ~1 MiB on Windows)
    // default stack -- see `apex_parser`'s module doc comment's "deep-tree
    // stack safety caveat": a pathologically long chain expression can
    // overflow the stack purely on *dropping* the parsed tree, unrelated
    // to how large the input file itself is. A one-shot CLI invocation
    // like this one can easily afford the thread-spawn cost to be safe
    // against that, unlike a hot per-file loop.
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .stack_size(apex_parser::RECOMMENDED_MIN_STACK_SIZE)
        .spawn(move || run_on_worker_thread(&path))
        .expect("failed to spawn worker thread")
        .join()
        .expect("worker thread panicked")
}

fn run_on_worker_thread(path: &Path) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("error reading {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let parse = apex_parser::parse_compilation_unit(&src);
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
