//! Bundled, offline snapshot of standard Salesforce schema: SObject/
//! field schema (`standard_sobjects`) -- the gap `apex_metadata`'s own
//! module doc comment documents as "deliberately left as an open
//! decision for whoever wires up the symbol table" -- and standard-
//! library class/method/property schema (`standard_classes`) -- the
//! `BACKLOG.md` §4 gap that blocks semantic diagnostics from shipping
//! at all, since a real `String.isBlank(...)` call is otherwise
//! indistinguishable from a genuine typo. Standard objects/fields and
//! standard classes/methods have no local metadata files a real project
//! ever ships, since they already exist on every org/runtime. This
//! crate fills both gaps with snapshots scraped directly from
//! Salesforce's own documentation (`tools/salesforce-doc-scraper`),
//! embedded at compile time so `apexls` never makes a network call at
//! runtime.
//!
//! Deliberately its own small `serde`-deriving struct shapes here
//! (`RawObject`/`RawField`, `RawClass`/`RawMethod`/...) rather than
//! depending on the scraper's own `tools/salesforce-doc-scraper::model`
//! types directly -- a workspace member depending on a `tools/` binary
//! crate would be backwards, and the scraper's own `model.rs` doc
//! comment already gives the same reasoning for not designing its
//! output against `apex-binder`'s types either: the two sides of this
//! mapping are deliberately decoupled.
//!
//! **Refreshing a snapshot**: re-run the scraper (see its own README --
//! `scrape-object-reference` for `standard_objects.json`,
//! `scrape-apex-reference` for `apex_reference.json`) and copy the
//! result over the matching file under `data/`. Manual, on a new Apex
//! release -- not automated, matching the scraper's own "runs once per
//! release, not once per build" design.

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

/// One standard Apex class/interface's bundled schema.
#[derive(Debug, Clone, PartialEq)]
pub struct StdlibClass {
    /// e.g. `Some("System")`, `Some("ConnectApi")` -- `None` was never
    /// observed in practice (every real class has one), but the
    /// scraper's own `ClassModel::namespace` is `Option`, so this stays
    /// `Option` too rather than defaulting to a sentinel string.
    pub namespace: Option<SmolStr>,
    pub name: SmolStr,
    pub methods: Vec<StdlibMethod>,
    pub properties: Vec<StdlibProperty>,
}

/// One method or constructor. Constructors appear in the scraped data
/// under `methods` with the class's own (possibly generic, e.g.
/// `"List<T>"`) name rather than as a separate model -- kept as-is
/// here rather than split out, since nothing in `apex-binder` needs to
/// tell a constructor apart from a same-named method for lookup
/// purposes (both are found by matching `name`).
#[derive(Debug, Clone, PartialEq)]
pub struct StdlibMethod {
    pub name: SmolStr,
    pub is_static: bool,
    /// Normalized (whitespace-collapsed, `[]`-sugar rewritten to
    /// `List<T>`) but still one opaque string, e.g. `"List<String>"` --
    /// never pre-split into base+args (see [`split_generic_type`]).
    pub return_type: Option<SmolStr>,
    /// Each parameter, positional.
    pub params: Vec<StdlibParam>,
    pub description: Option<SmolStr>,
}

