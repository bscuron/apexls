//! A "golden ratchet" regression guard, complementing (not replacing)
//! `scope_resolution_smoke.rs`'s loose sanity floors/ceiling: pins the
//! exact whole-corpus `Resolved`/`Unresolved` counts as of the commit
//! that added this test, and fails if either one regresses --
//! `unresolved` rising, or `resolved` falling, both mean *something*
//! that used to connect a real reference to its declaration no longer
//! does, project-wide, not just in whichever one file a human happened
//! to be reading when they noticed.
//!
//! This exists because three real, independent resolver bugs
//! (self-nested qualified type names never resolving outside
//! `extends`/`implements`, dead-code detection blind to interface/
//! virtual dynamic dispatch, and an ambiguous overload silently
//! discarding a chained call's type) were each found only because a
//! user happened to notice one specific false positive in one specific
//! real file and reported it -- there was nothing that would have
//! caught any of them automatically, or would notice if a *future*
//! change silently reintroduced a similar gap somewhere else in the
//! corpus. `scope_resolution_smoke.rs`'s existing checks (a 0.70
//! `Unresolved`-ratio *ceiling*, a 100,000 `Resolved` *floor*) are
//! deliberately loose -- generous enough to tolerate normal grammar/AST
//! evolution without becoming flaky -- which is exactly why they never
//! caught any of these three: each individual bug was many orders of
//! magnitude too small to move a ratio that coarse.
//!
//! This test intentionally does the opposite: pin the *exact* current
//! numbers as tightly as possible, so even a small handful of newly-
//! broken references trips it. The tradeoff is exactly the one that
//! implies: it *will* need its constants bumped for entirely legitimate
//! reasons (a grammar fix changes how many reference nodes exist at
//! all, a new `crate::generics`/`crate::conversions` rule correctly
//! resolves something that used to be `Unresolved`, `crate::resolve`'s
//! dynamic-dispatch widening correctly turns more `Resolved` calls into
//! `Candidates`, ...). That's expected maintenance, not a sign this test
//! is too strict -- when a change legitimately moves these numbers,
//! update the constants below in the same commit, with a one-line note
//! on why, the same way any other golden-snapshot test's baseline is
//! maintained. What this test guards against is a change that moves
//! them with *no* accompanying explanation at all.
//!
//! Deliberately does **not** try to distinguish "a real resolver bug"
//! from "a known, still-unmodeled standard-library gap" in what counts
//! toward `Unresolved` -- every `Unresolved` reference counts, stdlib
//! gaps included. That's a feature, not an oversight: when standard-
//! library method modeling is eventually added, that work will lower
//! this exact baseline for a documented reason, giving a precise,
//! accurate record of how much it actually improved -- rather than the
//! count being pre-filtered down to "only the parts that were already
//! believed to be bugs," which would silently hide that improvement.

use apex_binder::{BoundProgram, Resolution};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// Exact whole-corpus counts as of the commit that added this test
/// (after the dotted-nested-type, dynamic-dispatch, and ambiguous-
/// overload-chaining fixes). Update alongside any change that
/// legitimately moves them -- see this module's own doc comment.
const BASELINE_RESOLVED: usize = 204_984;
const BASELINE_UNRESOLVED: usize = 152_539;

#[test]
fn resolved_and_unresolved_counts_never_regress_from_their_pinned_baseline() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let program = BoundProgram::from_files(&root);

    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    for (_, resolution) in program.all_resolutions() {
        match resolution {
            Resolution::Resolved(_) => resolved += 1,
            Resolution::Unresolved => unresolved += 1,
            Resolution::Candidates(_) | Resolution::SchemaObject(_) | Resolution::UnknownSchema(_) => {}
        }
    }

    assert!(
        resolved >= BASELINE_RESOLVED,
        "Resolved count dropped from {BASELINE_RESOLVED} to {resolved} ({} fewer) -- likely a \
         real resolution regression somewhere in crate::resolve/crate::inherit, not just \
         incidental drift: something that used to connect a reference to its declaration no \
         longer does. If this drop is expected (e.g. a grammar/AST change legitimately changed \
         how references are counted), update BASELINE_RESOLVED with a one-line note on why.",
        BASELINE_RESOLVED - resolved
    );
    assert!(
        unresolved <= BASELINE_UNRESOLVED,
        "Unresolved count rose from {BASELINE_UNRESOLVED} to {unresolved} ({} more) -- likely a \
         real resolution regression: something that used to resolve (or at least land in \
         Candidates/SchemaObject/UnknownSchema) no longer does. If this rise is expected, update \
         BASELINE_UNRESOLVED with a one-line note on why.",
        unresolved - BASELINE_UNRESOLVED
    );
}
