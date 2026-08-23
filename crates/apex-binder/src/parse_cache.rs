//! A cross-rebuild cache of each file's last-seen `(content, Parse)`, so
//! [`crate::BoundProgram::from_files_cached`] doesn't re-lex/re-parse a
//! file whose content hasn't changed since the previous rebuild -- the
//! common case for a single-keystroke edit in an editor, where every file
//! but the one being typed in is unchanged. See `BACKLOG.md` §2.
//!
//! Keyed by exact path plus exact string equality of content, not a
//! content hash: at the file counts this project targets (a real org's
//! whole Apex source tree), an exact `String` comparison is cheap enough
//! that hashing would only add a theoretical (if vanishingly unlikely)
//! collision risk for no real speed benefit.

use apex_parser::Parse;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub struct ParseCache {
    pub(crate) by_path: HashMap<PathBuf, (String, Parse)>,
}
