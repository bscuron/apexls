//! [`apex_metadata::SObjectSchema`]/[`FieldSchema`] wrapped into a
//! lowercase-keyed lookup index. Salesforce API names, like Apex
//! identifiers, are case-insensitive, so every consultation point in
//! `crate::soql`/`crate::resolve` goes through this rather than
//! matching `api_name` fields directly.

use crate::ci_key::{CiKey, CiMap, CiQuery};
use crate::ptr::SyntaxPtr;
use crate::reference_table::{ReferenceTable, Resolution, SchemaObjectRef, UnknownSchemaRef};
use apex_metadata::{FieldSchema, SObjectSchema};
use smol_str::SmolStr;
use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

pub struct SchemaIndex {
    objects: CiMap<ObjectEntry>,
}

struct ObjectEntry {
    /// Borrowed straight from `apex_stdlib::standard_sobjects()`'s
    /// `'static` slice for the (overwhelming majority of) objects the
    /// project doesn't override, so rebuilding the index doesn't
    /// deep-clone the whole bundled snapshot -- only objects actually
    /// merged with (or wholly defined by) project-local metadata own
    /// their `SObjectSchema`.
    schema: Cow<'static, SObjectSchema>,
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
        Self::from_cow_sobjects(merge_sobjects(apex_metadata::discover_sobjects(root)))
    }

    /// Like [`Self::build`], but parses an already-computed
    /// `apex_discover::Discovery` instead of walking `root` itself.
    pub fn from_discovery(discovery: &apex_discover::Discovery) -> Self {
        Self::from_cow_sobjects(merge_sobjects(apex_metadata::sobjects_from_discovery(discovery)))
    }

    pub fn from_sobjects(sobjects: Vec<SObjectSchema>) -> Self {
        Self::from_cow_sobjects(sobjects.into_iter().map(Cow::Owned).collect())
    }

    fn from_cow_sobjects(sobjects: Vec<Cow<'static, SObjectSchema>>) -> Self {
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
        self.objects.get(&CiQuery(api_name)).map(|e| e.schema.as_ref())
    }

    pub fn field(&self, object_api_name: &str, field_api_name: &str) -> Option<&FieldSchema> {
        let entry = self.objects.get(&CiQuery(object_api_name))?;
        match entry.fields.get(&CiQuery(field_api_name)) {
            Some(&idx) => entry.schema.fields.get(idx),
            // Every real SObject -- standard or custom -- inherits a
            // handful of base fields (`Id`, `OwnerId`, `CreatedDate`, ...)
            // that Salesforce's own docs describe once, in prose, as
            // common to every object rather than repeating per object;
            // neither the bundled `standard_objects.json` scrape nor a
            // project's own `.field-meta.xml` files ever list them
            // per-object (confirmed directly against the raw JSON --
            // zero of Account/Contact/Opportunity/OpportunityContactRole
            // list an `Id` field). `"id"` alone was the single most
            // common unresolved reference across the whole NPSP corpus.
            // Tried only once the object's own real fields have already
            // missed, so a project that *does* declare a custom field
            // that happens to share one of these names still wins.
            None => universal_field(field_api_name),
        }
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

/// Unions the bundled standard-object snapshot
/// (`apex_stdlib::standard_sobjects`) with `local` (this project's own
/// XML-discovered custom objects/fields), keyed case-insensitively by
/// `api_name`. `SchemaIndex::from_sobjects` alone has no such merge
/// behavior -- it's a flat `Vec` -> map build, so feeding it both a
/// bundled `Account` and a locally-discovered `Account` (a standard
/// object with a custom field added) directly would let whichever is
/// later in the `Vec` win outright, silently losing the other's fields.
///
/// An object present in only one list passes through unchanged -- a
/// bundled object `local` doesn't touch is returned as a `Cow::Borrowed`
/// straight into the `'static` snapshot, with no clone at all, since
/// that's true of nearly every standard object on nearly every rebuild.
/// An object in both gets its field lists unioned by the field's own
/// case-insensitive `api_name`, with `local`'s field winning on a
/// collision (a project's own declaration is more authoritative than
/// the bundled generic snapshot) -- and `local`'s `is_custom`/
/// `object_path` used for the merged entry too, for the same reason
/// (only a genuinely project-declared custom object ever has either set
/// to something other than `apex_stdlib`'s own `false`/`None`).
fn merge_sobjects(local: Vec<SObjectSchema>) -> Vec<Cow<'static, SObjectSchema>> {
    let mut local_by_name: CiMap<SObjectSchema> = local
        .into_iter()
        .map(|s| (CiKey::from(s.api_name.as_str()), s))
        .collect();

    let stdlib = apex_stdlib::standard_sobjects();
    let mut merged = Vec::with_capacity(stdlib.len() + local_by_name.len());

    for stdlib_object in stdlib {
        let key = CiKey::from(stdlib_object.api_name.as_str());
        match local_by_name.remove(&key) {
            Some(local_object) => {
                merged.push(Cow::Owned(merge_fields(stdlib_object.clone(), local_object)));
            }
            None => merged.push(Cow::Borrowed(stdlib_object)),
        }
    }

    merged.extend(local_by_name.into_values().map(Cow::Owned));
    merged
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

/// One of the handful of base fields every real SObject -- standard or
/// custom -- inherits (Salesforce's own docs: "Standard Fields Available
/// on All Objects"), looked up case-insensitively the same way a real
/// per-object field is. Built once into a `'static` slice (`OnceLock`,
/// the same pattern `apex_stdlib`'s own bundled-snapshot parsers use):
/// there's exactly one copy of this handful of fields regardless of how
/// many objects [`SchemaIndex::field`] falls back to it for, so there's
/// no reason to rebuild it per object or per lookup. `Owner`/`CreatedBy`/
/// `LastModifiedBy` are deliberately left out here -- those are
/// *relationship* names, not field API names, and get no special
/// handling for the same documented reason [`relationship_field_api_name`]
/// doesn't attempt one either (a standard relationship name has no fixed
/// transform this crate can derive).
fn universal_field(field_api_name: &str) -> Option<&'static FieldSchema> {
    static FIELDS: OnceLock<Vec<FieldSchema>> = OnceLock::new();
    fn field(api_name: &'static str, field_type: &'static str, reference_to: &[&'static str]) -> FieldSchema {
        FieldSchema {
            api_name: SmolStr::new_static(api_name),
            field_type: Some(SmolStr::new_static(field_type)),
            reference_to: reference_to.iter().map(|&s| SmolStr::new_static(s)).collect(),
            source_path: None,
        }
    }
    let fields = FIELDS.get_or_init(|| {
        vec![
            field("Id", "id", &[]),
            field("OwnerId", "reference", &["User"]),
            field("CreatedDate", "datetime", &[]),
            field("CreatedById", "reference", &["User"]),
            field("LastModifiedDate", "datetime", &[]),
            field("LastModifiedById", "reference", &["User"]),
            field("SystemModstamp", "datetime", &[]),
            field("IsDeleted", "boolean", &[]),
        ]
    });
    fields.iter().find(|f| f.api_name.eq_ignore_ascii_case(field_api_name))
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
