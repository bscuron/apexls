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
    pub kind: StdlibKind,
    pub methods: Vec<StdlibMethod>,
    pub properties: Vec<StdlibProperty>,
}

/// Whether a [`StdlibClass`] is a real Apex class, interface, or enum --
/// restored from `RawClass::kind` (dropped entirely until now), needed so
/// a caller can ask "does this name a stdlib *interface*" rather than just
/// "does this name a stdlib type." `standard_classes()`'s own filter
/// already guarantees a `RawClass` only ever survives into a real
/// `StdlibClass` when its raw `kind` is exactly one of the three strings
/// this maps from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibKind {
    Class,
    Interface,
    Enum,
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
    /// The method's real declaration text (`"public static
    /// List<SObject> query(String queryString)"`), still present in the
    /// scraped page even where `return_type`/each param's own `type_name`
    /// lost a generic argument -- see [`parse_signature`]'s own doc
    /// comment for why this is needed at all.
    signature: Option<String>,
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
/// schema, parsed once on first use. Excludes the scraper's own empty
/// navigation-page entries (`kind` other than `Class`/`Interface`/`Enum`,
/// or no methods/properties at all -- roughly 1550 of the ~2220 raw
/// entries, confirmed via direct inspection to be section/index pages
/// like `"Apex Release Notes"`, not real types).
///
/// A further handful of raw entries came through with real content but
/// `kind: "Unknown"` -- the scraper found a real page but couldn't
/// classify it, usually because its title has an atypical shape
/// (`"Email Class (Base Email Methods)"`) or, for `Exception`/`Trigger`
/// specifically, because the whole page is laid out too differently from
/// a normal method-reference table for the scraper's walker to extract
/// any methods/properties at all. Most of these are hand-corrected
/// directly in `data/apex_reference.json` rather than left as an accepted
/// gap, since each was a real, outsized cost once actually found via a
/// real report (`extends Exception`/an inherited `Exception` method,
/// `Trigger.oldMap`/`Trigger.isBefore`/..., a `Messaging.Email` parameter
/// type, ... are all common real Apex): `kind` corrected to `"Class"`,
/// `name` stripped of its title's parenthetical/prefix noise where
/// present, and -- for `Exception`/`Trigger`, whose pages had zero
/// methods/properties actually captured -- their real, common members
/// hand-populated, verified directly against a live connected org (`sf
/// apex run`), not guessed, the same "measure, don't guess" discipline
/// this project's own `sf`-CLI-oracle convention already applies to
/// disputed grammar questions. (`Exception::initCause` in particular
/// returns `void`, not `Exception` as its name might suggest -- confirmed
/// by the exact compile error a wrong guess produced, `Illegal assignment
/// from void to Exception`.) Two of the original raw `"Unknown"` entries
/// stay excluded, deliberately not guessed at: `"Custom Settings Methods"`
/// (its own title isn't a real class name at all, more likely a grouped
/// how-to page than one specific class) and `"ConnectApi.BatchResult"`
/// (name carries its own namespace prefix baked in, a difference in kind
/// from every other entry's plain bare name that needs more thought
/// before assuming a safe rename).
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
        kind: match raw.kind.as_str() {
            "Interface" => StdlibKind::Interface,
            "Enum" => StdlibKind::Enum,
            _ => StdlibKind::Class,
        },
        methods: raw.methods.into_iter().map(to_stdlib_method).collect(),
        properties: raw.properties.into_iter().map(to_stdlib_property).collect(),
    }
}

fn to_stdlib_method(raw: RawMethod) -> StdlibMethod {
    let parsed_signature = raw
        .signature
        .as_deref()
        .and_then(|sig| parse_signature(sig, &raw.name, raw.params.len()));

    let return_type = raw.return_type.as_deref().map(normalize_type_string).map(|rt| {
        if is_bare_collection(&rt) {
            if let Some(derived) = parsed_signature.as_ref().and_then(|(rt, _)| rt.as_deref()) {
                return normalize_type_string(derived);
            }
        }
        rt
    });

    let params = raw
        .params
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let type_name = p.type_name.as_deref().map(normalize_type_string).map(|tn| {
                if is_bare_collection(&tn) {
                    if let Some(derived) = parsed_signature.as_ref().and_then(|(_, params)| params.get(i)) {
                        return normalize_type_string(derived);
                    }
                }
                tn
            });
            StdlibParam {
                name: p.name.as_deref().map(SmolStr::new),
                type_name,
            }
        })
        .collect();

    StdlibMethod {
        name: SmolStr::new(&raw.name),
        is_static: raw.is_static,
        return_type,
        params,
        description: raw.description.as_deref().map(normalize_description),
    }
}

