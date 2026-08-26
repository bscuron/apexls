//! Whole-corpus reference-site invariant test: binds every real NPSP
//! file (declarations + scopes + reference resolution, the full
//! pipeline) and asserts floor counts per `Resolution` outcome, plus a
//! ceiling on the `Unresolved` ratio -- a floor alone can't catch "used
//! to resolve 90% of references, a regression silently dropped that to
//! 10%, but 10% is still nonzero so the floor still passes." A fixed,
//! narrow numeric target would be too brittle (any grammar/AST change
//! shifts these counts), so the ceiling is set well above the current
//! observed ratio (~20%, down from ~45% once `apex_stdlib`'s bundled
//! standard-library class/method/property schema was wired into
//! `crate::resolve`'s `Ty::System` arms -- both the method-call/field-
//! access lookup itself and `bind_name_expr`'s previously-missing
//! fallback for a bare class name used as a static-call receiver, e.g.
//! `String.isBlank(...)`/`Database.query(...)` -- common enough in real
//! Apex to move the whole-corpus ratio by more than half) rather than
//! pinned to it.

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
    stdlib_member: usize,
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
    for (_, resolution) in program.all_resolutions() {
        match resolution {
            Resolution::Resolved(_) => counts.resolved += 1,
            Resolution::Candidates(_) => counts.candidates += 1,
            Resolution::SchemaObject(_) => counts.schema_object += 1,
            Resolution::UnknownSchema(_) => counts.unknown_schema += 1,
            Resolution::StdlibMember(_) => counts.stdlib_member += 1,
            Resolution::Unresolved => counts.unresolved += 1,
        }
    }

    let total = counts.resolved
        + counts.candidates
        + counts.schema_object
        + counts.unknown_schema
        + counts.stdlib_member
        + counts.unresolved;
    assert!(
        total > 200_000,
        "expected >200,000 total reference resolutions across NPSP, got {total}"
    );

    // `resolved`'s floor sits well above `candidates`' now that call
    // expressions go through arity-then-type overload narrowing
    // (`crate::resolve::narrow_by_overload`): most real overload sets in
    // NPSP are same-name-different-arity, which arity alone resolves,
    // so the bulk of what used to land in `Candidates` moved to
    // `Resolved`. `candidates` still has a real floor -- genuinely
    // ambiguous same-arity overloads (most often disambiguated only by
    // argument types v1 doesn't model, like `Integer`/`String`) remain.
    assert!(
        counts.resolved > 100_000,
        "expected >100,000 Resolved references, got {}: {counts:?}",
        counts.resolved
    );
    assert!(
        counts.candidates > 1_000,
        "expected >1,000 Candidates references, got {}: {counts:?}",
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

    assert!(
        counts.stdlib_member > 60_000,
        "expected >60,000 StdlibMember references, got {}: {counts:?}",
        counts.stdlib_member
    );

    let unresolved_ratio = counts.unresolved as f64 / total as f64;
    assert!(
        unresolved_ratio < 0.70,
        "Unresolved ratio {unresolved_ratio:.2} exceeds the 0.70 ceiling ({}/{total}) -- likely a resolution regression, not just v1's known standard-library gap: {counts:?}",
        counts.unresolved
    );
}
