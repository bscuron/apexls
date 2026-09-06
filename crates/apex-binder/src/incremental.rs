//! [`BindCache`]: everything [`crate::BoundProgram::from_files_cached`]
//! remembers between calls so it can skip redoing work an edit couldn't
//! possibly have affected. Superset of the earlier `ParseCache` (parsed
//! trees only) added in `BACKLOG.md` §2 Step 1/2 -- this also persists
//! the project's [`SymbolTable`] itself plus every file's Pass 1.5 input
//! (`raw_extends`/`raw_super`) and Pass 2 output (refs/scopes), patched
//! file-by-file rather than rebuilt from nothing every call. See
//! `crate::symbol::SymbolId`'s and `crate::file_table::FileTable`'s doc
//! comments for the stable-identity foundation this relies on.

use crate::db::{BindDatabase, DiscoveryInput, FileSetInput, FileTextInput, RawInheritanceInputs};
use crate::file_id::FileId;
use crate::file_table::FileTable;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::reference_table::ReferenceTable;
use crate::scope::ScopeTree;
use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use apex_discover::Discovery;
use apex_parser::Parse;
use apex_syntax::ast::Type;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use smol_str::SmolStr;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// A cheap, whole-content fingerprint for an `overrides`-supplied buffer's
/// "is this unchanged since last time" check -- see [`BindCache::parses`]'s
/// doc comment for why a hash replaced a stored `String` copy, and the
/// honest tradeoff that comes with it. Only used for `overrides` entries
/// ([`Freshness::ContentHash`]): an unsaved editor buffer has no
/// filesystem metadata to check instead, but there's normally at most a
/// handful of these per call (the file(s) actually being edited), so
/// hashing them costs nothing worth avoiding.
pub(crate) fn content_fingerprint(content: &str) -> u64 {
    let mut hasher = FxHasher::default();
    hasher.write(content.as_bytes());
    hasher.finish()
}

/// What [`BindCache::parses`] compares against to decide whether a
/// cached `Parse` can be reused as-is. Two independent bases, matched by
/// variant (a mismatch -- e.g. a file that used to arrive via
/// `overrides` and now doesn't -- always falls through to "treat as
/// changed," never silently compares across kinds):
///
/// - [`Freshness::Stat`]: a file read from disk. `len`/`modified` come
///   from one `std::fs::metadata` call -- cheap enough to check *before*
///   reading the file at all, so a stat match skips the read (and any
///   hash) entirely for every unaffected file, not just the reparse.
///   This is the same size+mtime staleness check `make`/Cargo's own
///   fingerprinting/most incremental build systems use, with the same
///   honest caveat: a filesystem with coarse mtime resolution (or an
///   external tool that preserves mtime after rewriting a file with the
///   same length) could in principle produce a false "unchanged." No
///   worse than -- and for this project's dominant edit-in-the-editor
///   workflow, strictly better than -- the already-accepted "no
///   filesystem-watcher" staleness limit `BoundProgram::from_files_cached`'s
///   doc comment already documents for exactly this class of out-of-band
///   disk change.
/// - [`Freshness::ContentHash`]: an `overrides`-supplied buffer, checked
///   via [`content_fingerprint`] instead, since an unsaved buffer has no
///   metadata to stat.
#[derive(Clone, PartialEq)]
pub(crate) enum Freshness {
    Stat { len: u64, modified: SystemTime },
    ContentHash(u64),
}

/// One file's Pass 2 output: every reference resolved and every body's
/// scope tree, for bodies declared in that one file. Replaced wholesale
/// whenever that file gets rebound (see `BoundProgram::from_files_cached`),
/// which is also why every entry in [`BindCache::bodies`] and
/// `BoundProgram`'s own copy is `Arc`-wrapped -- assembling a
/// `BoundProgram` snapshot only needs to clone the `Arc` (a pointer) for
/// every *unaffected* file, not walk and clone its whole reference table
/// and every scope tree in it.
#[derive(Default)]
pub(crate) struct FileBodies {
    pub(crate) refs: ReferenceTable,
    pub(crate) scopes: FxHashMap<SyntaxPtr, ScopeTree>,
    /// Every provable type-checking defect found inline while binding
    /// this file's bodies -- see `crate::resolve::TypeMismatch`'s own
    /// doc comment (Wayfinder `apex-diagnostics` map, ticket 23).
    pub(crate) type_mismatches: Vec<crate::resolve::TypeMismatch>,
}

