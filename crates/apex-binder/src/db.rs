//! The salsa seam (Wayfinder `apex-diagnostics` map, ticket 26): real
//! `salsa` inputs and tracked queries for [`crate::incremental::BindCache`]'s
//! rowan-free, directory-walk-derived data -- `SchemaIndex`/`LabelIndex`/
//! `PageIndex`/`vf_referenced_classes` -- Stage 1 of ticket 25's locked
//! salsa-migration plan, plus (ticket 29) Stage 2's per-file parsing and
//! Pass 1/Collect. A module, not a crate, per ticket 25's own answer:
//! this codebase splits every internal concern this way already
//! (`schema_index`, `label_index`, `page_index`, ... -- no precedent for
//! spinning one out on its own).
//!
//! `BindCache` (`crate::incremental`) stays the sole staleness authority
//! throughout the staged migration -- this module is pure memoization
//! sitting downstream of `need_fresh_discovery`'s (Stage 1) and each
//! file's own `Freshness` check's (Stage 2) existing dirty detection,
//! never its own independent staleness signal. [`sync_discovery_into_db`]/
//! [`sync_file_text_into_db`] are the only places fresh data ever reaches
//! this database, called exactly where `crate::BoundProgram::from_files_cached`
//! used to write straight into `BindCache`'s own (now-deleted) `schema`/
//! `labels`/`pages`/`vf_referenced_classes`/`parses` fields.
//!
//! Every Stage 1 tracked query below is `no_eq`: none of `SchemaIndex`/
//! `LabelIndex`/`PageIndex`/`HashSet<String>` need to compare their own
//! *content* for salsa's early-cutoff, since they only ever recompute at
//! all when [`DiscoveryInput`] itself changed -- which already means the
//! discovered file set changed, per `need_fresh_discovery`'s own
//! contract. Comparing a rebuilt `SchemaIndex` field-by-field against its
//! predecessor on every such call would cost real time for a cutoff that
//! can, by construction, never actually fire. [`parse_query`]/
//! [`collect_query`] are `no_eq` for the identical reason, one layer
//! down: [`FileTextInput`] only ever gets a fresh `.set_text()` call for
//! a file `crate::incremental::Freshness` already confirmed changed.
//!
//! [`PARSE_NODE_CACHE`] replaces `parse_one`'s old `rayon::fold()`-scoped
//! `apex_parser::NodeCache` sharing (ticket 27/28): salsa's clone-per-
//! thread parallel model has no `fold()` accumulator to share one
//! through, so each `rayon` worker thread gets its own persistent
//! `NodeCache` instead, reused across every file that thread ever parses
//! -- validated (ticket 28, `examples/salsa_nodecache_prototype.rs`) as
//! faster than the old fold-scoped sharing on a cold corpus parse (it
//! shares across a whole run per thread, not just one fold segment), with
//! no material warm-single-edit regression.

use crate::collect::FileCollection;
use crate::file_id::FileId;
use crate::label_index::LabelIndex;
use crate::page_index::PageIndex;
use crate::schema_index::SchemaIndex;
use apex_discover::Discovery;
use apex_parser::{NodeCache, Parse};
use apex_syntax::ast::decl::{CompilationUnit, TriggerUnit};
use rowan::ast::AstNode;
use salsa::Setter;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

// One `NodeCache` per `rayon` worker thread, persisted across every
// file that thread ever parses for the life of the process -- see the
// module doc comment.
thread_local! {
    static PARSE_NODE_CACHE: RefCell<NodeCache> = RefCell::new(NodeCache::default());
}

/// `Clone` (restored -- dropped after Stage 1 as unneeded, ticket 26's
/// own note) is what makes Stage 2's per-file parallel reads possible at
/// all: `Storage<Db>` isn't `Sync` (confirmed, ticket 28), so a `rayon`
/// closure can never hold a *shared reference* to one `db` and clone it
/// per call -- every clone needed for a parallel section must be made
/// up front, owned per item, before entering that section (salsa's own
/// test precedent: `db_t1`/`db_t2`, cloned and moved into separate
/// threads, never borrowed). Still deliberately never handed to `rayon`
/// *by shared reference* -- only ever by value, one to-be-moved clone
/// per task. A salsa database's thread-local "attached" state
/// (`salsa::attach`) panics if a *different* database tries to attach on
/// a thread that already has one attached, which a shared, process-wide
/// `rayon` pool can trigger the moment two unrelated `BindDatabase`s
/// (e.g. two concurrently-running `BoundProgram::from_files` calls) are
/// both in play -- confirmed via a real "Cannot change database
/// mid-query" panic under `cargo test`'s parallel test threads before
/// Stage 1's four whole-project queries were made sequential; Stage 2's
/// per-file queries stay parallel by giving every task its own clone
/// instead.
#[salsa::db]
#[derive(Default, Clone)]
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