/// One scraped method/constructor parameter. Either field can be `None`
/// when the scraper couldn't extract it -- confirmed common for both
/// (roughly 1,800 of ~4,900 real scraped params have no `name`, ~2,100
/// no `type_name`), so a caller needs to handle either going missing
/// independently, not just one or the other.
#[derive(Debug, Clone, PartialEq)]
pub struct StdlibParam {
    pub name: Option<SmolStr>,
    pub type_name: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StdlibProperty {
    pub name: SmolStr,
    pub is_static: bool,
    pub type_name: Option<SmolStr>,
    pub description: Option<SmolStr>,
}

#[derive(Deserialize)]
struct RawClass {
    namespace: Option<String>,
    name: String,
    kind: String,
    #[serde(default)]
    methods: Vec<RawMethod>,
    #[serde(default)]
    properties: Vec<RawProperty>,
}

#[derive(Deserialize)]
struct RawMethod {
    name: String,
    is_static: bool,
    return_type: Option<String>,
    #[serde(default)]
    params: Vec<RawParam>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawParam {
    name: Option<String>,
    type_name: Option<String>,
}

#[derive(Deserialize)]
struct RawProperty {
    name: String,
    is_static: bool,
    type_name: Option<String>,
    description: Option<String>,
}

const APEX_REFERENCE_JSON: &str = include_str!("../data/apex_reference.json");

/// Every standard Apex class/interface's bundled method/property
/// schema, parsed once on first use. Excludes both the scraper's own
/// empty navigation-page entries (`kind` other than `Class`/`Interface`,
/// or no methods/properties at all -- roughly 1550 of the ~2220 raw
/// entries, confirmed via direct inspection to be section/index pages
/// like `"Apex Release Notes"`, not real types) and a handful (~8) of
/// synthetic "Unknown"-kind doc-section groupings with real content but
/// an atypical title (e.g. `"Email Class (Base Email Methods)"`) -- an
/// accepted, tiny, documented gap rather than a bespoke title parser
/// for a handful of entries, the same tolerance this project's scraper
/// work has already established for similarly small residuals.
///
/// One of those `"Unknown"`-kind entries was `"Exception Class and
/// Built-In Exceptions"` -- the page documenting `Exception`'s own common
/// methods (`getMessage`, `setMessage`, `getCause`, ...), laid out too
/// differently from a normal method-reference page for the scraper's
/// table walker to extract at all (real content, zero methods captured).
/// Unlike the still-accepted "atypical title" residuals above, this one
/// was worth hand-fixing directly in `data/apex_reference.json`: `extends
/// Exception` and an inherited `Exception` method call are both extremely
/// common in real Apex (every custom exception subclass has exactly this
/// base), so this single entry's absence had an outsized real-world cost.
/// `kind` corrected to `"Class"`, `name` to `"Exception"`, and `methods`
/// populated with its real four constructors and seven common methods --
/// verified directly against a live connected org (`sf apex run`), not
/// guessed, the same "measure, don't guess" discipline this project's own
/// `sf`-CLI-oracle convention already applies to disputed grammar
/// questions. `initCause` in particular returns `void`, not `Exception`
/// as its name might suggest -- confirmed by the exact compile error a
/// wrong guess produced (`Illegal assignment from void to Exception`)
/// before this was corrected.
///
/// Includes `Enum` (104 in the whole corpus, e.g. `LoggingLevel`) --
/// each of its values is modeled as one of its `properties` (`is_static:
/// true`, `type_name`: the enum's own name), so `LoggingLevel.INFO`
/// resolves through the exact same property-lookup path a real stdlib
/// property already does, with no separate enum-value concept needed
/// anywhere in `apex-binder`/`apexls-server`.
pub fn standard_classes() -> &'static [StdlibClass] {
    static CLASSES: OnceLock<Vec<StdlibClass>> = OnceLock::new();
    CLASSES.get_or_init(|| {
        let raw: Vec<RawClass> = serde_json::from_str(APEX_REFERENCE_JSON)
            .expect("bundled data/apex_reference.json failed to parse");
        raw.into_iter()
            .filter(|c| {
                matches!(c.kind.as_str(), "Class" | "Interface" | "Enum")
                    && (!c.methods.is_empty() || !c.properties.is_empty())
            })
            .map(to_stdlib_class)
            .collect()
    })
}

fn to_stdlib_class(raw: RawClass) -> StdlibClass {
    StdlibClass {
        namespace: raw.namespace.map(|n| SmolStr::new(&n)),
        name: SmolStr::new(&raw.name),
        methods: raw.methods.into_iter().map(to_stdlib_method).collect(),
        properties: raw.properties.into_iter().map(to_stdlib_property).collect(),
    }
}

fn to_stdlib_method(raw: RawMethod) -> StdlibMethod {
    StdlibMethod {
        name: SmolStr::new(&raw.name),
        is_static: raw.is_static,
        return_type: raw.return_type.as_deref().map(normalize_type_string),
        params: raw
            .params
            .into_iter()
            .map(|p| StdlibParam {
                name: p.name.as_deref().map(SmolStr::new),
                type_name: p.type_name.as_deref().map(normalize_type_string),
            })
            .collect(),
        description: raw.description.as_deref().map(normalize_description),
    }
}

fn to_stdlib_property(raw: RawProperty) -> StdlibProperty {
    StdlibProperty {
        name: SmolStr::new(&raw.name),
        is_static: raw.is_static,
        type_name: raw.type_name.as_deref().map(normalize_type_string),
        description: raw.description.as_deref().map(normalize_description),
    }
}

/// Cleans up the real messiness confirmed in the scraped data: embedded
/// literal `\n`/run-on indentation from the source HTML (collapsed to
/// single spaces), a stray space before `<`/`,`/`>` in a few entries
/// (e.g. `"Map <String, Boolean>"`), legacy `Type[]` array-sugar
/// rewritten to `List<Type>` so a consumer only ever needs to
/// understand one generic-collection spelling, and a trailing
/// `" Class"`/`" Interface"`/`" Enum"` -- confirmed present on 33 real
/// param/return/property types across the whole corpus (e.g.
/// `System.debug`'s second overload's own `logLevel` parameter comes
/// through as `"LoggingLevel Enum"`, not `"LoggingLevel"`): a
/// cross-reference link's own visible text in the source HTML carries
/// its target's kind suffix baked in, the exact same pattern
/// `tools/salesforce-doc-scraper`'s own `split_title` already strips
/// from a *page title* -- this is that same artifact showing up in a
/// type reference instead.
/// Collapses whitespace runs -- including the embedded literal `\n` and
/// following run-on indentation confirmed throughout the scraped source
/// HTML (present in roughly half of the corpus's descriptions, e.g.
/// `"Returns the expression that is evaluated when the action\nis
/// invoked."`) -- down to single spaces, and trims the ends. Shared by
/// [`normalize_type_string`] and [`normalize_description`] since both
/// see the same source-HTML wrapping artifact.
fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cleans up a scraped method/property description the same way
/// [`normalize_type_string`] cleans up a type string: collapsing the
/// source HTML's embedded `\n` plus run-on indentation to single
/// spaces. Left un-normalized, this shows up verbatim in hover text and
/// signature-help documentation (`apexls-server::capabilities`), which
/// otherwise renders the raw mid-sentence line breaks and indentation.
fn normalize_description(raw: &str) -> SmolStr {
    SmolStr::new(collapse_whitespace(raw))
}

fn normalize_type_string(raw: &str) -> SmolStr {
    let collapsed = collapse_whitespace(raw);
    let collapsed = collapsed
        .replace(" <", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(", ", ",");
    let collapsed = ["Class", "Interface", "Enum"]
        .iter()
        .find_map(|kind| collapsed.strip_suffix(&format!(" {kind}")))
        .map(str::to_string)
        .unwrap_or(collapsed);
    if let Some(base) = collapsed.strip_suffix("[]") {
        SmolStr::new(format!("List<{}>", base.trim()))
    } else {
        SmolStr::new(collapsed)
    }
}

/// Splits a normalized, possibly-generic type string into its base name
/// and type arguments, e.g. `"List<String>"` -> `("List", ["String"])`,
/// `"Map<String,Boolean>"` -> `("Map", ["String", "Boolean"])`,
/// `"Boolean"` -> `("Boolean", [])`. Only ever splits the *outermost*
/// angle-bracket pair, with a naive (not nesting-depth-aware) top-level
/// comma split inside it. Confirmed against the whole real corpus: only
/// one method anywhere has a doubly-nested generic at all
/// (`Map<String,Set<String>>`), and it happens to split correctly here
/// only because its inner `Set<String>` has no comma of its own to
/// confuse the split -- a hypothetical `Map<String,Map<K,V>>` would
/// come out wrong (three pieces instead of two). Accepted as a real but
/// currently-unobserved gap rather than a full recursive parser for a
/// shape that has never actually occurred in this data.
pub fn split_generic_type(type_str: &str) -> (SmolStr, Vec<SmolStr>) {
    match type_str.find('<') {
        Some(open) if type_str.ends_with('>') => {
            let base = &type_str[..open];
            let inner = &type_str[open + 1..type_str.len() - 1];
            let args = inner.split(',').map(SmolStr::new).collect();
            (SmolStr::new(base), args)
        }
        _ => (SmolStr::new(type_str), Vec::new()),
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

    #[test]
    fn embedded_apex_reference_parses_and_has_string_isblank() {
        let classes = standard_classes();
        assert!(
            classes.len() > 500,
            "expected hundreds of standard classes, got {}",
            classes.len()
        );

        let string_class = classes
            .iter()
            .find(|c| c.name == "String" && c.namespace.as_deref() == Some("System"))
            .expect("String should be in the bundled snapshot");
        let is_blank = string_class
            .methods
            .iter()
            .find(|m| m.name == "isBlank")
            .expect("String.isBlank should be in the bundled snapshot");
        assert!(is_blank.is_static);
        assert_eq!(is_blank.return_type.as_deref(), Some("Boolean"));
        assert_eq!(is_blank.params.len(), 1);
        assert_eq!(is_blank.params[0].type_name.as_deref(), Some("String"));
        assert_eq!(is_blank.params[0].name.as_deref(), Some("inputString"));
        assert!(is_blank.description.is_some());
    }

    /// `ApexPages.Action.getExpression`'s scraped description is
    /// `"Returns the expression that is evaluated when the action\nis
    /// invoked."` -- a raw embedded newline from the source HTML's own
    /// line-wrapping, not an intentional paragraph break. Confirms it
    /// comes through the bundled snapshot collapsed to a single space
    /// rather than verbatim, since a verbatim `\n` renders as a
    /// mid-sentence line break in hover/signature-help markdown.
    #[test]
    fn a_methods_description_has_its_source_html_line_wrap_collapsed() {
        let classes = standard_classes();
        let action_class = classes
            .iter()
            .find(|c| c.name == "Action" && c.namespace.as_deref() == Some("ApexPages"))
            .expect("ApexPages.Action should be in the bundled snapshot");
        let get_expression = action_class
            .methods
            .iter()
            .find(|m| m.name == "getExpression")
            .expect("ApexPages.Action.getExpression should be in the bundled snapshot");
        assert_eq!(
            get_expression.description.as_deref(),
            Some("Returns the expression that is evaluated when the action is invoked.")
        );
    }

    /// `Database.query` is genuinely overloaded (1-arg and 2-arg real
    /// forms) -- confirms both survive into the bundled snapshot rather
    /// than one silently overwriting the other.
    #[test]
    fn an_overloaded_stdlib_method_keeps_every_overload() {
        let classes = standard_classes();
        let database = classes
            .iter()
            .find(|c| c.name == "Database" && c.namespace.as_deref() == Some("System"))
            .expect("Database should be in the bundled snapshot");
        let query_overloads: Vec<_> = database.methods.iter().filter(|m| m.name == "query").collect();
        assert_eq!(
            query_overloads.len(),
            2,
            "expected exactly 2 real Database.query overloads, got {}",
            query_overloads.len()
        );
    }

    /// `LoggingLevel` is an `Enum`, not a `Class`/`Interface` -- confirms
    /// `standard_classes` includes enum kinds too, and that each of its
    /// values comes through as a `StdlibProperty` (the scraper's own
    /// `parse_enum_values` models a value as a static property of the
    /// enum's own type, deliberately reusing the property shape rather
    /// than inventing a separate enum-value concept).
    #[test]
    fn an_enums_values_come_through_as_static_properties_of_its_own_type() {
        let classes = standard_classes();
        let logging_level = classes
            .iter()
            .find(|c| c.name == "LoggingLevel")
            .expect("LoggingLevel should be in the bundled snapshot");
        assert_eq!(
            logging_level.properties.len(),
            8,
            "expected all 8 real LoggingLevel values, got {}",
            logging_level.properties.len()
        );
        let info = logging_level
            .properties
            .iter()
            .find(|p| p.name == "INFO")
            .expect("INFO should be one of LoggingLevel's values");
        assert!(info.is_static);
        assert_eq!(info.type_name.as_deref(), Some("LoggingLevel"));
    }

    /// `Test` collides between the `Canvas` and `System` namespaces --
    /// confirms both survive as distinct entries (disambiguation is
    /// `apex-binder::StdlibIndex`'s job, not this crate's).
    #[test]
    fn a_namespace_colliding_class_name_keeps_both_entries() {
        let classes = standard_classes();
        let test_classes: Vec<_> = classes.iter().filter(|c| c.name == "Test").collect();
        assert_eq!(test_classes.len(), 2, "expected both Canvas.Test and System.Test");
        assert!(test_classes.iter().any(|c| c.namespace.as_deref() == Some("Canvas")));
        assert!(test_classes.iter().any(|c| c.namespace.as_deref() == Some("System")));
    }

    #[test]
    fn normalize_type_string_collapses_whitespace_and_rewrites_array_sugar() {
        assert_eq!(normalize_type_string("Map <String, Boolean>").as_str(), "Map<String,Boolean>");
        assert_eq!(normalize_type_string("String\n                    []").as_str(), "List<String>");
        assert_eq!(
            normalize_type_string("List<Messaging.RenderEmailTemplateError>").as_str(),
            "List<Messaging.RenderEmailTemplateError>"
        );
    }

    /// Real, confirmed bug: a cross-reference link's own visible text in
    /// the scraped source HTML carries its target's kind suffix baked in
    /// (`System.debug`'s second overload's `logLevel` parameter comes
    /// through as literally `"LoggingLevel Enum"`, not `"LoggingLevel"`)
    /// -- the same artifact `tools/salesforce-doc-scraper`'s own
    /// `split_title` already strips from a *page title*, showing up here
    /// in a type reference instead. Confirmed via a direct corpus scan
    /// (33 real occurrences) that every real case ends in exactly one of
    /// these three suffixes.
    #[test]
    fn normalize_type_string_strips_a_trailing_kind_suffix_from_a_cross_reference_link() {
        assert_eq!(normalize_type_string("LoggingLevel Enum").as_str(), "LoggingLevel");
        assert_eq!(normalize_type_string("QueueableDuplicateSignature Class").as_str(), "QueueableDuplicateSignature");
        assert_eq!(normalize_type_string("EventPublishFailureCallback Interface").as_str(), "EventPublishFailureCallback");
        // A real class name never contains a space of its own -- only the
        // known kind-suffix artifact does -- so this is safe even for a
        // dotted/namespaced name.
        assert_eq!(
            normalize_type_string("commercepayments.RequestType Enum").as_str(),
            "commercepayments.RequestType"
        );
    }

    #[test]
    fn split_generic_type_separates_base_from_args() {
        assert_eq!(
            split_generic_type("List<String>"),
            (SmolStr::new("List"), vec![SmolStr::new("String")])
        );
        assert_eq!(
            split_generic_type("Map<String,Boolean>"),
            (SmolStr::new("Map"), vec![SmolStr::new("String"), SmolStr::new("Boolean")])
        );
        assert_eq!(split_generic_type("Boolean"), (SmolStr::new("Boolean"), vec![]));
    }
}
