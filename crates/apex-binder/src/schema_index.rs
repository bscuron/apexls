//! [`apex_metadata::SObjectSchema`]/[`FieldSchema`] wrapped into a
//! lowercase-keyed lookup index. Salesforce API names, like Apex
//! identifiers, are case-insensitive, so every consultation point in
//! `crate::soql`/`crate::resolve` goes through this rather than
//! matching `api_name` fields directly.

use crate::ptr::SyntaxPtr;
use crate::reference_table::{ReferenceTable, Resolution};
use apex_metadata::{FieldSchema, SObjectSchema};
use std::collections::HashMap;
use std::path::Path;

pub struct SchemaIndex {
    objects: HashMap<String, ObjectEntry>,
}

struct ObjectEntry {
    schema: SObjectSchema,
    /// Lowercase field API name -> index into `schema.fields`.
    fields: HashMap<String, usize>,
}

impl SchemaIndex {
    /// Walks `root` for SFDX metadata and builds the index from it --
    /// see `apex_metadata::discover_sobjects`'s own doc comment for the
    /// standard-object gap this inherits.
    pub fn build(root: impl AsRef<Path>) -> Self {
        Self::from_sobjects(apex_metadata::discover_sobjects(root))
    }

    pub fn from_sobjects(sobjects: Vec<SObjectSchema>) -> Self {
        let objects = sobjects
            .into_iter()
            .map(|schema| {
                let fields = schema
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.api_name.to_ascii_lowercase(), i))
                    .collect();
                (
                    schema.api_name.to_ascii_lowercase(),
                    ObjectEntry { schema, fields },
                )
            })
            .collect();
        SchemaIndex { objects }
    }

    pub fn object(&self, api_name: &str) -> Option<&SObjectSchema> {
        self.objects
            .get(&api_name.to_ascii_lowercase())
            .map(|e| &e.schema)
    }

    pub fn field(&self, object_api_name: &str, field_api_name: &str) -> Option<&FieldSchema> {
        let entry = self.objects.get(&object_api_name.to_ascii_lowercase())?;
        let idx = *entry.fields.get(&field_api_name.to_ascii_lowercase())?;
        entry.schema.fields.get(idx)
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

/// Resolves a bare object-name reference (a SOQL `FROM` entry, a
/// trigger's `ON <object>`, a SOSL field-spec object, ...) against
/// `schema`, recording the outcome at `ptr`. Free-standing (not a
/// `BodyBinder` method) since a couple of call sites -- `TriggerUnit`'s
/// `object_ref`, in particular -- don't have (and don't need) a whole
/// expression-binding context just to resolve one object name.
pub(crate) fn resolve_object(
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    ptr: SyntaxPtr,
    name: &str,
) {
    let resolution = if schema.object(name).is_some() {
        Resolution::SchemaObject {
            object: name.to_string(),
            field: None,
        }
    } else {
        Resolution::UnknownSchema {
            object: Some(name.to_string()),
            field: None,
        }
    };
    refs.set(ptr, resolution);
}
