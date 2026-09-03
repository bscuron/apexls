//! **Temporary** -- delete this module once it confirms parity and is no
//! longer needed (Wayfinder `apex-diagnostics` map, ticket 26's own
//! instructions, mirroring ticket 24's `salsa_smoke.rs`). Ticket 25's
//! locked validation technique for the salsa migration: before ever
//! trusting a stage's new salsa-routed data alone, dual-run it against
//! the pre-salsa computation over the real NPSP corpus (1,035 files) and
//! assert byte-identical output.
//!
//! Stage 1 moved `SchemaIndex`/`LabelIndex`/`PageIndex`/
//! `vf_referenced_classes` from `BindCache`'s own hand-rolled fields to
//! `crate::db`'s salsa-tracked queries. This test builds **one**
//! `apex_discover::Discovery` and feeds it down both paths:
//!
//! - "old": `SchemaIndex::from_discovery`/`LabelIndex::from_discovery`/
//!   `PageIndex::from_discovery`/`apex_metadata::visualforce::referenced_controller_classes`,
//!   called directly -- the exact functions `crate::db`'s tracked queries
//!   call internally, just not routed through salsa.
//! - "new": `crate::db::schema_index`/`label_index`/`page_index`/
//!   `vf_referenced_classes`, called against a `BindDatabase` synced with
//!   the *same* `Discovery` via `crate::db::sync_discovery_into_db`.
//!
//! Deliberately does **not** call `apex_discover::discover` twice (once
//! directly, once again inside `BoundProgram::from_files`) the way a
//! first version of this test did: `ignore`'s parallel walker returns
//! `Discovery::object_meta_files`/`field_meta_files` in a run-to-run-
//! nondeterministic order (confirmed directly -- two independent
//! `discover()` calls over the same tree produced different orderings,
//! and `SchemaIndex::from_discovery` given two differently-ordered but
//! semantically identical `Discovery`s produces `SchemaIndex`es whose
//! per-object `fields: Vec<FieldSchema>` differ in *order*, tripping
//! `Vec`'s order-sensitive `PartialEq` even though nothing about the
//! actual schema changed). That's a real, pre-existing property of
//! `apex-discover`/`apex-metadata`'s own walk-order handling, unrelated
//! to this ticket's salsa seam -- comparing against a second independent
//! walk would make this test flaky for a reason that has nothing to do
//! with what it's actually meant to check, so both paths below are fed
//! the identical, single `Discovery` instead.
//!
//! `SchemaIndex`/`LabelIndex`/`PageIndex`/`VisualforcePage` gained
//! `PartialEq`/`Debug` derives (this ticket) solely so this comparison
//! can `assert_eq!` them directly -- nothing else in the crate needs to
//! compare two instances of any of these.

#[cfg(test)]
mod tests {
    use crate::db::{self, BindDatabase};
    use crate::{LabelIndex, PageIndex, SchemaIndex};
    use std::path::{Path, PathBuf};

    fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
    }

    #[test]
    fn salsa_routed_stage1_data_is_byte_identical_to_the_pre_salsa_computation() {
        let root = corpus_root();
        assert!(
            root.exists(),
            "no NPSP corpus found at {root:?}; is the submodule checked out? \
             (git submodule update --init --recursive)"
        );

        let discovery = apex_discover::discover(&root);

        let old_schema = SchemaIndex::from_discovery(&discovery);
        let old_labels = LabelIndex::from_discovery(&discovery);
        let old_pages = PageIndex::from_discovery(&discovery);
        let old_vf_referenced_classes =
            apex_metadata::visualforce::referenced_controller_classes(&discovery.page_files);

        let mut salsa_db = BindDatabase::default();
        let input = db::sync_discovery_into_db(&mut salsa_db, None, &discovery);

        assert_eq!(
            *db::schema_index(&salsa_db, input),
            old_schema,
            "salsa-routed SchemaIndex diverged from the pre-salsa direct computation"
        );
        assert_eq!(
            *db::label_index(&salsa_db, input),
            old_labels,
            "salsa-routed LabelIndex diverged from the pre-salsa direct computation"
        );
        assert_eq!(
            *db::page_index(&salsa_db, input),
            old_pages,
            "salsa-routed PageIndex diverged from the pre-salsa direct computation"
        );
        assert_eq!(
            *db::vf_referenced_classes(&salsa_db, input),
            old_vf_referenced_classes,
            "salsa-routed vf_referenced_classes diverged from the pre-salsa direct computation"
        );
    }
}
