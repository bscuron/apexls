//! The salsa seam (Wayfinder `apex-diagnostics` map, ticket 26): real
//! `salsa` inputs and tracked queries for [`crate::incremental::BindCache`]'s
//! rowan-free, directory-walk-derived data -- `SchemaIndex`/`LabelIndex`/
//! `PageIndex`/`vf_referenced_classes` -- Stage 1 of ticket 25's locked
//! salsa-migration plan. A module, not a crate, per ticket 25's own
//! answer: this codebase splits every internal concern this way already
//! (`schema_index`, `label_index`, `page_index`, ... -- no precedent for
//! spinning one out on its own).
//!
//! `BindCache` (`crate::incremental`) stays the sole staleness authority
//! throughout the staged migration -- this module is pure memoization
//! sitting downstream of `need_fresh_discovery`'s existing dirty
//! detection, never its own independent staleness signal.
//! [`sync_discovery_into_db`] is the only place a fresh [`Discovery`]
//! ever reaches this database, called exactly where
//! `crate::BoundProgram::from_files_cached` used to write straight into
//! `BindCache`'s own (now-deleted) `schema`/`labels`/`pages`/
//! `vf_referenced_classes` fields.
//!
//! Every tracked query below is `no_eq`: none of `SchemaIndex`/
//! `LabelIndex`/`PageIndex`/`HashSet<String>` need to compare their own
//! *content* for salsa's early-cutoff, since they only ever recompute at
//! all when [`DiscoveryInput`] itself changed -- which already means the
//! discovered file set changed, per `need_fresh_discovery`'s own
//! contract. Comparing a rebuilt `SchemaIndex` field-by-field against its
//! predecessor on every such call would cost real time for a cutoff that
//! can, by construction, never actually fire.

use crate::label_index::LabelIndex;
use crate::page_index::PageIndex;
use crate::schema_index::SchemaIndex;
use apex_discover::Discovery;
use salsa::Setter;
use std::collections::HashSet;
use std::sync::Arc;

/// Deliberately never handed to `rayon` (no `par_iter`/`join` call in
/// this crate captures a `BindDatabase` or calls any query below from
/// inside one): a salsa database's thread-local "attached" state
/// (`salsa::attach`) panics if a *different* database tries to attach on
/// a thread that already has one attached, which a shared, process-wide
/// `rayon` pool can trigger the moment two unrelated `BindDatabase`s
/// (e.g. two concurrently-running `BoundProgram::from_files` calls) are
/// both in play -- confirmed via a real "Cannot change database
/// mid-query" panic under `cargo test`'s parallel test threads before
/// every query below was made sequential.
#[salsa::db]
#[derive(Default)]
pub(crate) struct BindDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for BindDatabase {}

/// One call's worth of discovered files -- the salsa input every Stage 1
/// query is downstream of. A single instance is created the first time a
/// project is bound; its `discovery` field is overwritten (never
/// recreated) on every later fresh walk, via [`sync_discovery_into_db`] --
/// creating a fresh `DiscoveryInput` per call would give every query a
/// new identity to memoize against, defeating memoization entirely.
#[salsa::input]
pub(crate) struct DiscoveryInput {
    discovery: Discovery,
}

/// Pushes a freshly-walked `discovery` into `db`, creating `existing`'s
/// [`DiscoveryInput`] the first time this is called for `db` and setting
/// its field every time after. The one adapter function
/// `need_fresh_discovery`'s write-site (`crate::BoundProgram::from_files_cached`)
/// calls instead of writing straight into `BindCache`'s own fields, per
/// ticket 25's locked coexistence strategy.
pub(crate) fn sync_discovery_into_db(
    db: &mut BindDatabase,
    existing: Option<DiscoveryInput>,
    discovery: &Discovery,
) -> DiscoveryInput {
    match existing {
        Some(input) => {
            input.set_discovery(db).to(discovery.clone());
            input
        }
        None => DiscoveryInput::new(db, discovery.clone()),
    }
}

// `returns(clone)` (an `Arc` clone -- cheap, a pointer/refcount copy, not
// a deep one): the default tracked-fn return mode hands back a
// `db`-lifetime-tied reference, but every call site here wants to store
// an owned `Arc` straight into `BoundProgram`, the same shape
// `BindCache`'s deleted fields used to hand back.
#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn schema_index(db: &dyn salsa::Database, input: DiscoveryInput) -> Arc<SchemaIndex> {
    Arc::new(SchemaIndex::from_discovery(input.discovery(db)))
}

#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn label_index(db: &dyn salsa::Database, input: DiscoveryInput) -> Arc<LabelIndex> {
    Arc::new(LabelIndex::from_discovery(input.discovery(db)))
}

#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn page_index(db: &dyn salsa::Database, input: DiscoveryInput) -> Arc<PageIndex> {
    Arc::new(PageIndex::from_discovery(input.discovery(db)))
}

#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn vf_referenced_classes(
    db: &dyn salsa::Database,
    input: DiscoveryInput,
) -> Arc<HashSet<String>> {
    Arc::new(apex_metadata::visualforce::referenced_controller_classes(
        &input.discovery(db).page_files,
    ))
}
