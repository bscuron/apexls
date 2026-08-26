//! Parses one Apex Reference Guide page's `content` HTML fragment (as
//! returned by `get_document_content/apexref/<page>.htm/en-us/<version>`)
//! into a [`ClassModel`]. Confirmed markup shape (see this tool's own
//! README/the project's planning notes for how this was verified): the
//! class/interface/enum's own description and namespace sit directly in
//! the page body; each individual method or constructor lives in its
//! own `<div class="topic reference nested2">` leaf block with three
//! `<div class="section">` children headed `Signature`, `Parameters`,
//! and `Return Value`. Confirmed (by inspecting real output, not just a
//! single hand-picked fixture -- the first version of this parser only
//! ever checked "more than N methods found" against one class page,
//! which stayed true even with every method double-counted) that a
//! page also wraps each *group* of methods/constructors in an outer
//! `<div class="topic reference nested1">` (e.g. "Action Constructors"),
//! which itself matches a `div.topic.reference` selector -- matching
//! that class alone double-counts every method, once for the group
//! wrapper and once for the real leaf block, since `Html::select`
//! searches all descendants, not just direct children. Scoping the
//! selector to `.nested2` specifically (the real, hierarchy-verified
//! leaf level) fixes this.

use crate::model::{ClassModel, MethodModel, ParamModel, PropertyModel};
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;

/// The complete, confirmed set of title *last words* the doc set's own
/// DITA templates use for a pure navigational "group" page -- one that
/// exists only to list a class/interface's own members (by member kind)
/// and carries no content of its own. Confirmed exhaustively, not
/// guessed: every TOC node in the whole Apex Reference Guide that has
/// children was enumerated and grouped by its own title's last word --
/// `"Methods"` (600 occurrences), `"Properties"` (160), `"Constructors"`
/// (125), plus the singular forms `"Method"`/`"Constructor"` (used for a
/// class with exactly one member of that kind) and `"Fields"` (1 --
/// `Math`'s `E`/`PI` constants) were the *only* recurring pattern; every
/// other last word observed (`"Class"`/`"Interface"`/`"Namespace"`/...,
/// or a real class's own atypical title like `"ConnectApi.BatchResult"`)
/// belongs to a page that either already has a real name/kind or is
/// itself the thing members should be attributed to, not skipped past.
/// `"Property"`/`"Field"` (singular) are included defensively even
/// though not observed in this snapshot, for symmetry with the observed
/// `"Method"`/`"Constructor"` singulars and as a hedge against a future
/// Salesforce release introducing a class with exactly one property or
/// custom field-shaped constant.
const GROUP_PAGE_TITLE_LAST_WORDS: &[&str] = &[
    "Methods",
    "Method",
    "Properties",
    "Property",
    "Constructors",
    "Constructor",
    "Fields",
    "Field",
];

/// True for a title whose own last word is one of
/// [`GROUP_PAGE_TITLE_LAST_WORDS`] -- a pure navigational group page,
/// never a valid reattachment target. Deliberately checks the *title's*
/// last word, not `ClassModel::kind` (which `split_title` only ever
/// derives from an exact `" Class"`/`" Interface"`/`" Enum"` trailing
/// suffix, and so reports `"Unknown"` just as often for a *real* class
/// with an atypical title -- e.g. a dotted qualified name like
/// `"ConnectApi.BatchResult"`, or a `"Class (...)"` title with trailing
/// parenthetical text -- as it does for an actual group page). Checking
/// the title directly instead means a real class is correctly accepted
/// as a reattachment target regardless of whether `split_title` managed
/// to recognize its kind, which is exactly the gap that left 12 members
/// unreattached the first time this function shipped: `kind != "Unknown"`
/// was being used as a stand-in for "is this a real page," but the two
/// questions aren't the same one.
fn is_group_page_title(title: &str) -> bool {
    title
        .split_whitespace()
        .next_back()
        .is_some_and(|last| GROUP_PAGE_TITLE_LAST_WORDS.contains(&last))
}

