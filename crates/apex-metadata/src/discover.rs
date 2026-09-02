//! Groups the `.object-meta.xml`/`.field-meta.xml` paths
//! `apex_discover::discover` finds by their owning `objects/<ApiName>/`
//! directory, and parses each into a [`FieldSchema`]. No directory
//! walking happens in this crate at all -- `apex_discover` is the single
//! walker shared by Apex-source and metadata discovery alike, so a
//! caller that wants both never pays for reading the same directory
//! tree twice.

use crate::xml::{parse_field_meta, parse_object_meta_name_field};
use crate::{FieldSchema, SObjectSchema};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    #[derive(Default)]
    struct Entry {
        is_custom: bool,
        object_path: Option<PathBuf>,
        fields: Vec<FieldSchema>,
    }
    let mut objects: HashMap<String, Entry> = HashMap::new();

    for path in &found.object_meta_files {
        if let Some(api_name) = object_api_name(path) {
            let entry = objects.entry(api_name).or_default();
            entry.is_custom = true;
            entry.object_path = Some(path.clone());
            // The object's own implicit `Name` field -- see
            // `parse_object_meta_name_field`'s own doc comment for why
            // this is the one field never covered by a separate
            // `fields/*.field-meta.xml` file.
            if let Some(name_field) =
                std::fs::read_to_string(path).ok().and_then(|xml| parse_object_meta_name_field(&xml, path.clone()))
            {
                entry.fields.push(name_field);
            }
        }
    }

    // Parallel (`rayon`): each field-meta file's read + XML parse is
    // fully independent of every other one -- no shared state touched
    // until the sequential merge below, the same parallel-map-then-
    // merge shape `apex-binder`'s own Pass 1/Pass 2 use throughout.
    // Real payoff on a project like NPSP, which has thousands of these
    // (one per custom/standard field), and was previously the single
    // largest un-parallelized stage in a cold bind -- see the `hotpath`-
    // measured finding in `BACKLOG.md` §2 this targets.
    let parsed_fields: Vec<(String, FieldSchema)> = found
        .field_meta_files
        .par_iter()
        .filter_map(|path| {
            let api_name = field_owner_api_name(path)?;
            let content = std::fs::read_to_string(path).ok()?;
            let field = parse_field_meta(&content, path.clone())?;
            Some((api_name, field))
        })
        .collect();
    for (api_name, field) in parsed_fields {
        objects.entry(api_name).or_default().fields.push(field);
    }

    objects
        .into_iter()
        .map(|(api_name, entry)| SObjectSchema {
            api_name: api_name.into(),
            is_custom: entry.is_custom,
            object_path: entry.object_path,
            fields: entry.fields,
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
