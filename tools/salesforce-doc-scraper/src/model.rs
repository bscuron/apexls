//! What gets extracted per page. Deliberately not designed against
//! `apex-binder`'s own types (`Ty`, `SchemaObjectRef`, ...) -- that
//! mapping is separate, deferred follow-on work; this is just "what did
//! the page actually say."

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassModel {
    /// The page slug this was scraped from, e.g. `apex_methods_system_string`.
    pub page_id: String,
    /// The class/interface/enum's own name, e.g. `String` (the page
    /// title's ` Class`/` Interface`/` Enum` suffix is stripped).
    pub name: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub description: Option<String>,
    pub methods: Vec<MethodModel>,
    /// A class-level static/instance property, e.g. `ApexPages.Component.childComponents`
    /// (`public List<ApexPages.Component> childComponents {get; set;}`) --
    /// documented in the same `nested2` leaf shape a method is, but with
    /// no parameter list and a `Property Value` section instead of
    /// `Return Value`. Kept separate from `methods` rather than modeled
    /// as a zero-param method: conflating the two would silently
    /// misreport a property as a method with no arguments, and a
    /// property's own type is genuinely a different concept.
    pub properties: Vec<PropertyModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MethodModel {
    /// The anchor id within the page, e.g. `apex_System_String_isBlank`.
    pub anchor_id: String,
    pub name: String,
    /// The raw `Signature` section text as written, e.g.
    /// `public static Boolean isBlank(String inputString)` -- kept
    /// verbatim alongside the structured fields below since free-text
    /// modifiers (`global`, `testMethod`, ...) aren't all worth their
    /// own structured field yet.
    pub signature: Option<String>,
    pub is_static: bool,
    pub visibility: Option<String>,
    pub return_type: Option<String>,
    pub params: Vec<ParamModel>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PropertyModel {
    pub anchor_id: String,
    pub name: String,
    pub is_static: bool,
    pub visibility: Option<String>,
    pub type_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParamModel {
    pub name: String,
    pub type_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectModel {
    /// The page slug this was scraped from, e.g. `sforce_api_objects_account`.
    pub page_id: String,
    /// The object's API name, e.g. `Account`.
    pub name: String,
    pub fields: Vec<FieldModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldModel {
    pub name: String,
    pub field_type: Option<String>,
    /// As listed in the doc's own `Properties` row, e.g. `["Create",
    /// "Filter", "Group", "Nillable", "Sort", "Update"]` -- kept as the
    /// doc's own vocabulary rather than mapped onto some closed enum,
    /// same reasoning `apex_metadata::FieldSchema::field_type` already
    /// documents for itself (forward-compatible, lossless).
    pub properties: Vec<String>,
    pub description: Option<String>,
    /// The doc's own `Refers To` row for a lookup/master-detail field --
    /// the target object(s) it can point at, e.g. `["Account"]` or, for
    /// a polymorphic field like `Task.OwnerId`, `["Group", "User"]`
    /// (confirmed comma-separated in the real `<dd>` text). Empty for a
    /// non-relationship field.
    pub reference_to: Vec<String>,
}
