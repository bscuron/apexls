//! Every Visualforce page (`.page` file) a project declares, keyed by its
//! own file-stem name -- what a `Page.<name>` reference (real Apex
//! compiler-magic syntax, e.g. `PageReference pr = Page.MyPage;`) resolves
//! against. Unlike `crate::label_index::LabelIndex`, there's no content to
//! parse at all: a page's own name *is* its file's base name (no
//! `<fullName>`-style declaration inside the file the way a custom label
//! has one), so this lives directly in `apex-binder` rather than needing
//! an `apex-metadata` XML-parsing layer -- `apex_discover::Discovery::page_files`
//! is already everything this needs.

use crate::ci_key::{CiKey, CiMap, CiQuery};
use smol_str::SmolStr;
use std::path::{Path, PathBuf};

/// One declared Visualforce page.
#[derive(Debug, PartialEq)]
pub struct VisualforcePage {
    pub name: SmolStr,
    pub path: PathBuf,
}

/// `PartialEq`/`Debug` exist solely for `crate::salsa_stage1_dual_run`'s
/// dual-run-and-diff comparison (Wayfinder `apex-diagnostics` map, ticket
/// 26) -- nothing else in this crate compares two `PageIndex`es.
#[derive(Debug, PartialEq)]
pub struct PageIndex {
    pages: CiMap<VisualforcePage>,
}

impl PageIndex {
    /// Walks `root` for SFDX metadata and builds the index from it.
    /// Prefer [`Self::from_discovery`] if the caller already has an
    /// `apex_discover::Discovery` in hand, so the directory tree isn't
    /// walked twice.
    pub fn build(root: impl AsRef<Path>) -> Self {
        Self::from_discovery(&apex_discover::discover(root))
    }

    /// Like [`Self::build`], but reads an already-computed
    /// `apex_discover::Discovery` instead of walking `root` itself.
    pub fn from_discovery(discovery: &apex_discover::Discovery) -> Self {
        let pages = discovery
            .page_files
            .iter()
            .filter_map(|path| {
                let stem = path.file_stem()?.to_str()?;
                let name = SmolStr::new(stem);
                Some((
                    CiKey::from(name.as_str()),
                    VisualforcePage {
                        name,
                        path: path.clone(),
                    },
                ))
            })
            .collect();
        PageIndex { pages }
    }

    pub fn get(&self, name: &str) -> Option<&VisualforcePage> {
        self.pages.get(&CiQuery(name))
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(page_paths: &[&str]) -> apex_discover::Discovery {
        apex_discover::Discovery {
            page_files: page_paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn looks_up_a_page_by_its_file_stem_case_insensitively() {
        let index = PageIndex::from_discovery(&discovery(&["pages/MyPage.page"]));
        assert_eq!(index.get("MyPage").unwrap().name, "MyPage");
        assert_eq!(index.get("mypage").unwrap().name, "MyPage");
        assert_eq!(index.get("MYPAGE").unwrap().name, "MyPage");
    }

    #[test]
    fn a_missing_page_returns_none() {
        let index = PageIndex::from_discovery(&discovery(&["pages/MyPage.page"]));
        assert!(index.get("NoSuchPage").is_none());
    }

    #[test]
    fn an_empty_index_reports_len_and_is_empty_correctly() {
        let index = PageIndex::from_discovery(&discovery(&[]));
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }
}