/// `true` for a bare `List`/`Set`/`Map` name with no generic argument at
/// all -- the specific shape [`parse_signature`] is worth consulting for
/// (a real class name, `Object`, or an already-generic `List<Foo>` never
/// needs it).
fn is_bare_collection(type_str: &str) -> bool {
    matches!(type_str.to_ascii_lowercase().as_str(), "list" | "set" | "map")
}

/// Recovers a return type's and each parameter's own generic argument
/// from a method's real signature text, for a confirmed gap in this
/// crate's separately-scraped `return_type`/param `type_name` fields:
/// across the whole bundled `apex_reference.json` snapshot, 168 return
/// types and 132 parameter types came through as a bare `List`/`Set`/
/// `Map` with no argument at all, even though the very same method's own
/// `signature` string -- scraped from the same page, never independently
/// re-derived -- still carries the real, full type (confirmed directly
/// against the raw JSON: `Database.query`'s `return_type` is literally
/// `"List"`, but its `signature` reads `"public static List<SObject>
/// query(String queryString)"`). Real bug this fixes: `SObject x =
/// Database.query(soql);` (real NPSP shape, `UTIL_CurrencyCache.cls` and
/// others) inferred `Database.query`'s result as a bare `List`, not
/// `List<SObject>`, so ticket 23's type-mismatch checkpoints reported the
/// (also-wrong) message "cannot assign a value of type 'List' to a
/// variable of type 'SObject'" instead of correctly flagging the
/// genuinely-wrong element type, or -- for the many `List<Concrete>`-vs-
/// `List` positions elsewhere -- silently missing a real generic-argument
/// comparison [`crate::conversions`]'s own collection handling could
/// otherwise make.
///
/// `expected_param_count` guards against a signature this doesn't parse
/// the way it expects (an unusual layout, a default-value expression with
/// its own parens, ...): if the number of comma-split parameter pieces
/// doesn't match how many params the scraper's own `params` array
/// already found, this returns `None` rather than handing back
/// misaligned positional types. `None` from any other unrecognized shape
/// (no parens at all) leaves both fields exactly as they already were --
/// this only ever *adds* information, never guesses one into existence.
fn parse_signature(signature: &str, method_name: &str, expected_param_count: usize) -> Option<(Option<String>, Vec<String>)> {
    let open = signature.find('(')?;
    let close = signature.rfind(')')?;
    if close < open {
        return None;
    }
    let before = signature[..open].trim_end();
    let return_type = before.strip_suffix(method_name).map(|prefix| last_top_level_segment(prefix.trim_end()));

    let params_str = signature[open + 1..close].trim();
    let param_types: Vec<String> = if params_str.is_empty() {
        Vec::new()
    } else {
        split_top_level(params_str, ',')
            .into_iter()
            .map(|p| param_type_from_slice(p.trim()))
            .collect()
    };
    if param_types.len() != expected_param_count {
        return None;
    }
    Some((return_type, param_types))
}

/// The last whitespace-delimited segment of `s`, where whitespace nested
/// inside a `<...>` pair (e.g. the space in `Map<String, String>`) never
/// counts as a boundary -- so `"public static Map<String, String>"`
/// yields `"Map<String, String>"` as one segment, not two. Scans from the
/// end since the segment wanted is always the *last* one (a return type,
/// which always follows every modifier keyword).
fn last_top_level_segment(s: &str) -> String {
    let mut depth = 0i32;
    let mut boundary = 0;
    for (i, c) in s.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                boundary = i + c.len_utf8();
                break;
            }
            _ => {}
        }
    }
    s[boundary..].trim().to_string()
}

/// The type portion of one `"Type name"`/bare `"Type"` parameter-list
/// entry -- the mirror image of [`last_top_level_segment`]: everything
/// *before* the last top-level whitespace boundary (the parameter's own
/// name, when the scraper captured one) rather than everything after it.
/// A parameter with no captured name (confirmed common -- see
/// [`StdlibParam`]'s own doc comment) has no top-level whitespace at all,
/// so the whole trimmed slice is already just the type.
fn param_type_from_slice(s: &str) -> String {
    let mut depth = 0i32;
    let mut boundary = None;
    for (i, c) in s.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                boundary = Some(i);
                break;
            }
            _ => {}
        }
    }
    match boundary {
        Some(i) => s[..i].trim().to_string(),
        None => s.trim().to_string(),
    }
}

