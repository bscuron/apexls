//! Bundled, offline snapshot of standard Salesforce SObject/field schema
//! -- the gap `apex_metadata`'s own module doc comment documents as
//! "deliberately left as an open decision for whoever wires up the
//! symbol table": standard objects and fields (`Account`, `Contact`,
//! `Opportunity`, ...) have no local metadata files a real project ever
//! ships, since they already exist on every org. This crate fills that
//! gap with a snapshot scraped directly from Salesforce's own Object
//! Reference documentation (`tools/salesforce-doc-scraper`), embedded
//! at compile time so `apexls` never makes a network call at runtime.
//!
//! Deliberately its own small `serde`-deriving struct shape here
//! (`RawObject`/`RawField`) rather than depending on the scraper's own
//! `tools/salesforce-doc-scraper::model` types directly -- a workspace
//! member depending on a `tools/` binary crate would be backwards, and
//! the scraper's own `model.rs` doc comment already gives the same
//! reasoning for not designing its output against `apex-binder`'s types
//! either: the two sides of this mapping are deliberately decoupled.
//!
//! **Refreshing the snapshot**: re-run
//! `cargo run -p salesforce-doc-scraper -- scrape-object-reference --out out/standard_objects.json`
//! (see that crate's own README) and copy the result over
//! `data/standard_objects.json`. Manual, on a new Apex release -- not
//! automated, matching the scraper's own "runs once per release, not
//! once per build" design.

use apex_metadata::{FieldSchema, SObjectSchema};
use serde::Deserialize;
use smol_str::SmolStr;
use std::sync::OnceLock;

#[derive(Deserialize)]
struct RawObject {
    name: String,
    fields: Vec<RawField>,
}

#[derive(Deserialize)]
struct RawField {
    name: String,
    field_type: Option<String>,
    #[serde(default)]
    reference_to: Vec<String>,
}

const STANDARD_OBJECTS_JSON: &str = include_str!("../data/standard_objects.json");

/// Every standard SObject's bundled schema, parsed once on first use.
/// Panics on first access if the embedded JSON is missing/malformed --
/// a build-time-detectable failure (the data is `include_str!`'d, so a
/// corrupt snapshot breaks every build's tests, not just a runtime
/// caller) rather than a silently-empty schema index.
pub fn standard_sobjects() -> &'static [SObjectSchema] {
    static SOBJECTS: OnceLock<Vec<SObjectSchema>> = OnceLock::new();
    SOBJECTS.get_or_init(|| {
        let raw: Vec<RawObject> = serde_json::from_str(STANDARD_OBJECTS_JSON)
            .expect("bundled data/standard_objects.json failed to parse");
        raw.into_iter().map(to_sobject_schema).collect()
    })
}

fn to_sobject_schema(raw: RawObject) -> SObjectSchema {
    SObjectSchema {
        api_name: SmolStr::new(&raw.name),
        is_custom: false,
        object_path: None,
        fields: raw.fields.into_iter().map(to_field_schema).collect(),
    }
}

fn to_field_schema(raw: RawField) -> FieldSchema {
    FieldSchema {
        api_name: SmolStr::new(&raw.name),
        field_type: raw.field_type.map(|t| SmolStr::new(&t)),
        reference_to: raw.reference_to.iter().map(SmolStr::new).collect(),
        source_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_parses_and_has_account_name() {
        let objects = standard_sobjects();
        assert!(
            objects.len() > 500,
            "expected hundreds of standard objects, got {}",
            objects.len()
        );

        let account = objects
            .iter()
            .find(|o| o.api_name == "Account")
            .expect("Account should be in the bundled snapshot");
        assert!(!account.is_custom);
        assert!(account.object_path.is_none());

        let name_field = account
            .fields
            .iter()
            .find(|f| f.api_name == "Name")
            .expect("Account.Name should be in the bundled snapshot");
        assert_eq!(name_field.field_type.as_deref(), Some("string"));
        assert!(name_field.source_path.is_none());
    }

    #[test]
    fn a_lookup_field_carries_its_reference_to() {
        let objects = standard_sobjects();
        let contact = objects
            .iter()
            .find(|o| o.api_name == "Contact")
            .expect("Contact should be in the bundled snapshot");
        let account_id = contact
            .fields
            .iter()
            .find(|f| f.api_name == "AccountId")
            .expect("Contact.AccountId should be in the bundled snapshot");
        assert_eq!(account_id.reference_to, vec![SmolStr::new("Account")]);
    }
}
