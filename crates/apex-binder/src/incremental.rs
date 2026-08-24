//! [`BindCache`]: everything [`crate::BoundProgram::from_files_cached`]
//! remembers between calls so it can skip redoing work an edit couldn't
//! possibly have affected. Superset of the earlier `ParseCache` (parsed
//! trees only) added in `BACKLOG.md` §2 Step 1/2 -- this also persists
//! the project's [`SymbolTable`] itself plus every file's Pass 1.5 input
//! (`raw_extends`/`raw_super`) and Pass 2 output (refs/scopes), patched
//! file-by-file rather than rebuilt from nothing every call. See
//! `crate::symbol::SymbolId`'s and `crate::file_table::FileTable`'s doc
//! comments for the stable-identity foundation this relies on.

use crate::file_id::FileId;
use crate::file_table::FileTable;
use crate::ptr::SyntaxPtr;
use crate::reference_table::ReferenceTable;
use crate::schema_index::SchemaIndex;
use crate::scope::ScopeTree;
use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use apex_discover::Discovery;
use apex_parser::Parse;
use rustc_hash::FxHashMap;
use smol_str::SmolStr;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// this gets refreshed, and what staleness it accepts as an honest
    /// v1 limit (a file added on disk but never opened in the editor, or
    /// metadata XML edited with no corresponding Apex-file signal).
    pub(crate) discovery: Option<Discovery>,
    pub(crate) schema: Option<Arc<SchemaIndex>>,
    /// Each path's last-seen `(content, Parse)` -- a parse is reused
    /// as-is whenever a file's content is byte-for-byte identical to
    /// last time, skipping that file's lex/parse entirely.
    pub(crate) parses: FxHashMap<PathBuf, (String, Parse)>,
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
    pub(crate) bodies: FxHashMap<FileId, Arc<FileBodies>>,
}
