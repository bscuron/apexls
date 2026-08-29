//! Parses the `.labels-meta.xml` paths `apex_discover::discover` finds
//! into a flat `Vec<LabelSchema>` -- no grouping needed the way
//! `discover.rs` groups field files by their owning object, since a
//! label has no owner beyond the one file it's declared in, and that
//! file already carries every label it declares in one place.

use crate::xml::parse_labels_meta;
use crate::LabelSchema;
use rayon::prelude::*;
use std::path::Path;

/// Discovers every `.labels-meta.xml` file under `root` and parses all
/// of their `<labels>` blocks into one flat `Vec<LabelSchema>`. Malformed
/// XML in one file, or a malformed `<labels>` block within an otherwise-
/// valid file, is silently skipped rather than failing the whole
/// discovery, matching `apex_discover::discover`'s and
/// `discover_sobjects`'s existing error-tolerance.
///
/// Walks fresh every call -- prefer [`labels_from_discovery`] if the
/// caller already has a [`apex_discover::Discovery`] in hand, so the tree
/// isn't read twice.
pub fn discover_labels(root: impl AsRef<Path>) -> Vec<LabelSchema> {
    labels_from_discovery(&apex_discover::discover(root))
}

/// Like [`discover_labels`], but parses an already-computed
/// [`apex_discover::Discovery`] instead of walking `root` itself.
pub fn labels_from_discovery(found: &apex_discover::Discovery) -> Vec<LabelSchema> {
    // Parallel (`rayon`): each file's read + XML parse is fully
    // independent of every other one, the same parallel-map shape
    // `discover::sobjects_from_discovery` already uses for field files.
    found
        .labels_meta_files
        .par_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok().map(|content| (path, content)))
        .flat_map(|(path, content)| parse_labels_meta(&content, path).into_par_iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_every_label_across_multiple_files() {
        let dir = std::env::temp_dir().join(format!("apex-metadata-labels-test-{}", std::process::id()));
        let labels_dir = dir.join("labels");
        std::fs::create_dir_all(&labels_dir).unwrap();

        std::fs::write(
            labels_dir.join("CustomLabels.labels-meta.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">
    <labels>
        <fullName>greeting</fullName>
        <value>Hello</value>
    </labels>
</CustomLabels>"#,
        )
        .unwrap();
        std::fs::write(
            labels_dir.join("Vendor-CustomLabels.labels-meta.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">
    <labels>
        <fullName>vendor_error</fullName>
        <value>Something went wrong</value>
    </labels>
    <labels>
        <fullName>vendor_warning</fullName>
        <value>Careful now</value>
    </labels>
</CustomLabels>"#,
        )
        .unwrap();

        let mut labels = discover_labels(&dir);
        labels.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(labels.len(), 3);
        assert_eq!(labels[0].full_name, "greeting");
        assert_eq!(labels[0].value.as_deref(), Some("Hello"));
        assert_eq!(labels[1].full_name, "vendor_error");
        assert_eq!(labels[2].full_name, "vendor_warning");
    }
}