/// Splits `s` on every top-level occurrence of `sep`, skipping any that
/// falls inside a `<...>` pair -- so a parameter list's own generic
/// argument comma (`Map<String,String> headers, String body`) never gets
/// mistaken for the parameter separator itself.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
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

    /// `Database.query`'s raw scraped `return_type` is a bare `"List"`
    /// with no generic argument at all (confirmed directly against the
    /// raw `apex_reference.json`), even though the method's real
    /// signature is `"public static List<SObject> query(String
    /// queryString)"` -- `parse_signature` recovers the missing
    /// `<SObject>` from that signature text instead. Real bug this fixes:
    /// `SObject x = Database.query(soql);` (real NPSP shape) inferred
    /// `Database.query`'s result as a bare `List`, not `List<SObject>`.
    #[test]
    fn a_bare_collection_return_type_is_enriched_from_the_real_signature() {
        let classes = standard_classes();
        let database_class = classes
            .iter()
            .find(|c| c.name == "Database" && c.namespace.as_deref() == Some("System"))
            .expect("Database should be in the bundled snapshot");
        let query = database_class
            .methods
            .iter()
            .find(|m| m.name == "query" && m.params.len() == 1)
            .expect("Database.query(String) should be in the bundled snapshot");
        assert_eq!(query.return_type.as_deref(), Some("List<SObject>"));
    }

    /// The parameter-side mirror of the return-type case above:
    /// `StandardController.addFields`'s single parameter scraped as a
    /// bare `"List"` `type_name`, recovered as `List<String>` from
    /// `"public Void addFields(List<String> fieldNames)"`.
    #[test]
    fn a_bare_collection_param_type_is_enriched_from_the_real_signature() {
        let classes = standard_classes();
        let controller_class = classes
            .iter()
            .find(|c| c.name == "StandardController")
            .expect("StandardController should be in the bundled snapshot");
        let add_fields = controller_class
            .methods
            .iter()
            .find(|m| m.name == "addFields")
            .expect("StandardController.addFields should be in the bundled snapshot");
        assert_eq!(add_fields.params.len(), 1);
        assert_eq!(add_fields.params[0].type_name.as_deref(), Some("List<String>"));
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

    /// `Database.Batchable` is a real, commonly-implemented interface --
    /// confirms `kind` survives `to_stdlib_class` as `Interface`, not the
    /// default `Class`, and that an ordinary class (`Database` itself)
    /// still comes through as `Class`.
    #[test]
    fn kind_distinguishes_a_stdlib_interface_from_a_stdlib_class() {
        let classes = standard_classes();
        let batchable = classes
            .iter()
            .find(|c| c.name == "Batchable" && c.namespace.as_deref() == Some("Database"))
            .expect("Database.Batchable should be in the bundled snapshot");
        assert_eq!(batchable.kind, StdlibKind::Interface);
        let database = classes
            .iter()
            .find(|c| c.name == "Database" && c.namespace.as_deref() == Some("System"))
            .expect("Database should be in the bundled snapshot");
        assert_eq!(database.kind, StdlibKind::Class);
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

    /// `ApexPages.Message`/`ApexPages.Severity` had no scraped page at all
    /// captured with real content (unlike the sibling `ApexPages.Action`
    /// tested above) -- hand-added directly to `data/apex_reference.json`
    /// and verified against a live connected org (`sf apex run`), the
    /// same "measure, don't guess" discipline `Exception`/`Trigger`'s own
    /// hand-corrected entries already use (see `standard_classes`'s doc
    /// comment). Confirmed empirically that `ApexPages.Message` has no
    /// single-`String` constructor and no `setComponentLabel`/
    /// `getStrength` methods, despite those being plausible guesses from
    /// the sibling `getComponentLabel` getter.
    #[test]
    fn apexpages_message_and_severity_are_bundled_as_nested_types() {
        let classes = standard_classes();
        let message = classes
            .iter()
            .find(|c| c.name == "ApexPages.Message")
            .expect("ApexPages.Message should be in the bundled snapshot");
        let ctor_overloads: Vec<_> = message.methods.iter().filter(|m| m.name == "Message").collect();
        assert_eq!(ctor_overloads.len(), 2, "expected both real Message constructors");
        for getter in ["getSummary", "getDetail", "getSeverity", "getComponentLabel"] {
            assert!(
                message.methods.iter().any(|m| m.name == getter),
                "expected {getter} on ApexPages.Message"
            );
        }

        let severity = classes
            .iter()
            .find(|c| c.name == "ApexPages.Severity")
            .expect("ApexPages.Severity should be in the bundled snapshot");
        for value in ["CONFIRM", "ERROR", "FATAL", "INFO", "WARNING"] {
            let prop = severity
                .properties
                .iter()
                .find(|p| p.name == value)
                .unwrap_or_else(|| panic!("expected ApexPages.Severity.{value}"));
            assert!(prop.is_static);
            assert_eq!(prop.type_name.as_deref(), Some("ApexPages.Severity"));
        }
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