#[derive(Default)]
pub struct BindCache {
    pub(crate) files: FileTable,
    /// The last directory walk (`apex_files`/`object_meta_files`/
    /// `field_meta_files`) and the `SchemaIndex` built from it, reused
    /// across calls instead of re-walking the whole tree (and re-parsing
    /// every SFDX metadata XML file) unconditionally on every edit.
    /// `None` only before the first call. See
    /// `BoundProgram::from_files_cached`'s doc comment for exactly when
    /// this gets refreshed on its own, and what staleness it otherwise
    /// accepts as an honest v1 limit (a file added on disk but never
    /// opened in the editor, or metadata XML edited with no
    /// corresponding Apex-file signal) -- [`Self::invalidate_discovery`]
    /// is the hook a caller with its own out-of-band change signal (a
    /// filesystem watcher) uses to force a fresh walk instead of relying
    /// on that limit.
    /// **Invariant**: written only from `BoundProgram::from_files_cached`'s
    /// single `need_fresh_discovery` block, always in the same statement
    /// group as `discovery_input`/`db` below -- the three stay in lockstep
    /// by construction (one write site), not by a shared type enforcing
    /// it. [`Self::invalidate_discovery`] resetting only this field to
    /// `None` is still sound: it just forces that one write site to fire
    /// again next call, which re-syncs `discovery_input`/`db` too: it
    /// never needs its own separate reset.
    pub(crate) discovery: Option<Discovery>,
    /// The salsa database backing `SchemaIndex`/`LabelIndex`/`PageIndex`/
    /// `vf_referenced_classes` (Wayfinder `apex-diagnostics` map, ticket
    /// 26's Stage 1 cutover -- see `crate::db`'s module doc comment).
    /// `discovery` above stays the sole staleness authority: this is pure
    /// memoization downstream of it, synced via `crate::db::sync_discovery_into_db`
    /// exactly where a fresh walk happens, never invalidated on its own.
    pub(crate) db: BindDatabase,
    /// `discovery`'s own salsa-input identity, `None` only before the
    /// first fresh walk -- reused (its field overwritten, never
    /// recreated) on every later one so `db`'s tracked queries keep
    /// memoizing against the same input. Same lockstep invariant as
    /// `discovery` above -- see its own doc comment.
    pub(crate) discovery_input: Option<DiscoveryInput>,
    /// Each path's last-seen [`Freshness`] -- a file is skipped (no read,
    /// no salsa input write, no reparse) whenever this call's freshly
    /// computed `Freshness` still matches. Ticket 29 (Wayfinder
    /// `apex-diagnostics` map, Stage 2 of the salsa migration) narrowed
    /// this from the old `parses` field's `(Freshness, Parse)` pair down
    /// to `Freshness` alone: the actual `Parse` this used to also cache
    /// is now `crate::db::parse_query`'s own salsa-memoized job (see
    /// [`Self::file_text_inputs`]) -- keeping a second, redundant copy
    /// here would just be two caches doing the same job. No separate copy
    /// of the content itself is kept here either (real, measured memory
    /// cost -- see `examples/mem_profile.rs`): see [`Freshness`]'s doc
    /// comment for what's compared instead.
    pub(crate) freshness: FxHashMap<PathBuf, Freshness>,
    /// Every currently-live file's path, keyed by its stable `FileId` --
    /// persisted and patched file-by-file (dirty/removed only) across
    /// calls, same discipline as `table`/`bodies` below, so
    /// `BoundProgram::from_files_cached` only has to `.clone()` this map
    /// once per call to assemble its snapshot instead of rebuilding it
    /// with a fresh `PathBuf` clone per file, every file, every call --
    /// see the `hotpath`-measured finding in `BACKLOG.md` §2 that
    /// motivated this (69% of a warm single-edit rebind's allocated
    /// bytes traced to exactly this rebuild).
    pub(crate) paths: FxHashMap<FileId, PathBuf>,
    /// Reverse of `paths`, patched alongside it.
    pub(crate) path_ids: FxHashMap<PathBuf, FileId>,
    /// Every recently-touched file's last-bound `Parse`, keyed by `FileId`
    /// -- the `BoundProgram`-facing counterpart to `freshness` above
    /// (which is keyed by `PathBuf`, purely for that field's own
    /// staleness check). Patched for dirty files each call the same way
    /// `table`/`bodies` below are -- an unaffected file's entry is already
    /// correct and never re-queried -- but, unlike them, **not** kept
    /// forever: ticket 07 of `.scratch/apex-performance/` (following
    /// ticket 29's own precedent of routing every file through
    /// `crate::db::parse_query` measuring slower for the warm single-edit
    /// path) found this map's own permanent, never-evicted retention was
    /// the real reason apexls-server's steady-state rowan-tree memory
    /// never shrinks even for a file no LSP request or rebind has touched
    /// in a long time. [`Self::evict_stale_parses`] now prunes any entry
    /// [`PARSE_EVICTION_WINDOW`] rebinds stale, called once per
    /// `crate::BoundProgram::from_files_cached` call. A miss here (either
    /// because a file was never dirty yet, or because it was evicted)
    /// falls back to `crate::db::parse_query`'s own salsa memoization
    /// (bounded by its own `lru` cap, see that function's doc comment) --
    /// re-populated into this map on that fetch, so a file touched again
    /// soon after eviction doesn't pay a repeated cache-miss cost every
    /// single call.
    pub(crate) file_parses: FxHashMap<FileId, Parse>,
    /// Bumped once per `crate::BoundProgram::from_files_cached` call --
    /// [`Self::parse_last_used`]'s clock. See [`Self::evict_stale_parses`].
    pub(crate) parse_generation: u64,
    /// Each file's most recent [`Self::parse_generation`] at which
    /// [`Self::file_parses`] needed its entry (a hit or a miss-then-refetch
    /// both count) -- what [`Self::evict_stale_parses`] compares against
    /// `parse_generation` to decide what's gone stale. A file with no
    /// entry here has never been part of a `files_to_rebind` set at all
    /// (nothing to evict).
    pub(crate) parse_last_used: FxHashMap<FileId, u64>,
    /// Each currently-known file's own [`FileTextInput`] identity --
    /// created once (`crate::db::sync_file_text_into_db`) and reused
    /// (its `text` field overwritten, never recreated) so `db`'s
    /// `parse_query`/`collect_query` keep memoizing against the same
    /// input across calls, the same reason `discovery_input` above is
    /// reused rather than recreated. Only a dirty file's entry is ever
    /// touched.
    pub(crate) file_text_inputs: FxHashMap<FileId, FileTextInput>,
    /// The current project-wide file set's own salsa-input identity
    /// (Wayfinder `apex-diagnostics` map, ticket 30/31, Stage 3), `None`
    /// only before the first call. Reused (its `entries` field
    /// overwritten, never recreated) the same way `discovery_input`
    /// above is -- but unlike `discovery_input`, only re-set when the
    /// file set itself actually changed (a file added/removed), not
    /// every call. See `crate::db::FileSetInput`'s own doc comment.
    pub(crate) file_set_input: Option<FileSetInput>,
    /// The last value [`crate::db::raw_inheritance_inputs`] returned,
    /// kept so `BoundProgram::from_files_cached` can compare this call's
    /// freshly-fetched value against it *by content* (not by `Arc`
    /// pointer -- salsa's own early cutoff means a query body can rerun
    /// and still return content-equal output, so pointer equality alone
    /// would under-detect "unchanged") to decide whether
    /// `crate::inherit::resolve_inheritance` actually needs to rerun,
    /// replacing `declarations_changed`'s coarser trigger for this one
    /// decision. `None` only before the first call.
    pub(crate) raw_inheritance_inputs: Option<Arc<RawInheritanceInputs>>,
    /// The project's declared symbols, persisted and patched file-by-file
    /// across calls rather than rebuilt from nothing -- see
    /// `SymbolTable`'s module doc comment.
    pub(crate) table: SymbolTable,
    /// Each type symbol's raw (unresolved) `extends`/`implements` names,
    /// by the file that declared it -- Pass 1.5's input. Persisted
    /// per-file so an unchanged file's entry doesn't need recomputing,
    /// but every entry across every file is still fed to
    /// `crate::inherit::resolve_inheritance` together whenever *any*
    /// file's declarations changed (inheritance is inherently whole-
    /// project, not something one file's edit can resolve in isolation).
    pub(crate) raw_extends: FxHashMap<FileId, Vec<(SymbolId, Vec<SmolStr>)>>,
    pub(crate) raw_super: FxHashMap<FileId, Vec<(SymbolId, SmolStr)>>,
    /// Each `extends`/`implements` supertype name's own `Type` node
    /// pointer, by the file that declared it -- `crate::collect::FileCollection::supertype_ptrs`'s
    /// doc comment explains why this is separate from `raw_extends`/
    /// `raw_super`. Persisted the same way, and resolved (recording a
    /// `Resolution` for goto-definition) alongside Pass 2 whenever that
    /// file gets rebound.
    pub(crate) supertype_ptrs: FxHashMap<FileId, Vec<(SymbolId, AstPtr<Type>)>>,
    pub(crate) bodies: FxHashMap<FileId, Arc<FileBodies>>,
    /// Whether `crate::BoundProgram::from_files_cached` should scope Pass 2
    /// (body/reference binding) to a working set instead of the whole
    /// project (ticket 04, `.scratch/apex-memory/`). `false` for every
    /// CLI/test/batch caller (`from_files`/`from_files_with_overrides`'s
    /// implicit default, never toggled) -- a working set isn't a
    /// meaningful concept for a one-shot full-project bind. `apexls-server`
    /// is the only caller that ever sets this `true`, once, before its
    /// first `from_files_cached` call.
    pub scoping_enabled: bool,
    /// Every currently-open file's path (`apexls-server`'s `Documents`,
    /// re-synced before each `from_files_cached` call from the same
    /// `overrides` map that call itself already receives) -- pinned,
    /// unconditionally, in the Pass 2 working set whenever
    /// [`Self::scoping_enabled`] is set. Ignored entirely otherwise.
    pub open_paths: FxHashSet<PathBuf>,
    /// Non-open files promoted into the Pass 2 working set on demand
    /// (`crate::BoundProgram::ensure_bound`, via a cross-file jump landing
    /// somewhere not open), each mapped to the [`Self::promoted_generation`]
    /// it was last touched at -- the same recency-tracking shape as
    /// `parse_last_used`/`PARSE_EVICTION_WINDOW`, capped at
    /// [`PROMOTED_FILE_CAP`] rather than time-windowed (see
    /// [`Self::note_promoted`]). Only ever populated when
    /// [`Self::scoping_enabled`] is set.
    pub(crate) promoted_files: FxHashMap<FileId, u64>,
    /// Bumped once per [`Self::note_promoted`] call -- [`Self::promoted_files`]'s
    /// clock, mirroring `parse_generation`.
    pub(crate) promoted_generation: u64,
}

