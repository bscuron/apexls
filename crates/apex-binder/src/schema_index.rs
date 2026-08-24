//! [`apex_metadata::SObjectSchema`]/[`FieldSchema`] wrapped into a
//! lowercase-keyed lookup index. Salesforce API names, like Apex
//! identifiers, are case-insensitive, so every consultation point in
//! `crate::soql`/`crate::resolve` goes through this rather than
//! matching `api_name` fields directly.

use crate::ci_key::{CiKey, CiMap, CiQuery};
use crate::ptr::SyntaxPtr;
use crate::reference_table::{ReferenceTable, Resolution, SchemaObjectRef, UnknownSchemaRef};
use apex_metadata::{FieldSchema, SObjectSchema};
use std::path::Path;

pub struct SchemaIndex {
    objects: CiMap<ObjectEntry>,
}

struct ObjectEntry {
    schema: SObjectSchema,
    /// Field API name -> index into `schema.fields`, case-insensitively
    /// keyed via [`CiKey`]/[`CiQuery`] the same way `objects` itself is.
    fields: CiMap<usize>,
}

impl SchemaIndex {
    /// Walks `root` for SFDX metadata and builds the index from it --
    /// see `apex_metadata::discover_sobjects`'s own doc comment for the
    /// standard-object gap this inherits. Prefer [`Self::from_discovery`]
    /// if the caller already has an `apex_discover::Discovery` in hand
    /// (e.g. `crate::BoundProgram::from_files_cached`, which also needs
    /// it for `apex_files`), so the directory tree isn't walked twice.
    pub fn build(root: impl AsRef<Path>) -> Self {
        Self::from_sobjects(apex_metadata::discover_sobjects(root))
    }

    /// Like [`Self::build`], but parses an already-computed
    /// `apex_discover::Discovery` instead of walking `root` itself.
    pub fn from_discovery(discovery: &apex_discover::Discovery) -> Self {
        Self::from_sobjects(apex_metadata::sobjects_from_discovery(discovery))
    }

    pub fn from_sobjects(sobjects: Vec<SObjectSchema>) -> Self {
        let objects = sobjects
            .into_iter()
            .map(|schema| {
                let fields = schema
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (CiKey::from(f.api_name.as_str()), i))
                    .collect();
                (
                    CiKey::from(schema.api_name.as_str()),
                    ObjectEntry { schema, fields },
                )
            })
            .collect();
        SchemaIndex { objects }
    }

    pub fn object(&self, api_name: &str) -> Option<&SObjectSchema> {
        self.objects.get(&CiQuery(api_name)).map(|e| &e.schema)
    }

    pub fn field(&self, object_api_name: &str, field_api_name: &str) -> Option<&FieldSchema> {
        let entry = self.objects.get(&CiQuery(object_api_name))?;
        let idx = *entry.fields.get(&CiQuery(field_api_name))?;
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
        Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: name.into(),
            field: None,
        }))
    } else {
        Resolution::UnknownSchema(Box::new(UnknownSchemaRef {
            object: Some(name.into()),
            field: None,
        }))
    };
    refs.set(ptr, resolution);
}

/// A custom relationship name's field-schema-lookup form (`Batch__r` ->
/// `Batch__c`) -- the common, documented Salesforce convention for
/// custom lookup/master-detail fields; a standard relationship name
/// (`Owner`, `CreatedBy`, ...) has no such transform and is looked up
/// as-is. v1 doesn't model the full relationship-name table Salesforce
/// derives server-side, so a standard relationship name that doesn't
/// happen to equal its field's own API name (rare, but possible) won't
/// hop correctly -- an accepted, documented gap, not silently assumed
/// away. Shared by `crate::soql` (a SOQL relationship-field chain) and
/// `crate::resolve` (the same `__r` alias used in a plain Apex
/// expression, e.g. `dataImport.Related__r.Name__c`).
pub(crate) fn relationship_field_api_name(segment: &str) -> String {
    if segment.len() > 3 && segment.to_ascii_lowercase().ends_with("__r") {
        format!("{}__c", &segment[..segment.len() - 3])
    } else {
        segment.to_string()
    }
}
