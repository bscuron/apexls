//! A persistent `PathBuf -> FileId` mapping, owned by [`crate::BindCache`]
//! and reused across every `BoundProgram::from_files_cached` call in a
//! session. `apex_discover::discover`'s returned file list is not stable
//! across calls -- if any file anywhere in the project is added or
//! removed, a fresh directory walk can shift where an *unrelated* file
//! lands in that list -- so `FileId` can no longer be "a file's position
//! in this call's discovery results" (that was `BoundProgram::from_files`'s
//! original scheme) without breaking every per-file cache keyed by it.
//! `FileTable` fixes that: a path's `FileId`, once assigned, never
//! changes for the rest of the session, and ids are never recycled even
//! if a file is later deleted -- both properties `SymbolId` (`crate::symbol`)
//! now depends on for its own stability guarantee.

use crate::file_id::FileId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct FileTable {
    ids: HashMap<PathBuf, FileId>,
    next: u32,
}

impl FileTable {
    pub(crate) fn id_for(&mut self, path: &Path) -> FileId {
        if let Some(&id) = self.ids.get(path) {
            return id;
        }
        let id = FileId(self.next);
        self.next += 1;
        self.ids.insert(path.to_path_buf(), id);
        id
    }
}