/// How many non-open files [`BindCache::note_promoted`] keeps Pass-2-bound
/// on top of whatever's currently open, before evicting the least-
/// recently-promoted one -- generous enough that ordinary cross-file
/// navigation (a handful of goto-definition/hover jumps into unopened
/// files) never thrashes, small enough that it stays well inside the
/// map's ~140-150MB destination's margin (ticket 04, `.scratch/apex-memory/`:
/// ~4-8MB at this cap's size, on top of the 15-open-file projection the
/// map's own budget already assumed).
pub(crate) const PROMOTED_FILE_CAP: usize = 100;

/// How many `crate::BoundProgram::from_files_cached` calls a file's `Parse`
/// survives in [`BindCache::file_parses`] without being touched (a rebind or
/// a capability-handler read) before [`BindCache::evict_stale_parses`] drops
/// it. Chosen generously, not measured against a specific corpus size --
/// large enough that ordinary back-and-forth editing between a couple of
/// files never evicts either one (each edit only bumps the *edited* file's
/// own recency, so a same-file streak keeps every other recently-touched
/// file within the window too), small enough that a long-idle file's tree
/// doesn't sit resident indefinitely. See ticket 07 of
/// `.scratch/apex-performance/` for the full reasoning and the real
/// `BindCache::file_parses`-vs-`parse_query` tension this resolves.
pub(crate) const PARSE_EVICTION_WINDOW: u64 = 8;