/// Merges an orphaned member (a `kind: "Unknown"` pseudo-class produced
/// by `parse_class_page`'s whole-page fallback -- see this module's own
/// doc comment) back into the real class/interface/enum it actually
/// belongs to, using `parents` (`crate::toc::fetch_parent_map`'s output)
/// to walk up the doc's own TOC hierarchy. Confirmed real shape: a
/// member's own page nests under a "Methods"/"Constructors"/"Properties"/
/// "Fields" *group* page (see [`is_group_page_title`]), which nests
/// under the real class/interface/enum page -- this walks past any
/// number of such group-page levels rather than assuming exactly one
/// (some classes are two levels deep, member page directly under the
/// class; others, three, through an intermediate group page).
///
/// An orphan whose ancestor chain never reaches an already-scraped,
/// non-group-page ancestor is left in the output as-is (never silently
/// dropped) -- happens for a member page whose logical parent wasn't in
/// this run's own page list for some reason (a `--limit`-truncated run,
/// most likely), so keeping the orphan's own record is strictly better
/// than losing the data outright.
pub fn reattach_orphaned_members(mut classes: Vec<ClassModel>, parents: &HashMap<String, String>) -> Vec<ClassModel> {
    let by_page_id: HashMap<String, usize> = classes
        .iter()
        .enumerate()
        .map(|(i, c)| (c.page_id.clone(), i))
        .collect();

    let mut merge_into: Vec<(usize, usize)> = Vec::new(); // (orphan_idx, ancestor_idx)
    for (i, class) in classes.iter().enumerate() {
        let is_orphan = class.kind == "Unknown" && (!class.methods.is_empty() || !class.properties.is_empty());
        if !is_orphan {
            continue;
        }
        let mut current = class.page_id.clone();
        // Bounded, not because a real TOC should ever cycle, but as a
        // cheap defensive guard against a malformed one looping forever.
        for _ in 0..32 {
            let Some(parent_id) = parents.get(&current) else {
                break;
            };
            if let Some(&idx) = by_page_id.get(parent_id) {
                if !is_group_page_title(&classes[idx].name) && classes[idx].page_id != class.page_id {
                    merge_into.push((i, idx));
                    break;
                }
            }
            current = parent_id.clone();
        }
    }

    let mut merged_orphans = vec![false; classes.len()];
    for (orphan_idx, ancestor_idx) in merge_into {
        let methods = std::mem::take(&mut classes[orphan_idx].methods);
        let properties = std::mem::take(&mut classes[orphan_idx].properties);
        classes[ancestor_idx].methods.extend(methods);
        classes[ancestor_idx].properties.extend(properties);
        merged_orphans[orphan_idx] = true;
    }

    let mut kept = Vec::with_capacity(classes.len());
    for (i, class) in classes.into_iter().enumerate() {
        if !merged_orphans[i] {
            kept.push(class);
        }
    }
    kept
}

/// Apex modifier keywords that can appear in a `Signature` section's
/// text -- used to tell a constructor's signature (no return type at
/// all, e.g. `public Action(String action)`) apart from a method's
/// (`public static Boolean isBlank(String inputString)`): the token
/// immediately before `<name>(` is the return type only when it *isn't*
/// itself one of these.
const MODIFIER_KEYWORDS: &[&str] = &[
    "public",
    "global",
    "protected",
    "private",
    "static",
    "abstract",
    "final",
    "virtual",
    "override",
    "testmethod",
    "webservice",
    "transient",
];

pub fn parse_class_page(page_id: &str, title: &str, content_html: &str) -> ClassModel {
    let (name, kind) = split_title(title);
    let document = Html::parse_fragment(content_html);
    let root = document.root_element();

    let description = first_text(&root, "div.shortdesc");
    let namespace = find_section_value(&root, "Namespace");

    // `.nested2` specifically -- see this module's own doc comment for
    // why matching the broader `div.topic.reference` alone double-counts.
    let leaf_sel = selector("div.topic.reference.nested2");
    let header_sel = selector("h3.helpHead3");
    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let leaves: Vec<ElementRef> = document.select(&leaf_sel).collect();
    if leaves.is_empty() {
        // No `nested2` leaves at all -- confirmed real, not a parsing
        // failure: some interfaces (e.g. `canvas.ApplicationContext`)
        // document each method on its own dedicated page instead of as
        // anchors within a shared class page. On that shape, the page's
        // own `<h1>` *is* the member's name/signature-bearing header, and
        // `Signature`/`Parameters`/`Return Value` sit directly under the
        // page body as `<h2>` sections (the same heading level a normal
        // class page's own `Namespace`/`Usage` sections use) rather than
        // the `<h3>`/`<h4>` a `nested2` leaf uses -- `find_section`
        // already checks both levels, so treating the whole page as one
        // implicit leaf (using `page_id` as its anchor id, since it's a
        // real one-page-per-member mapping) reuses the exact same
        // extraction logic unchanged.
        if find_section(&root, "Signature").is_some() {
            let is_property = !title.contains('(');
            if is_property {
                properties.push(parse_property(root, page_id, title));
            } else {
                methods.push(parse_method(root, page_id, title));
            }
        } else if kind == "Enum" {
            properties = parse_enum_values(&root, page_id, &name);
        }
    } else {
        for el in leaves {
            // A property's own `<h3>` is just its bare name
            // (`childComponents`); a method's/constructor's always has a
            // parameter list, even an empty one (`childComponents()`,
            // `isBlank(inputString)`) -- this is a more reliable signal
            // than checking the `Signature` text itself for a `(`, since
            // that's exactly the field a property's own `Signature`
            // (`public List<T> name {get; set;}`) lacks.
            let header = first_text_in(&el, &header_sel).unwrap_or_default();
            let anchor_id = el.value().attr("id").unwrap_or_default();
            let is_property = !header.contains('(');
            if is_property {
                properties.push(parse_property(el, anchor_id, &header));
            } else {
                methods.push(parse_method(el, anchor_id, &header));
            }
        }
    }

    ClassModel {
        page_id: page_id.to_string(),
        name,
        kind,
        namespace,
        description,
        methods,
        properties,
    }
}

