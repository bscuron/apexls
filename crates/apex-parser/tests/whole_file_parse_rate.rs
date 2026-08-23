//! Whole-file coverage gate: every real `.cls`/`.trigger` compilation unit
//! in the NPSP corpus must parse with zero errors. This is the permanent
//! form of the ad hoc measurement used while building out Phase 3/4
//! (declarations, SOQL, SOSL) -- kept as a regression test so a future
//! change that narrows the grammar (e.g. a bad `ids::is_id_kind` edit)
//! fails CI immediately instead of silently regressing corpus coverage.
//!
//! A handful of NPSP files are deliberately excluded: `scripts/*.cls` and
//! `datasets/rd2/config_npsp_for_ldv_data_load.cls` are anonymous-Apex
//! scripts (bare top-level statements, meant to be run via `sf apex run
//! --file`, not deployed) that happen to carry a `.cls` extension for
//! tooling convenience. They are not valid `compilationUnit`s under the
//! reference grammar -- the real Salesforce compiler would reject them
//! too if deployed as-is -- so failing to parse them is correct, not a
//! gap.

use std::collections::HashMap;

fn is_known_non_compilation_unit(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/scripts/") || s.ends_with("datasets/rd2/config_npsp_for_ldv_data_load.cls")
}

#[test]
fn every_real_npsp_file_parses_with_zero_errors() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let files = apex_discover::find_apex_files(&root);
    assert!(
        !files.is_empty(),
        "expected the NPSP submodule to be checked out"
    );

    let mut total = 0usize;
    let mut clean = 0usize;
    let mut error_counts: HashMap<String, usize> = HashMap::new();
    let mut sample_errors: HashMap<String, String> = HashMap::new();

    for path in &files {
        if is_known_non_compilation_unit(path) {
            continue;
        }
        total += 1;
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let is_trigger = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
        let parse = if is_trigger {
            apex_parser::parse_trigger_unit(&src)
        } else {
            apex_parser::parse_compilation_unit(&src)
        };
        if parse.errors.is_empty() {
            clean += 1;
        } else {
            let msg = &parse.errors[0].message;
            let key = msg.split(", found").next().unwrap_or(msg).to_string();
            *error_counts.entry(key.clone()).or_insert(0) += 1;
            sample_errors
                .entry(key)
                .or_insert_with(|| format!("{}: {}", path.display(), msg));
        }
    }

    if clean != total {
        let mut counts: Vec<_> = error_counts.into_iter().collect();
        counts.sort_by_key(|a| std::cmp::Reverse(a.1));
        eprintln!(
            "{clean}/{total} whole files parse with zero errors ({:.1}%)",
            100.0 * clean as f64 / total as f64
        );
        eprintln!("\ntop first-error causes:");
        for (key, count) in counts.iter().take(25) {
            eprintln!("  {count:5}  {key}");
            if let Some(sample) = sample_errors.get(key) {
                eprintln!("         e.g. {sample}");
            }
        }
    }

    assert_eq!(
        clean, total,
        "not every real NPSP compilation unit parsed cleanly -- see stderr for a breakdown"
    );
}
