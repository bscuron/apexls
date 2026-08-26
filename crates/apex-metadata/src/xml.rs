//! Thin extraction over `roxmltree`'s generic DOM: pull just the handful
//! of elements `FieldSchema` needs out of a `.field-meta.xml` file's
//! parsed tree, ignoring the many other `<CustomField>` child elements
//! (`label`, `description`, `required`, `trackFeedHistory`, ...) that
//! aren't needed yet -- extending `FieldSchema` later only means reading
//! another child here, not touching the walk or discovery logic.

use crate::FieldSchema;
use smol_str::SmolStr;
use std::path::PathBuf;

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
}