/// An `Enum` page's own values list -- confirmed a completely different
/// shape from a Class/Interface page's Methods/Properties/Constructors
/// sections: no `nested2` leaves, no `Signature` section at all (so the
/// ordinary whole-page-fallback branch above never fires for one
/// either). Modeled as [`PropertyModel`]s (`is_static: true`,
/// `type_name`: the enum's own name, since each value literally *is* an
/// instance of its own enum type) rather than inventing a separate
/// enum-value model -- this lets `LoggingLevel.INFO`-style access reuse
/// every bit of existing property lookup/hover machinery `apex-binder`/
/// `apexls-server` already have for a stdlib property, with zero new
/// code needed on that side at all.
///
/// Two real, confirmed shapes, tried in order (auditing every real enum
/// with zero captured values, not just the one fixture the first version
/// of this function was written against -- 4 of 104 came back empty
/// against a naive first pass, all for a real, different reason):
/// 1. A `<table>` -- but *not* reliably `<td data-title="Value">`/
///    `<td data-title="Description">`: `DisplayType`/`SOAPType`/
///    `JsonToken` instead use entirely different column labels
///    (`"Type Field Value"`/`"What the Field Object Contains"`, say),
///    so this keys off structural position instead (first `<td>` in a
///    `<tbody>` row is the value, second is the description) --
///    matching the same lesson `object_reference.rs`'s own parser
///    already learned from `data-title` varying there too.
/// 2. No table at all: `TriggerOperation`'s page instead lists its
///    values as `<ul class="ul bulletList"><li>0: BEFORE_INSERT</li>...`
///    -- an ordinal, a colon, then the bare value name, no separate
///    description. Tried only when the table shape finds nothing.
fn parse_enum_values(root: &ElementRef, page_id: &str, enum_name: &str) -> Vec<PropertyModel> {
    let from_table = parse_enum_values_table(root, page_id, enum_name);
    if !from_table.is_empty() {
        return from_table;
    }
    parse_enum_values_bullet_list(root, page_id, enum_name)
}

fn parse_enum_values_table(root: &ElementRef, page_id: &str, enum_name: &str) -> Vec<PropertyModel> {
    let row_sel = selector("tbody tr"); // `tbody` specifically excludes the `<thead>` header row.
    let td_sel = selector("td");
    root.select(&row_sel)
        .filter_map(|row| {
            let mut cells = row.select(&td_sel);
            let value = cells.next()?.text().collect::<String>().trim().to_string();
            if value.is_empty() {
                return None;
            }
            let description = cells
                .next()
                .map(|d| d.text().collect::<String>().trim().to_string())
                .filter(|s| !s.is_empty());
            Some(PropertyModel {
                anchor_id: format!("{page_id}_{value}"),
                name: value,
                is_static: true,
                visibility: Some("public".to_string()),
                type_name: Some(enum_name.to_string()),
                description,
            })
        })
        .collect()
}

fn parse_enum_values_bullet_list(root: &ElementRef, page_id: &str, enum_name: &str) -> Vec<PropertyModel> {
    let li_sel = selector("ul.bulletList li");
    root.select(&li_sel)
        .filter_map(|li| {
            let text = li.text().collect::<String>();
            // "0: BEFORE_INSERT" -> "BEFORE_INSERT" (tolerates a missing
            // "N: " ordinal prefix too, via `next_back` on a single
            // no-colon segment). A real value name never contains
            // whitespace, so this also guards against some unrelated
            // bullet list elsewhere on the page being swept in.
            let value = text.rsplit(':').next()?.trim().to_string();
            if value.is_empty() || value.contains(char::is_whitespace) {
                return None;
            }
            Some(PropertyModel {
                anchor_id: format!("{page_id}_{value}"),
                name: value,
                is_static: true,
                visibility: Some("public".to_string()),
                type_name: Some(enum_name.to_string()),
                description: None,
            })
        })
        .collect()
}