impl BindCache {
    /// Forces the next [`crate::BoundProgram::from_files_cached`] call to
    /// redo the directory walk (and SFDX metadata parse) instead of
    /// reusing the cached one -- the hook a filesystem-watcher-driven
    /// caller needs to pick up a file added/removed/edited on disk
    /// outside the editor, which `from_files_cached`'s own staleness
    /// check can't detect by itself (see `discovery`'s doc comment above
    /// for the honest limit this closes).
    pub fn invalidate_discovery(&mut self) {
        self.discovery = None;
    }

    /// Drops every [`Self::file_parses`] entry not touched in the last
    /// [`PARSE_EVICTION_WINDOW`] generations -- see that constant's and
    /// `file_parses`'s own doc comments. Called once per
    /// `crate::BoundProgram::from_files_cached` call, after that call's own
    /// touched files have already updated [`Self::parse_last_used`], so
    /// nothing this call itself needed is ever evicted out from under it.
    pub(crate) fn evict_stale_parses(&mut self) {
        let floor = self.parse_generation.saturating_sub(PARSE_EVICTION_WINDOW);
        let stale: Vec<FileId> = self
            .parse_last_used
            .iter()
            .filter(|&(_, &last_used)| last_used < floor)
            .map(|(&file, _)| file)
            .collect();
        for file in stale {
            self.file_parses.remove(&file);
            self.parse_last_used.remove(&file);
        }
    }

