//! Thin extraction over `roxmltree`'s generic DOM: pull just the handful
//! of elements `FieldSchema`/`LabelSchema` need out of a
//! `.field-meta.xml`/`.labels-meta.xml` file's parsed tree, ignoring the
//! many other child elements (`label`, `description`, `required`,
//! `trackFeedHistory`, ...) that aren't needed yet -- extending either
//! schema type later only means reading another child here, not touching
//! the walk or discovery logic.

use crate::{FieldSchema, LabelSchema};
use smol_str::SmolStr;
use std::path::{Path, PathBuf};

pub(crate) fn parse_field_meta(xml: &str, source_path: PathBuf) -> Option<FieldSchema> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();

    let api_name = SmolStr::new(child_text(root, "fullName")?);
    let field_type = child_text(root, "type").map(SmolStr::new);
    let reference_to = root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "referenceTo")
        .filter_map(|n| n.text())
        .map(SmolStr::new)
        .collect();

    Some(FieldSchema {
        api_name,
        field_type,
        reference_to,
        source_path: Some(source_path),
    })
}

/// A `CustomLabels.labels-meta.xml` file's root holds *many* `<labels>`
/// children, one per declared label -- unlike `.field-meta.xml`, whose
/// whole document is a single field, so this returns a `Vec` rather than
/// one `LabelSchema`. Malformed XML yields an empty `Vec` (matching
/// `parse_field_meta`'s `None`-on-malformed behavior, just at the
/// multi-item shape this file's content actually has); an individual
/// `<labels>` block missing its own `<fullName>` is skipped rather than
/// aborting the whole file, so one malformed label entry can't hide every
/// other real one in the same file.
pub(crate) fn parse_labels_meta(xml: &str, source_path: &Path) -> Vec<LabelSchema> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    doc.root_element()
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "labels")
        .filter_map(|label| {
            let full_name = SmolStr::new(child_text(label, "fullName")?);
            let value = child_text(label, "value").map(SmolStr::new);
            Some(LabelSchema {
                full_name,
                value,
                source_path: source_path.to_path_buf(),
            })
        })
        .collect()
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == tag)
        .and_then(|n| n.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_lookup_field_with_a_reference() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>Batch__c</fullName>
    <deleteConstraint>SetNull</deleteConstraint>
    <label>Batch</label>
    <referenceTo>Batch__c</referenceTo>
    <relationshipName>Accounts</relationshipName>
    <type>Lookup</type>
</CustomField>"#;
        let field = parse_field_meta(xml, PathBuf::from("Batch__c.field-meta.xml")).unwrap();
        assert_eq!(field.api_name, "Batch__c");
        assert_eq!(field.field_type.as_deref(), Some("Lookup"));
        assert_eq!(field.reference_to, vec![SmolStr::new("Batch__c")]);
    }

    #[test]
    fn parses_a_checkbox_field_with_no_reference() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>All_Members_Deceased__c</fullName>
    <defaultValue>false</defaultValue>
    <type>Checkbox</type>
</CustomField>"#;
        let field = parse_field_meta(
            xml,
            PathBuf::from("All_Members_Deceased__c.field-meta.xml"),
        )
        .unwrap();
        assert_eq!(field.api_name, "All_Members_Deceased__c");
        assert_eq!(field.field_type.as_deref(), Some("Checkbox"));
        assert!(field.reference_to.is_empty());
    }

    #[test]
    fn missing_full_name_is_rejected() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <type>Checkbox</type>
</CustomField>"#;
        assert!(parse_field_meta(xml, PathBuf::from("x.field-meta.xml")).is_none());
    }

    #[test]
    fn parses_every_label_in_a_multi_label_file() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">
    <labels>
        <fullName>fflib_security_error_object_not_insertable</fullName>
        <categories>security,error</categories>
        <language>en_US</language>
        <protected>true</protected>
        <shortDescription>fflib_security_error_object_not_insertable</shortDescription>
        <value>You do not have permission to create {0}</value>
    </labels>
    <labels>
        <fullName>fflib_security_error_object_not_readable</fullName>
        <language>en_US</language>
        <protected>true</protected>
        <shortDescription>fflib_security_error_object_not_readable</shortDescription>
        <value>You do not have permission to read {0}</value>
    </labels>
</CustomLabels>"#;
        let labels = parse_labels_meta(xml, Path::new("CustomLabels.labels-meta.xml"));
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].full_name, "fflib_security_error_object_not_insertable");
        assert_eq!(labels[0].value.as_deref(), Some("You do not have permission to create {0}"));
        assert_eq!(labels[1].full_name, "fflib_security_error_object_not_readable");
    }

    #[test]
    fn a_label_missing_its_own_full_name_is_skipped_not_fatal() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">
    <labels>
        <value>no fullName here</value>
    </labels>
    <labels>
        <fullName>a_real_label</fullName>
        <value>real</value>
    </labels>
</CustomLabels>"#;
        let labels = parse_labels_meta(xml, Path::new("CustomLabels.labels-meta.xml"));
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].full_name, "a_real_label");
    }

    #[test]
    fn malformed_labels_xml_yields_an_empty_vec() {
        assert!(parse_labels_meta("not xml at all", Path::new("x.labels-meta.xml")).is_empty());
    }
}