/// `el` is either a real `nested2` leaf (`header` from its own `<h3>`,
/// `anchor_id` from its own `id` attribute) or, for the whole-page
/// fallback case, the page's own root element (`header` is the page
/// title, `anchor_id` is the page slug) -- see `parse_class_page`'s own
/// doc comment for why both shapes are real.
fn parse_property(el: ElementRef, anchor_id: &str, header: &str) -> PropertyModel {
    let anchor_id = anchor_id.to_string();
    let name = header.split('(').next().unwrap_or(header).trim().to_string();
    let description = first_text(&el, "div.shortdesc");
    let signature = find_section_samp(&el, "Signature");
    let (visibility, is_static) = signature
        .as_deref()
        .map(|sig| {
            let words: Vec<&str> = sig.split_whitespace().collect();
            let is_static = words.contains(&"static");
            let visibility = words
                .iter()
                .find(|w| matches!(**w, "public" | "global" | "protected" | "private"))
                .map(|w| w.to_string());
            (visibility, is_static)
        })
        .unwrap_or((None, false));
    // `Property Value` is this section's name for a property, the same
    // role `Return Value` plays for a method.
    let type_name = find_section_xref_text(&el, "Property Value");

    PropertyModel {
        anchor_id,
        name,
        is_static,
        visibility,
        type_name,
        description,
    }
}

/// `"String Class"` -> `("String", "Class")`; also handles `Interface`/
/// `Enum` suffixes. Falls back to `"Unknown"` for a kind this doc set
/// doesn't use one of those three suffixes for (rare, e.g. some
/// exception-hierarchy pages) rather than guessing.
fn split_title(title: &str) -> (String, String) {
    for suffix in [" Class", " Interface", " Enum"] {
        if let Some(name) = title.strip_suffix(suffix) {
            return (name.to_string(), suffix.trim().to_string());
        }
    }
    (title.to_string(), "Unknown".to_string())
}

/// `el`/`anchor_id`/`header` follow the same "real `nested2` leaf vs.
/// whole-page fallback" shape `parse_property` documents.
fn parse_method(el: ElementRef, anchor_id: &str, header: &str) -> MethodModel {
    let anchor_id = anchor_id.to_string();
    let name = header
        .split('(')
        .next()
        .unwrap_or(header)
        .trim()
        .to_string();
    let description = first_text(&el, "div.shortdesc");

    let signature = find_section_samp(&el, "Signature");
    let (visibility, is_static, return_type) = signature
        .as_deref()
        .map(|sig| split_signature(sig, &name))
        .unwrap_or((None, false, None));

    let params = parse_params(&el);
    // The `Return Value` section's own `xref` link is authoritative
    // (it's the doc's own structured field, not a free-text parse) --
    // prefer it over whatever `split_signature` guessed, falling back
    // to the signature-derived guess only if this method has no
    // `Return Value` section at all (a `void` method, typically).
    let return_type = find_section_xref_text(&el, "Return Value").or(return_type);

    MethodModel {
        anchor_id,
        name,
        signature,
        is_static,
        visibility,
        return_type,
        params,
        description,
    }
}

/// Splits `"public static Boolean isBlank(String inputString)"` into
/// `(Some("public"), true, Some("Boolean"))`, using the already-known
/// method `name` to find where the modifier/return-type prefix ends.
/// The token immediately before `"<name>("` is the return type *unless*
/// it's itself a recognized modifier keyword ([`MODIFIER_KEYWORDS`]) --
/// a constructor's signature (`public Action(String action)`) has no
/// return type at all, so naively always taking the last prefix word as
/// the return type misreads a constructor's own visibility modifier as
/// one (confirmed against a real page: `ApexPages.Action`'s constructor
/// was coming out with `return_type: Some("public")`).
fn split_signature(signature: &str, name: &str) -> (Option<String>, bool, Option<String>) {
    let marker = format!("{name}(");
    let Some(idx) = signature.find(&marker) else {
        return (None, false, None);
    };
    let prefix_words: Vec<&str> = signature[..idx].split_whitespace().collect();
    if prefix_words.is_empty() {
        return (None, false, None);
    }
    let last_is_modifier = prefix_words
        .last()
        .is_some_and(|w| MODIFIER_KEYWORDS.contains(&w.to_ascii_lowercase().as_str()));
    let (return_type, modifiers): (Option<&str>, &[&str]) = if last_is_modifier {
        (None, &prefix_words)
    } else {
        let (&last, rest) = prefix_words.split_last().unwrap();
        (Some(last), rest)
    };
    let is_static = modifiers.contains(&"static");
    let visibility = modifiers
        .iter()
        .find(|w| matches!(**w, "public" | "global" | "protected" | "private"))
        .map(|w| w.to_string());
    (visibility, is_static, return_type.map(|s| s.to_string()))
}

fn parse_params(el: &ElementRef) -> Vec<ParamModel> {
    let Some(section) = find_section(el, "Parameters") else {
        return Vec::new();
    };
    let li_sel = selector("li.li");
    let name_sel = selector("var.varname, strong.ph");
    let type_sel = selector("a.xref");
    section
        .select(&li_sel)
        .map(|li| {
            let name = first_text_in(&li, &name_sel).unwrap_or_default();
            let type_name = first_text_in(&li, &type_sel);
            ParamModel { name, type_name }
        })
        .collect()
}