/// One file's text plus the two facts fixed at discovery time that its
/// parse depends on (`file`, for `Symbol`/`SyntaxPtr` identity;
/// `trigger`, to pick `parse_trigger_unit_with_cache` vs
/// `parse_compilation_unit_with_cache`) -- bundled into one input rather
/// than three separate ones since all three only ever change together
/// (a `FileId`/trigger-ness is fixed for the life of a path; only `text`
/// is ever re-set). One instance per currently-known file, created once
/// and reused (its `text` field overwritten, never recreated) via
/// [`sync_file_text_into_db`] -- creating a fresh input per edit would
/// give every query a new identity to memoize against, defeating
/// memoization entirely, the same reason [`DiscoveryInput`] is reused
/// rather than recreated.
#[salsa::input]
pub(crate) struct FileTextInput {
    file: FileId,
    trigger: bool,
    text: String,
}

/// Pushes a freshly-read file's text into `db`, creating `existing`'s
/// [`FileTextInput`] the first time this is called for that file and
/// setting its `text` field every time after. The one adapter function
/// `crate::incremental::Freshness`'s existing dirty-detection write-site
/// (`crate::BoundProgram::from_files_cached`) calls for a dirty file,
/// instead of writing straight into `BindCache`'s own (now-deleted)
/// `parses` field -- ticket 27's locked coexistence strategy, one layer
/// down from [`sync_discovery_into_db`]. Every call site sets every
/// dirty file's input *before* any parallel read phase begins (never
/// interleaved with one) -- ticket 27's confirmed load-bearing gotcha:
/// setting a salsa input cancels every other in-flight query on every
/// other clone of `db` and blocks until they drop.
pub(crate) fn sync_file_text_into_db(
    db: &mut BindDatabase,
    existing: Option<FileTextInput>,
    file: FileId,
    trigger: bool,
    text: String,
) -> FileTextInput {
    match existing {
        Some(input) => {
            input.set_text(db).to(text);
            input
        }
        None => FileTextInput::new(db, file, trigger, text),
    }
}

/// Parses one file's text, routed through this thread's persistent
/// [`PARSE_NODE_CACHE`] (see the module doc comment) rather than a fresh
/// or `fold()`-scoped one. `returns(clone)`: `Parse` is cheap to clone
/// (`Arc`-based `GreenNode` plus a `Vec<ParseError>`, see its own doc
/// comment) and every call site wants an owned copy, not a `db`-lifetime-
/// tied reference.
#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn parse_query(db: &dyn salsa::Database, input: FileTextInput) -> Parse {
    PARSE_NODE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if *input.trigger(db) {
            apex_parser::parse_trigger_unit_with_cache(input.text(db), &mut cache)
        } else {
            apex_parser::parse_compilation_unit_with_cache(input.text(db), &mut cache)
        }
    })
}

/// Pass 1/Collect for one file, built on top of [`parse_query`] (salsa
/// memoizes the inner call the same as any other caller -- no separate
/// pass over the tree). Mirrors `crate::collect::collect_compilation_unit`/
/// `collect_trigger_unit`'s old direct call sites in
/// `crate::BoundProgram::from_files_cached` exactly, just routed through
/// this file's tracked [`Parse`] instead of a plain function's local one.
#[salsa::tracked(no_eq, returns(clone))]
pub(crate) fn collect_query(db: &dyn salsa::Database, input: FileTextInput) -> FileCollection {
    let parse = parse_query(db, input);
    let root = parse.syntax();
    let file = *input.file(db);
    if *input.trigger(db) {
        TriggerUnit::cast(root)
            .map(|tu| crate::collect::collect_trigger_unit(file, &tu))
            .unwrap_or_default()
    } else {
        CompilationUnit::cast(root)
            .map(|cu| crate::collect::collect_compilation_unit(file, &cu))
            .unwrap_or_default()
    }
}