    /// Records `file` as freshly promoted into the Pass 2 working set
    /// (`crate::BoundProgram::ensure_bound`), then evicts the least-
    /// recently-promoted file's `bodies` entry -- returning it, so the
    /// caller can also drop it from its own live `BoundProgram` snapshot's
    /// `bodies` (see `ensure_bound`'s own doc comment: without that, a
    /// single long-lived snapshot could accumulate unbounded promotions
    /// between rebuilds, since this cache-side cap alone only bounds the
    /// *next* rebuild's snapshot) -- if that pushes [`Self::promoted_files`]
    /// over [`PROMOTED_FILE_CAP`]. An O(cap) scan per call, not a real LRU
    /// structure, since the cap is small enough (~100) that this is
    /// cheaper than the bookkeeping a dedicated LRU container would add.
    /// A no-op scan-and-reinsert (`None`) for a file that's already
    /// tracked -- this just refreshes its recency.
    pub(crate) fn note_promoted(&mut self, file: FileId) -> Option<FileId> {
        self.promoted_generation += 1;
        self.promoted_files.insert(file, self.promoted_generation);
        if self.promoted_files.len() <= PROMOTED_FILE_CAP {
            return None;
        }
        let &lru_file = self
            .promoted_files
            .iter()
            .min_by_key(|&(_, &generation)| generation)
            .map(|(file, _)| file)?;
        self.promoted_files.remove(&lru_file);
        self.bodies.remove(&lru_file);
        Some(lru_file)
    }
}