/// The `<div class="section">` among `el`'s descendants whose own
/// `<h4 class="helpHead4">` (or `<h2>`, for the class-level `Namespace`
/// section) text exactly matches `heading` -- every section on a real
/// page is one of a small fixed set (`Signature`/`Parameters`/`Return
/// Value`/`Example`/... for a method, `Namespace`/`Usage`/... for a
/// class), so an exact match is precise, not fragile.
fn find_section<'a>(el: &ElementRef<'a>, heading: &str) -> Option<ElementRef<'a>> {
    let section_sel = selector("div.section");
    let heading_sel = selector("h2.helpHead2, h4.helpHead4");
    el.select(&section_sel).find(|section| {
        first_text_in(section, &heading_sel).as_deref() == Some(heading)
    })
}

fn find_section_value(el: &ElementRef, heading: &str) -> Option<String> {
    let section = find_section(el, heading)?;
    let xref_sel = selector("a.xref");
    first_text_in(&section, &xref_sel).or_else(|| first_text(&section, "p.p"))
}

fn find_section_samp(el: &ElementRef, heading: &str) -> Option<String> {
    let section = find_section(el, heading)?;
    let samp_sel = selector("samp");
    first_text_in(&section, &samp_sel)
}

fn find_section_xref_text(el: &ElementRef, heading: &str) -> Option<String> {
    let section = find_section(el, heading)?;
    let xref_sel = selector("a.xref");
    first_text_in(&section, &xref_sel)
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).unwrap_or_else(|e| panic!("invalid selector {css:?}: {e:?}"))
}

fn first_text(el: &ElementRef, css: &str) -> Option<String> {
    first_text_in(el, &selector(css))
}

