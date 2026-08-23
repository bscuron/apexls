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
use crate::scope::ScopeTree;
use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use apex_parser::Parse;
use std::collections::HashMap;
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
    pub(crate) scopes: HashMap<SyntaxPtr, ScopeTree>,
}

#[derive(Default)]
pub struct BindCache {
    pub(crate) files: FileTable,
    /// Each path's last-seen `(content, Parse)` -- a parse is reused
    /// as-is whenever a file's content is byte-for-byte identical to
    /// last time, skipping that file's lex/parse entirely.
    pub(crate) parses: HashMap<PathBuf, (String, Parse)>,
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
    pub(crate) raw_extends: HashMap<FileId, Vec<(SymbolId, Vec<String>)>>,
    pub(crate) raw_super: HashMap<FileId, Vec<(SymbolId, String)>>,
    pub(crate) bodies: HashMap<FileId, Arc<FileBodies>>,
}
