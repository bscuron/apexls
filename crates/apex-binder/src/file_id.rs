//! Identifies one file within a [`crate::BoundProgram`] by its index
//! into the program's file table, rather than every `Symbol` carrying
//! its own `PathBuf` -- keeps `Symbol` small, `Copy`, and cheap to store
//! by the thousands.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub(crate) u32);

impl FileId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}
