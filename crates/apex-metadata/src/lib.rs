//! Salesforce SFDX-format metadata ingestion: takes the
//! `.object-meta.xml`/`.field-meta.xml` files `apex_discover::discover`
//! finds (custom objects, and custom fields -- including custom fields
//! added to *standard* objects like `Account`) and parses them into
//! [`SObjectSchema`]/[`FieldSchema`]. Directory walking itself lives in
//! `apex-discover`, not here (see [`discover::discover_sobjects`]'s doc
//! comment) -- this crate is XML parsing plus grouping-by-object only.
//! This is the metadata-side counterpart to `apex-parser`: a future
//! symbol-table binder needs both -- declarations from parsed `.cls`
//! files (`apex_syntax::ast::decl`), and SObject/field schema from here
//! -- to resolve real Apex code (`myAccount.My_Field__c`, `[SELECT Id
//! FROM MyObject__c]`, ...).
//!
//! **Known gap, by design, not solved here:** standard objects and
//! standard fields (`Account`, `Contact.Email`, `Opportunity.Amount`,
//! ...) have no local metadata files at all -- their schema exists only
//! in the org itself, since there's nothing to deploy for something that
//! already exists on every org. A repo that only touches standard
//! objects/fields produces an empty (or field-only, for standard objects
//! extended with custom fields) [`SObjectSchema`] from this crate alone.
//! Resolving standard schema needs either a bundled snapshot (goes stale
//! across Salesforce's three-times-a-year releases) or a live describe
//! call against a connected org (the `sf`/Tooling API path already used
//! to verify parser grammar questions against a real org) -- deliberately
//! left as an open decision for whoever wires up the symbol table, since
//! it determines whether the tool can ever work fully offline.

mod discover;
mod xml;

pub use discover::{discover_sobjects, sobjects_from_discovery};

use smol_str::SmolStr;

/// One SObject's schema, as reconstructed from local repo metadata only
/// -- see the module doc comment for the standard-object gap this
/// implies.
#[derive(Debug, Clone, PartialEq)]
pub struct SObjectSchema {
    /// The object's API name, e.g. `Contact` or `My_Object__c` -- taken
    /// from the containing `objects/<ApiName>/` directory name, since
    /// `.object-meta.xml` never repeats it in its own content.
    pub api_name: SmolStr,
    /// Whether this object has its own `<ApiName>.object-meta.xml` (a
    /// *declared* custom object), as opposed to only appearing here
    /// because custom fields were added to a standard object.
    pub is_custom: bool,
    pub fields: Vec<FieldSchema>,
}

/// One custom field's schema, from a single `.field-meta.xml` file.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSchema {
    /// The field's API name (`<fullName>`), e.g. `My_Field__c`.
    pub api_name: SmolStr,
    /// The raw `<type>` element text (`Lookup`, `Checkbox`, `Picklist`,
    /// ...), or `None` on the handful of custom-field shapes that omit
    /// it. Kept as an owned string rather than a closed enum so a
    /// Salesforce field type this crate doesn't know about yet is a
    /// forward-compatible "just a string" rather than a parse failure --
    /// converting to a closed enum, if a consumer wants one, is a
    /// lossless operation layered on top of this.
    pub field_type: Option<SmolStr>,
    /// The target object(s) of a `Lookup`/`MasterDetail` field -- more
    /// than one only for a polymorphic lookup (e.g. `Task.WhoId`, which
    /// itself is a standard field and so never appears here, but custom
    /// polymorphic lookups follow the same shape).
    pub reference_to: Vec<SmolStr>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_a_custom_object_and_a_field_on_a_standard_object() {
        let dir = std::env::temp_dir().join(format!("apex-metadata-test-{}", std::process::id()));
        let custom_obj = dir.join("objects/My_Object__c");
        let custom_fields = custom_obj.join("fields");
        let account_fields = dir.join("objects/Account/fields");
        std::fs::create_dir_all(&custom_fields).unwrap();
        std::fs::create_dir_all(&account_fields).unwrap();

        std::fs::write(
            custom_obj.join("My_Object__c.object-meta.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <label>My Object</label>
</CustomObject>"#,
        )
        .unwrap();
        std::fs::write(
            custom_fields.join("Status__c.field-meta.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>Status__c</fullName>
    <type>Picklist</type>
</CustomField>"#,
        )
        .unwrap();
        std::fs::write(
            account_fields.join("Batch__c.field-meta.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomField xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>Batch__c</fullName>
    <type>Lookup</type>
    <referenceTo>Batch__c</referenceTo>
</CustomField>"#,
        )
        .unwrap();

        let mut objects = discover_sobjects(&dir);
        objects.sort_by(|a, b| a.api_name.cmp(&b.api_name));

        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(objects.len(), 2);

        let account = &objects[0];
        assert_eq!(account.api_name, "Account");
        assert!(!account.is_custom);
        assert_eq!(account.fields.len(), 1);
        assert_eq!(account.fields[0].api_name, "Batch__c");
        assert_eq!(account.fields[0].field_type.as_deref(), Some("Lookup"));
        assert_eq!(
            account.fields[0].reference_to,
            vec![SmolStr::new("Batch__c")]
        );

        let my_object = &objects[1];
        assert_eq!(my_object.api_name, "My_Object__c");
        assert!(my_object.is_custom);
        assert_eq!(my_object.fields.len(), 1);
        assert_eq!(my_object.fields[0].api_name, "Status__c");
        assert_eq!(my_object.fields[0].field_type.as_deref(), Some("Picklist"));
    }
}
