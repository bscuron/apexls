//! Whole-corpus reference-site invariant test: binds every real NPSP
//! file (declarations + scopes + reference resolution, the full
//! pipeline) and asserts floor counts per `Resolution` outcome, plus a
//! ceiling on the `Unresolved` ratio -- a floor alone can't catch "used
//! to resolve 90% of references, a regression silently dropped that to
//! 10%, but 10% is still nonzero so the floor still passes." A fixed,
//! narrow numeric target would be too brittle (any grammar/AST change
//! shifts these counts), so the ceiling is set well above the current
//! observed ratio (~55%) rather than pinned to it.

use apex_binder::{BoundProgram, Resolution};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

#[derive(Default, Debug)]
struct Counts {
    resolved: usize,
    candidates: usize,
    schema_object: usize,
    unknown_schema: usize,
    unresolved: usize,
}

#[test]
fn every_real_npsp_file_binds_and_resolves_a_meaningful_share_of_references() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let program = BoundProgram::from_files(&root);

    let mut counts = Counts::default();
    for (_, resolution) in program.refs.iter() {
        match resolution {
            Resolution::Resolved(_) => counts.resolved += 1,
            Resolution::Candidates(_) => counts.candidates += 1,
            Resolution::SchemaObject { .. } => counts.schema_object += 1,
            Resolution::UnknownSchema { .. } => counts.unknown_schema += 1,
            Resolution::Unresolved => counts.unresolved += 1,
        }
    }

    let total = counts.resolved
        + counts.candidates
        + counts.schema_object
        + counts.unknown_schema
        + counts.unresolved;
    assert!(
        total > 200_000,
        "expected >200,000 total reference resolutions across NPSP, got {total}"
    );

    assert!(
        counts.resolved > 50_000,
        "expected >50,000 Resolved references, got {}: {counts:?}",
        counts.resolved
    );
    assert!(
        counts.candidates > 5_000,
        "expected >5,000 Candidates references, got {}: {counts:?}",
        counts.candidates
    );
    assert!(
        counts.schema_object > 5_000,
        "expected >5,000 SchemaObject references, got {}: {counts:?}",
        counts.schema_object
    );
    assert!(
        counts.unknown_schema > 2_000,
        "expected >2,000 UnknownSchema references, got {}: {counts:?}",
        counts.unknown_schema
    );

    let unresolved_ratio = counts.unresolved as f64 / total as f64;
    assert!(
        unresolved_ratio < 0.70,
        "Unresolved ratio {unresolved_ratio:.2} exceeds the 0.70 ceiling ({}/{total}) -- likely a resolution regression, not just v1's known standard-library gap: {counts:?}",
        counts.unresolved
    );
}
