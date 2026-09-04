//! **Temporary** -- delete this module once it confirms parity and is no
//! longer needed (Wayfinder `apex-diagnostics` map, ticket 29's own
//! instructions, mirroring ticket 26's `salsa_stage1_dual_run.rs`).
//! Ticket 25's locked validation technique for the salsa migration:
//! before ever trusting a stage's new salsa-routed data alone, dual-run
//! it against the pre-salsa computation over the real NPSP corpus (1,070
//! files) and assert byte-identical output.
//!
//! Stage 2 moves per-file parsing and Pass 1/Collect from `parse_one`/
//! `collect::collect_compilation_unit`/`collect_trigger_unit`'s direct
//! calls to `crate::db`'s salsa-tracked `parse_query`/`collect_query`.
//! This test feeds the **same** file text down both paths, per file:
//!
//! - "old": `apex_parser::parse_compilation_unit_with_cache`/
//!   `parse_trigger_unit_with_cache` (a fresh `NodeCache` per file, since
//!   this test isn't checking `parse_one`'s fold-sharing behavior, only
//!   that parsing the same text produces the same tree) followed by
//!   `crate::collect::collect_compilation_unit`/`collect_trigger_unit` --
//!   the exact functions `crate::db`'s tracked queries call internally,
//!   just not routed through salsa.
//! - "new": `crate::db::parse_query`/`collect_query`, called against a
//!   `BindDatabase` with that file's text synced in via
//!   `crate::db::sync_file_text_into_db`.
//!
//! Unlike Stage 1's dual-run test, there's no directory-walk-ordering
//! trap to route around here: each file's parse+collect is a pure
//! function of that one file's own text, with no project-wide
//! aggregation whose order could drift between two independent runs.
//!
//! `Parse` gained no new derives for this comparison -- `.syntax().to_string()`
//! (a lossless round-trip of the exact source, see `apex_parser::Parse`'s
//! doc comment) plus `.errors` (already `PartialEq`) is enough to prove
//! two `Parse`s are equivalent without adding a `PartialEq` impl to
//! `apex-parser`'s public `GreenNode`-backed type that nothing else
//! needs. `FileCollection` and `Symbol` did gain `PartialEq`/`Debug`
//! (this ticket) solely so this test can `assert_eq!` them directly,
//! matching `SchemaIndex`/`LabelIndex`/`PageIndex`'s own ticket-26
//! precedent.

#[cfg(test)]
mod tests {
    use crate::db::{self, BindDatabase};
    use apex_parser::NodeCache;
    use rowan::ast::AstNode;
    use std::path::{Path, PathBuf};

    fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
    }

    #[test]
    fn salsa_routed_stage2_data_is_byte_identical_to_the_pre_salsa_computation() {
        let root = corpus_root();
        assert!(
            root.exists(),
            "no NPSP corpus found at {root:?}; is the submodule checked out? \
             (git submodule update --init --recursive)"
        );

        let mut file_table = crate::file_table::FileTable::default();
        let mut db = BindDatabase::default();
        let mut checked = 0usize;

        for path in apex_discover::find_apex_files(&root) {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                // Same silent-drop discipline `parse_one` uses for an
                // unreadable candidate -- not this test's concern.
                Err(_) => continue,
            };
            let trigger = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));
            let file = file_table.id_for(&path);

            // "old"
            let mut node_cache = NodeCache::default();
            let old_parse = if trigger {
                apex_parser::parse_trigger_unit_with_cache(&text, &mut node_cache)
            } else {
                apex_parser::parse_compilation_unit_with_cache(&text, &mut node_cache)
            };
            let old_collection = if trigger {
                apex_syntax::ast::decl::TriggerUnit::cast(old_parse.syntax())
                    .map(|tu| crate::collect::collect_trigger_unit(file, &tu))
                    .unwrap_or_default()
            } else {
                apex_syntax::ast::decl::CompilationUnit::cast(old_parse.syntax())
                    .map(|cu| crate::collect::collect_compilation_unit(file, &cu))
                    .unwrap_or_default()
            };

            // "new"
            let input = db::sync_file_text_into_db(&mut db, None, file, trigger, text.clone());
            let new_parse = db::parse_query(&db, input);
            let new_collection = db::collect_query(&db, input);

            assert_eq!(
                new_parse.syntax().to_string(),
                old_parse.syntax().to_string(),
                "salsa-routed Parse diverged from the pre-salsa direct computation for {path:?}"
            );
            assert_eq!(
                new_parse.errors, old_parse.errors,
                "salsa-routed Parse errors diverged from the pre-salsa direct computation for {path:?}"
            );
            assert_eq!(
                new_collection, old_collection,
                "salsa-routed FileCollection diverged from the pre-salsa direct computation for {path:?}"
            );
            checked += 1;
        }

        assert!(
            checked > 1000,
            "expected to check over 1000 real NPSP files, only checked {checked} -- corpus may be missing/truncated"
        );
    }
}
