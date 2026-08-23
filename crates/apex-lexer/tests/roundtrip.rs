//! Round-trip (lossless) test against a real-world corpus.
//!
//! The lexer never discards a byte: whitespace and comments are trivia
//! tokens, not skipped, and every token is a zero-copy span into the
//! original source (case preserved, not normalized). So concatenating
//! every token's text back together should reproduce the source
//! *exactly* -- byte-for-byte, not just modulo case/whitespace. This
//! needs no reference oracle; see the apexls verification plan's
//! "round-trip / lossless testing" technique.

use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn collect_apex_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_apex_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cls") || ext.eq_ignore_ascii_case("trigger")
            })
        {
            out.push(path);
        }
    }
}

#[test]
fn tokenizing_npsp_round_trips_exactly() {
    let root = corpus_root();
    let mut files = Vec::new();
    collect_apex_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no .cls/.trigger files found under {}; is the NPSP submodule checked out? \
         (git submodule update --init --recursive)",
        root.display()
    );

    let mut failures = Vec::new();
    let mut checked = 0usize;
    for path in &files {
        // Apex source should be UTF-8; skip anything that isn't rather than
        // letting one fixture's encoding derail the whole corpus run.
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        checked += 1;

        let rebuilt: String = apex_lexer::tokenize(&source)
            .into_iter()
            .map(|t| t.text(&source))
            .collect();

        if rebuilt != source {
            failures.push(path.clone());
        }
    }

    assert!(
        failures.is_empty(),
        "{}/{checked} corpus files failed to round-trip exactly:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}