fn first_text_in(el: &ElementRef, sel: &Selector) -> Option<String> {
    el.select(sel).next().map(|found| {
        found
            .text()
            .collect::<String>()
            .trim()
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn load_fixture(name: &str) -> serde_json::Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn parses_the_real_string_class_page_fixture() {
        let fixture = load_fixture("apex_methods_system_string.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_methods_system_string", title, content);

        assert_eq!(class.name, "String");
        assert_eq!(class.kind, "Class");
        assert_eq!(class.namespace.as_deref(), Some("System"));
        assert!(class.description.is_some());
        assert!(
            class.methods.len() > 30,
            "expected the String class to have more than 30 documented methods, got {}",
            class.methods.len()
        );

        let is_blank = class
            .methods
            .iter()
            .find(|m| m.name == "isBlank")
            .expect("isBlank should have been parsed");
        assert_eq!(is_blank.anchor_id, "apex_System_String_isBlank");
        assert_eq!(is_blank.visibility.as_deref(), Some("public"));
        assert!(is_blank.is_static);
        assert_eq!(is_blank.return_type.as_deref(), Some("Boolean"));
        assert_eq!(is_blank.params.len(), 1);
        assert_eq!(is_blank.params[0].name, "inputString");
        assert_eq!(is_blank.params[0].type_name.as_deref(), Some("String"));

        let compare_to = class
            .methods
            .iter()
            .find(|m| m.name == "compareTo")
            .expect("compareTo should have been parsed");
        assert_eq!(compare_to.params.len(), 1);
        assert_eq!(compare_to.return_type.as_deref(), Some("Integer"));
    }

    /// Real, confirmed bug: an `Enum` page has neither `nested2` leaves
    /// nor a `Signature` section at all, so before `parse_enum_values`
    /// existed, every real enum (104 in the whole corpus) silently came
    /// out with zero methods *and* zero properties -- `LoggingLevel`
    /// (used by `System.debug`'s own second overload, arguably the most
    /// common enum in real Apex code) had no way to resolve `.INFO`/
    /// `.DEBUG`/etc. at all.
    #[test]
    fn parses_the_real_logginglevel_enums_values_as_static_properties() {
        let fixture = load_fixture("apex_enum_System_LoggingLevel.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_enum_System_LoggingLevel", title, content);

        assert_eq!(class.name, "LoggingLevel");
        assert_eq!(class.kind, "Enum");
        assert!(class.methods.is_empty(), "an enum has no methods");
        assert_eq!(
            class.properties.len(),
            8,
            "expected all 8 real LoggingLevel values (NONE/ERROR/WARN/INFO/DEBUG/FINE/FINER/FINEST), got {:?}",
            class.properties.iter().map(|p| &p.name).collect::<Vec<_>>()
        );

        let info = class
            .properties
            .iter()
            .find(|p| p.name == "INFO")
            .expect("INFO should have been parsed as one of LoggingLevel's values");
        assert!(info.is_static);
        assert_eq!(info.type_name.as_deref(), Some("LoggingLevel"));
        assert_eq!(info.description.as_deref(), Some("Informational logging."));

        // Regression guard for the group-wrapper/leaf double-counting
        // bug this module's own doc comment describes: every anchor id
        // must be unique. The pre-fix version of this parser passed
        // every assertion above too (`len() > 30` doesn't notice each
        // method appearing twice) -- this is the check that actually
        // would have caught it.
        let mut anchor_ids: Vec<&str> = class.methods.iter().map(|m| m.anchor_id.as_str()).collect();
        let total = anchor_ids.len();
        anchor_ids.sort_unstable();
        anchor_ids.dedup();
        assert_eq!(
            anchor_ids.len(),
            total,
            "found duplicate method anchor ids -- the group-wrapper/leaf selector scoping regressed"
        );
    }

    /// Real, confirmed second table variant: `DisplayType`'s own values
    /// table uses `data-title="Type Field Value"`/`"What the Field
    /// Object Contains"`, not `"Value"`/`"Description"` -- found by
    /// auditing every real enum with zero captured values rather than
    /// trusting `LoggingLevel`'s own shape was representative of all
    /// 104. `parse_enum_values_table` keys off structural position
    /// (first/second `<td>`) specifically so this doesn't matter.
    #[test]
    fn parses_the_real_displaytype_enum_using_a_differently_labeled_table() {
        let fixture = load_fixture("apex_enum_Schema_DisplayType.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_enum_Schema_DisplayType", title, content);

        assert_eq!(class.name, "DisplayType");
        assert_eq!(class.kind, "Enum");
        assert_eq!(
            class.properties.len(),
            30,
            "expected all 30 real DisplayType values, got {}",
            class.properties.len()
        );
        let address = class
            .properties
            .iter()
            .find(|p| p.name == "ADDRESS")
            .expect("ADDRESS should have been parsed");
        assert_eq!(address.type_name.as_deref(), Some("DisplayType"));
        assert_eq!(address.description.as_deref(), Some("Address values"));
    }

    /// Real, confirmed third shape: `TriggerOperation`'s page has no
    /// table at all -- its values are a bare `<ul class="ul bulletList">`
    /// of `<li>0: BEFORE_INSERT</li>` (ordinal, colon, value name, no
    /// description). `parse_enum_values` only tries this fallback when
    /// the table shape finds nothing.
    #[test]
    fn parses_the_real_triggeroperation_enum_from_its_bullet_list_shape() {
        let fixture = load_fixture("apex_enum_System_TriggerOperation.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_enum_System_TriggerOperation", title, content);

        assert_eq!(class.name, "TriggerOperation");
        assert_eq!(class.kind, "Enum");
        assert_eq!(
            class.properties.len(),
            7,
            "expected all 7 real TriggerOperation values, got {:?}",
            class.properties.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        let before_insert = class
            .properties
            .iter()
            .find(|p| p.name == "BEFORE_INSERT")
            .expect("BEFORE_INSERT should have been parsed");
        assert!(before_insert.is_static);
        assert_eq!(before_insert.type_name.as_deref(), Some("TriggerOperation"));
        assert!(before_insert.description.is_none(), "this shape has no per-value description");
    }

    /// `ApexPages.Action`'s only constructor: `public Action(String action)`
    /// -- no return type at all, since Apex constructors don't have one.
    /// Regression test for a real bug this parser had: naively taking
    /// "the word before `Name(`" as the return type misread this
    /// constructor's own `public` visibility modifier as its return
    /// type. Also exercises the real page structure that caused the
    /// group-wrapper double-counting bug (`Action Constructors`/`Action
    /// Methods` `nested1` wrapper divs around the real `nested2` leaves).
    #[test]
    fn parses_a_real_constructor_without_mistaking_its_modifier_for_a_return_type() {
        let fixture = load_fixture("apex_pages_action.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_pages_action", title, content);

        assert_eq!(class.name, "Action");
        assert_eq!(class.namespace.as_deref(), Some("ApexPages"));

        let ctor = class
            .methods
            .iter()
            .find(|m| m.anchor_id == "apex_ApexPages_Action_ctor")
            .expect("the Action constructor should have been parsed");
        assert_eq!(ctor.name, "Action");
        assert_eq!(ctor.visibility.as_deref(), Some("public"));
        assert!(!ctor.is_static);
        assert_eq!(
            ctor.return_type, None,
            "a constructor has no return type -- must not mistake its `public` modifier for one"
        );
        assert_eq!(ctor.params.len(), 1);
        assert_eq!(ctor.params[0].name, "action");
        assert_eq!(ctor.params[0].type_name.as_deref(), Some("String"));

        // The constructor must be counted exactly once, not once for
        // the "Action Constructors" group wrapper and once for the real
        // leaf block.
        let ctor_count = class
            .methods
            .iter()
            .filter(|m| m.anchor_id == "apex_ApexPages_Action_ctor")
            .count();
        assert_eq!(ctor_count, 1, "the constructor was double-counted");
    }

    /// `ApexPages.Component` documents only properties, no methods --
    /// each one's `<h3>` is a bare name (`childComponents`, no
    /// parameter list at all, unlike even a zero-arg method's
    /// `name()`), and its `Signature` section has no `(` -- e.g.
    /// `public List<ApexPages.Component> childComponents {get; set;}`.
    /// Regression test for a real bug this parser had: treating that
    /// shape as a method (which needs `Name(` to appear in the
    /// signature to find where modifiers end) silently produced a
    /// method with no visibility and no return type at all, rather than
    /// either correctly modeling it as a property or omitting it.
    #[test]
    fn parses_real_properties_separately_from_methods() {
        let fixture = load_fixture("apex_pages_dynamic_components.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_pages_dynamic_components", title, content);

        assert_eq!(class.name, "Component");
        assert!(
            class.methods.is_empty(),
            "Component only documents properties -- none should have been misparsed as methods: {:?}",
            class.methods
        );
        assert_eq!(class.properties.len(), 3, "{:?}", class.properties);

        let child_components = class
            .properties
            .iter()
            .find(|p| p.name == "childComponents")
            .expect("childComponents should have been parsed as a property");
        assert_eq!(child_components.visibility.as_deref(), Some("public"));
        assert!(!child_components.is_static);
        assert_eq!(child_components.type_name.as_deref(), Some("List"));
        assert!(child_components.description.is_some());
    }

    /// `canvas.ApplicationContext.getCanvasUrl()` -- one of a real
    /// interface's methods documented on its own dedicated page rather
    /// than as a `nested2` anchor within a shared class page. No
    /// `nested2` wrapper exists anywhere on this page at all; the page's
    /// own `<h1>` *is* the method's header, and `Signature`/`Return
    /// Value` sit directly under the body as `<h2>` sections instead of
    /// the `<h3>`/`<h4>` a normal leaf uses. Regression test for a real
    /// bug: the first version of this parser only ever looked for
    /// `div.topic.reference.nested2` elements, so a page shaped like
    /// this silently produced zero methods and zero properties --
    /// discovered by noticing `ApplicationContext`'s own real content
    /// (confirmed by hand to have several methods) came out with an
    /// empty `methods` list in a full scrape.
    #[test]
    fn parses_a_method_documented_on_its_own_dedicated_page() {
        let fixture = load_fixture("apex_canvas_ApplicationContext_getCanvasUrl.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let class = parse_class_page("apex_canvas_ApplicationContext_getCanvasUrl", title, content);

        assert_eq!(class.properties.len(), 0);
        assert_eq!(class.methods.len(), 1, "{:?}", class.methods);
        let method = &class.methods[0];
        assert_eq!(method.anchor_id, "apex_canvas_ApplicationContext_getCanvasUrl");
        assert_eq!(method.name, "getCanvasUrl");
        assert_eq!(method.visibility.as_deref(), Some("public"));
        assert!(!method.is_static);
        assert_eq!(method.return_type.as_deref(), Some("String"));
        assert!(method.params.is_empty());
        assert!(method.description.is_some());
    }

    fn orphan(page_id: &str, method_name: &str) -> ClassModel {
        ClassModel {
            page_id: page_id.to_string(),
            name: method_name.to_string(),
            kind: "Unknown".to_string(),
            methods: vec![MethodModel {
                anchor_id: page_id.to_string(),
                name: method_name.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn real_class(page_id: &str, name: &str) -> ClassModel {
        ClassModel {
            page_id: page_id.to_string(),
            name: name.to_string(),
            kind: "Interface".to_string(),
            ..Default::default()
        }
    }

    /// `name` must actually end in a recognized group-page word (e.g.
    /// `"ApplicationContext Methods"`) -- `is_group_page_title` checks
    /// the title's own last word, not `kind`, so an empty/placeholder
    /// name here would (incorrectly, for what this helper is meant to
    /// simulate) look like a valid reattachment target instead of a
    /// group page to skip past.
    fn group_page(page_id: &str, name: &str) -> ClassModel {
        ClassModel {
            page_id: page_id.to_string(),
            name: name.to_string(),
            kind: "Unknown".to_string(),
            ..Default::default()
        }
    }

    /// The real, confirmed shape: `ApplicationContext Interface` ->
    /// `ApplicationContext Methods` (an `Unknown`-kind group page with
    /// no content of its own) -> `getCanvasUrl()` (an orphaned,
    /// `Unknown`-kind pseudo-class holding the one real method).
    /// `reattach_orphaned_members` must walk past the empty group level
    /// and merge the method into the real interface.
    #[test]
    fn reattaches_an_orphaned_member_through_an_intermediate_group_page() {
        let classes = vec![
            real_class("apex_interface_canvas_ApplicationContext", "ApplicationContext"),
            group_page("apex_canvas_ApplicationContext_methods", "ApplicationContext Methods"),
            orphan("apex_canvas_ApplicationContext_getCanvasUrl", "getCanvasUrl"),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "apex_canvas_ApplicationContext_methods".to_string(),
            "apex_interface_canvas_ApplicationContext".to_string(),
        );
        parents.insert(
            "apex_canvas_ApplicationContext_getCanvasUrl".to_string(),
            "apex_canvas_ApplicationContext_methods".to_string(),
        );

        let result = reattach_orphaned_members(classes, &parents);

        // The orphan and the empty group page are both gone from the
        // top-level list -- the orphan because it was merged, the group
        // page because it never had any content merged into it either
        // way (it's just left in place, actually -- assert on that
        // separately below to be precise about which claim this is).
        assert!(
            !result.iter().any(|c| c.page_id == "apex_canvas_ApplicationContext_getCanvasUrl"),
            "the orphan should have been merged away, not kept as its own entry"
        );

        let app_context = result
            .iter()
            .find(|c| c.page_id == "apex_interface_canvas_ApplicationContext")
            .expect("the real ApplicationContext interface must still be present");
        assert_eq!(app_context.methods.len(), 1, "{:?}", app_context.methods);
        assert_eq!(app_context.methods[0].name, "getCanvasUrl");

        // The empty group page itself is harmless noise, not an error --
        // this function only merges *orphans* (Unknown-kind pages that
        // actually have content), so a genuinely-empty Unknown page is
        // left exactly as it was.
        assert!(result.iter().any(|c| c.page_id == "apex_canvas_ApplicationContext_methods"));
    }

    /// An orphan whose parent chain never reaches an already-scraped,
    /// non-`Unknown` ancestor (e.g. a `--limit`-truncated run that never
    /// fetched the real parent page) must be left in the output, not
    /// silently dropped.
    #[test]
    fn leaves_an_unreattachable_orphan_in_place_rather_than_dropping_it() {
        let classes = vec![orphan("apex_some_orphan_method", "someMethod")];
        let mut parents = HashMap::new();
        parents.insert(
            "apex_some_orphan_method".to_string(),
            "apex_a_parent_never_scraped_this_run".to_string(),
        );

        let result = reattach_orphaned_members(classes, &parents);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].page_id, "apex_some_orphan_method");
        assert_eq!(result[0].methods.len(), 1);
    }

    #[test]
    fn is_group_page_title_recognizes_every_confirmed_suffix_and_rejects_real_names() {
        for title in [
            "ApplicationContext Methods",
            "Action Constructors",
            "Math Fields",
            "SingleMethod Method",
            "SingleCtor Constructor",
        ] {
            assert!(is_group_page_title(title), "{title:?} should be a group page");
        }
        for title in [
            "String Class",
            "ApplicationContext Interface",
            "ConnectApi.BatchResult",
            "Email Class (Base Email Methods)",
            "IntegrationTest Class (Developer Preview)",
            "",
        ] {
            assert!(!is_group_page_title(title), "{title:?} should NOT be a group page");
        }
    }

    /// The exact real-world bug this fix closes: `ConnectApi.BatchResult`
    /// (the genuine parent class) has no recognized `" Class"`/
    /// `" Interface"`/`" Enum"` suffix at all -- a dotted qualified name,
    /// so `split_title` reports it as `kind: "Unknown"`, identically to
    /// the pure-navigational `"BatchResult Methods"` group page one
    /// level below it. The old `kind != "Unknown"` reattachment check
    /// couldn't tell these two `Unknown` pages apart and gave up,
    /// leaving `getError()` orphaned. Checking the *title* instead
    /// correctly walks past the group page (`"...Methods"`) and stops at
    /// the real class (title doesn't end in a recognized group word),
    /// regardless of what `kind` says about either one.
    #[test]
    fn reattaches_through_a_real_ancestor_whose_own_kind_is_unrecognized() {
        let classes = vec![
            ClassModel {
                page_id: "apex_connectapi_output_batch_result".to_string(),
                name: "ConnectApi.BatchResult".to_string(),
                kind: "Unknown".to_string(),
                ..Default::default()
            },
            group_page(
                "apex_connectapi_output_batch_result_methods",
                "BatchResult Methods",
            ),
            orphan("apex_connectapi_output_batch_result_get_error", "getError"),
        ];
        let mut parents = HashMap::new();
        parents.insert(
            "apex_connectapi_output_batch_result_methods".to_string(),
            "apex_connectapi_output_batch_result".to_string(),
        );
        parents.insert(
            "apex_connectapi_output_batch_result_get_error".to_string(),
            "apex_connectapi_output_batch_result_methods".to_string(),
        );

        let result = reattach_orphaned_members(classes, &parents);

        let batch_result = result
            .iter()
            .find(|c| c.page_id == "apex_connectapi_output_batch_result")
            .expect("ConnectApi.BatchResult must still be present");
        assert_eq!(batch_result.methods.len(), 1, "{:?}", batch_result.methods);
        assert_eq!(batch_result.methods[0].name, "getError");
        assert!(
            !result.iter().any(|c| c.page_id == "apex_connectapi_output_batch_result_get_error"),
            "the orphan should have been merged away"
        );
    }
}
