//! [`apex_metadata::LabelSchema`] wrapped into a lowercase-keyed lookup
//! index. Custom label API names, like every other Apex identifier, are
//! case-insensitive, so `crate::resolve` goes through this rather than
//! matching `full_name` fields directly -- the label counterpart of
//! `crate::schema_index::SchemaIndex`, structurally simpler since a label
//! has no per-entry sub-index (no field list) and no bundled-standard
//! snapshot to merge with (see `apex_metadata::LabelSchema`'s own doc
//! comment: there's no such thing as a "standard" label the way there's a
//! standard object).

use crate::ci_key::{CiKey, CiMap, CiQuery};
use apex_metadata::LabelSchema;
use std::path::Path;

pub struct LabelIndex {
    labels: CiMap<LabelSchema>,
}

impl LabelIndex {
    /// Walks `root` for SFDX metadata and builds the index from it.
    /// Prefer [`Self::from_discovery`] if the caller already has an
    /// `apex_discover::Discovery` in hand, so the directory tree isn't
    /// walked twice.
    pub fn build(root: impl AsRef<Path>) -> Self {
        Self::from_labels(apex_metadata::discover_labels(root))
    }

    /// Like [`Self::build`], but parses an already-computed
    /// `apex_discover::Discovery` instead of walking `root` itself.
    pub fn from_discovery(discovery: &apex_discover::Discovery) -> Self {
        Self::from_labels(apex_metadata::labels_from_discovery(discovery))
    }

    pub fn from_labels(labels: Vec<LabelSchema>) -> Self {
        let labels = labels
            .into_iter()
            .map(|label| (CiKey::from(label.full_name.as_str()), label))
            .collect();
        LabelIndex { labels }
    }

    pub fn get(&self, full_name: &str) -> Option<&LabelSchema> {
        self.labels.get(&CiQuery(full_name))
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(full_name: &str, value: &str) -> LabelSchema {
        LabelSchema {
            full_name: full_name.into(),
            value: Some(value.into()),
            source_path: std::path::PathBuf::from("CustomLabels.labels-meta.xml"),
        }
    }

    #[test]
    fn looks_up_a_label_case_insensitively() {
        let index = LabelIndex::from_labels(vec![label("greeting", "Hello")]);
        assert_eq!(index.get("greeting").unwrap().value.as_deref(), Some("Hello"));
        assert_eq!(index.get("GREETING").unwrap().value.as_deref(), Some("Hello"));
        assert_eq!(index.get("Greeting").unwrap().value.as_deref(), Some("Hello"));
    }

    #[test]
    fn a_missing_label_returns_none() {
        let index = LabelIndex::from_labels(vec![label("greeting", "Hello")]);
        assert!(index.get("nonexistent").is_none());
    }

    #[test]
    fn an_empty_index_reports_len_and_is_empty_correctly() {
        let index = LabelIndex::from_labels(Vec::new());
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }
}
