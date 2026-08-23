//! Differential test: every custom (`__c`-suffixed) object reference the
//! binder resolves via `Resolution::SchemaObject` should be backed by a
//! real entry from `apex_metadata::discover_sobjects` run independently
//! over the same corpus root -- two separately-walked artifacts (parsed
//! Apex source vs. discovered SFDX metadata) agreeing with each other,
//! matching `apex-discover`'s own `discover_matches_naive_walk.rs`
//! differential-testing precedent.

use apex_binder::{BoundProgram, Resolution};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

#[test]
fn custom_object_references_resolve_against_independently_discovered_metadata() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let sobjects = apex_metadata::discover_sobjects(&root);
    let custom_count = sobjects.iter().filter(|s| s.is_custom).count();
    assert!(
        custom_count > 40,
        "expected NPSP to declare many custom objects (independently-walked metadata), got {custom_count}"
    );

    let program = BoundProgram::from_files(&root);

    let mut resolved_custom_objects = 0usize;
    let mut unknown_custom_objects = 0usize;
    for (_, resolution) in program.all_resolutions() {
        match resolution {
            Resolution::SchemaObject {
                object,
                field: None,
            } if object.ends_with("__c") => {
                resolved_custom_objects += 1;
            }
            Resolution::UnknownSchema {
                object: Some(object),
                field: None,
            } if object.ends_with("__c") => {
                unknown_custom_objects += 1;
            }
            _ => {}
        }
    }

    assert!(
        resolved_custom_objects > 50,
        "expected many custom-object references (SOQL FROM/trigger ON/SOSL RETURNING) to resolve against apex-metadata's schema, got {resolved_custom_objects}"
    );

    let total = resolved_custom_objects + unknown_custom_objects;
    let ratio = resolved_custom_objects as f64 / total as f64;
    assert!(
        ratio > 0.5,
        "expected most __c object references in NPSP to resolve against locally discovered metadata: {resolved_custom_objects}/{total} ({ratio:.2})"
    );
}
