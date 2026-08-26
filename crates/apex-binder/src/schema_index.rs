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
    /// Walks `root` for SFDX metadata and builds the index from it,
    /// merged with the bundled standard-object schema (see
    /// [`merge_sobjects`]) -- `apex_metadata::discover_sobjects` alone
    /// only ever finds project-local custom objects/fields, per its own
    /// module doc comment. Prefer [`Self::from_discovery`] if the caller
    /// already has an `apex_discover::Discovery` in hand (e.g.
    /// `crate::BoundProgram::from_files_cached`, which also needs it for
    /// `apex_files`), so the directory tree isn't walked twice.
    pub fn build(root: impl AsRef<Path>) -> Self {
        Self::from_sobjects(merge_sobjects(
            apex_stdlib::standard_sobjects().to_vec(),
            apex_metadata::discover_sobjects(root),
        ))
    }

    /// Like [`Self::build`], but parses an already-computed
    /// `apex_discover::Discovery` instead of walking `root` itself.
    pub fn from_discovery(discovery: &apex_discover::Discovery) -> Self {
        Self::from_sobjects(merge_sobjects(
            apex_stdlib::standard_sobjects().to_vec(),
            apex_metadata::sobjects_from_discovery(discovery),
        ))
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

/// Unions `stdlib` (the bundled standard-object snapshot,
/// `apex_stdlib::standard_sobjects`) with `local` (this project's own
/// XML-discovered custom objects/fields), keyed case-insensitively by
/// `api_name`. `SchemaIndex::from_sobjects` alone has no such merge
/// behavior -- it's a flat `Vec` -> map build, so feeding it both a
/// bundled `Account` and a locally-discovered `Account` (a standard
/// object with a custom field added) directly would let whichever is
/// later in the `Vec` win outright, silently losing the other's fields.
///
/// An object present in only one list passes through unchanged. An
/// object in both gets its field lists unioned by the field's own
/// case-insensitive `api_name`, with `local`'s field winning on a
/// collision (a project's own declaration is more authoritative than
/// the bundled generic snapshot) -- and `local`'s `is_custom`/
/// `object_path` used for the merged entry too, for the same reason
/// (only a genuinely project-declared custom object ever has either set
/// to something other than `apex_stdlib`'s own `false`/`None`).
fn merge_sobjects(stdlib: Vec<SObjectSchema>, local: Vec<SObjectSchema>) -> Vec<SObjectSchema> {
    let mut by_name: CiMap<SObjectSchema> = stdlib
        .into_iter()
        .map(|s| (CiKey::from(s.api_name.as_str()), s))
        .collect();

    for local_object in local {
        let key = CiKey::from(local_object.api_name.as_str());
        match by_name.remove(&key) {
            Some(stdlib_object) => {
                by_name.insert(key, merge_fields(stdlib_object, local_object));
            }
            None => {
                by_name.insert(key, local_object);
            }
        }
    }

    by_name.into_values().collect()
}

fn merge_fields(stdlib_object: SObjectSchema, local_object: SObjectSchema) -> SObjectSchema {
    let mut fields_by_name: CiMap<FieldSchema> = stdlib_object
        .fields
        .into_iter()
        .map(|f| (CiKey::from(f.api_name.as_str()), f))
        .collect();
    for field in local_object.fields {
        fields_by_name.insert(CiKey::from(field.api_name.as_str()), field);
    }

    SObjectSchema {
        api_name: local_object.api_name,
        is_custom: local_object.is_custom,
        object_path: local_object.object_path,
        fields: fields_by_name.into_values().collect(),
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
