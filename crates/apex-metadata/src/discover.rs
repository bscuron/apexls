//! Groups the `.object-meta.xml`/`.field-meta.xml` paths
//! `apex_discover::discover` finds by their owning `objects/<ApiName>/`
//! directory, and parses each into a [`FieldSchema`]. No directory
//! walking happens in this crate at all -- `apex_discover` is the single
//! walker shared by Apex-source and metadata discovery alike, so a
//! caller that wants both never pays for reading the same directory
//! tree twice.

use crate::xml::parse_field_meta;
use crate::{FieldSchema, SObjectSchema};
use std::collections::HashMap;
use std::path::Path;

/// Discovers every `objects/<ApiName>/` directory under `root` and
/// parses its `.object-meta.xml`/`fields/*.field-meta.xml` files into one
/// [`SObjectSchema`] per distinct `<ApiName>`. Unreadable files and
/// malformed XML are silently skipped rather than failing the whole
/// discovery, matching `apex_discover::discover`'s error-tolerance.
///
/// Walks fresh every call -- prefer [`sobjects_from_discovery`] if the
/// caller already has a [`apex_discover::Discovery`] in hand (e.g. it
/// also needs `apex_files`, or is deciding whether to reuse a cached
/// walk at all), so the tree isn't read twice.
pub fn discover_sobjects(root: impl AsRef<Path>) -> Vec<SObjectSchema> {
    sobjects_from_discovery(&apex_discover::discover(root))
}

/// Like [`discover_sobjects`], but parses an already-computed
/// [`apex_discover::Discovery`] instead of walking `root` itself.
pub fn sobjects_from_discovery(found: &apex_discover::Discovery) -> Vec<SObjectSchema> {
    let mut objects: HashMap<String, (bool, Vec<FieldSchema>)> = HashMap::new();

    for path in &found.object_meta_files {
        if let Some(api_name) = object_api_name(path) {
            objects.entry(api_name).or_default().0 = true;
        }
    }

    for path in &found.field_meta_files {
        let Some(api_name) = field_owner_api_name(path) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(field) = parse_field_meta(&content) else {
            continue;
        };
        objects.entry(api_name).or_default().1.push(field);
    }

    objects
        .into_iter()
        .map(|(api_name, (is_custom, fields))| SObjectSchema {
            api_name,
            is_custom,
            fields,
        })
        .collect()
}

/// `objects/<ApiName>/<ApiName>.object-meta.xml` -- only counts if the
/// file's own name matches its parent directory's name, ruling out an
/// unrelated same-suffix file some other tool might have dropped nearby.
fn object_api_name(path: &Path) -> Option<String> {
    let api_name = path
        .file_name()?
        .to_str()?
        .strip_suffix(".object-meta.xml")?;
    let parent_matches = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some(api_name);
    parent_matches.then(|| api_name.to_string())
}

/// `objects/<ApiName>/fields/<FieldName>.field-meta.xml`.
fn field_owner_api_name(path: &Path) -> Option<String> {
    let fields_dir: &Path = path.parent()?;
    let in_fields_dir = fields_dir.file_name().and_then(|n| n.to_str()) == Some("fields");
    if !in_fields_dir {
        return None;
    }
    fields_dir
        .parent()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}
