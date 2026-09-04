//! [`BindCache`]: everything [`crate::BoundProgram::from_files_cached`]
//! remembers between calls so it can skip redoing work an edit couldn't
//! possibly have affected. Superset of the earlier `ParseCache` (parsed
//! trees only) added in `BACKLOG.md` §2 Step 1/2 -- this also persists
//! the project's [`SymbolTable`] itself plus every file's Pass 1.5 input
//! (`raw_extends`/`raw_super`) and Pass 2 output (refs/scopes), patched
//! file-by-file rather than rebuilt from nothing every call. See
//! `crate::symbol::SymbolId`'s and `crate::file_table::FileTable`'s doc
//! comments for the stable-identity foundation this relies on.

use crate::db::{BindDatabase, DiscoveryInput, FileTextInput};
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
use rustc_hash::{FxHashMap, FxHasher};
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
    /// Every currently-live file's last-bound `Parse`, keyed by `FileId`
    /// -- the `BoundProgram`-facing counterpart to `freshness` above
    /// (which is keyed by `PathBuf`, purely for that field's own
    /// staleness check). Patched only for dirty files each call, exactly
    /// like `table`/`bodies` below -- an unaffected file's entry is
    /// already correct and never re-queried. Ticket 29 deliberately
    /// keeps this field despite its own text calling for its deletion:
    /// `crate::BoundProgram::from_files_cached`'s Pass 2 needs *every*
    /// current file's `Parse` on hand for the conservative "declarations
    /// changed somewhere, rebind everything" case, and sweeping all
    /// ~1,070 corpus files through a salsa query every single call (most
    /// of them just a memoized-value fetch) measured slower than this
    /// persisted map's near-free `.clone()` on the warm single-edit path
    /// -- the exact case this migration's own done-bar protects. A dirty
    /// file's entry is now populated from `crate::db::parse_query`'s
    /// salsa-memoized output (see [`Self::file_text_inputs`]) rather than
    /// `parse_one`'s direct computation, but the field's role is
    /// otherwise unchanged.
    pub(crate) file_parses: FxHashMap<FileId, Parse>,
    /// Each currently-known file's own [`FileTextInput`] identity --
    /// created once (`crate::db::sync_file_text_into_db`) and reused
    /// (its `text` field overwritten, never recreated) so `db`'s
    /// `parse_query`/`collect_query` keep memoizing against the same
    /// input across calls, the same reason `discovery_input` above is
    /// reused rather than recreated. Only a dirty file's entry is ever
    /// touched.
    pub(crate) file_text_inputs: FxHashMap<FileId, FileTextInput>,
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
}

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
}
