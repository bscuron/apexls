//! Builds a page-id -> parent-page-id map from a doc-set's own recursive
//! `toc` tree (returned by the doc-set root's `get_document` response,
//! separately from `llms_index`'s flat page enumeration). Confirmed real
//! shape (see `apex_reference`'s own doc comment for the motivating
//! bug): a class/interface page can nest a "Methods"/"Constructors"
//! *group* page, which itself nests each individual member's own
//! dedicated page -- three levels deep, e.g.
//! `ApplicationContext Interface` -> `ApplicationContext Methods` ->
//! `getCanvasUrl()`. This map is what lets a member documented on its
//! own page (parsed as an orphaned, `kind: "Unknown"` pseudo-class) be
//! walked back up to the real class/interface/enum page it actually
//! belongs to.

use crate::http::Client;
use serde_json::Value;
use std::collections::HashMap;

/// `doc_set` is the full `atlas.en-us.<name>.meta` id (matching
/// `main.rs`'s `APEXREF_DOC_SET`/`OBJECT_REFERENCE_DOC_SET` constants).
pub fn fetch_parent_map(client: &Client, doc_set: &str) -> Result<HashMap<String, String>, String> {
    let url = format!("https://developer.salesforce.com/docs/get_document/{doc_set}");
    let body = client.get_text(&url)?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON for doc-set root: {e}"))?;
    let mut parents = HashMap::new();
    if let Some(toc) = parsed["toc"].as_array() {
        for root in toc {
            walk(root, None, &mut parents);
        }
    }
    Ok(parents)
}

fn walk(node: &Value, parent_id: Option<&str>, parents: &mut HashMap<String, String>) {
    let Some(href) = node["a_attr"]["href"].as_str() else {
        return;
    };
    let Some(page_id) = page_slug(href) else {
        return;
    };
    if let Some(parent_id) = parent_id {
        parents.insert(page_id.to_string(), parent_id.to_string());
    }
    if let Some(children) = node["children"].as_array() {
        for child in children {
            walk(child, Some(page_id), parents);
        }
    }
}

/// `"apex_pages_action.htm"` (optionally with a `#anchor`) -> `Some("apex_pages_action")`.
/// Identical logic to `llms_index::page_slug`, duplicated rather than
/// shared: that one operates on a full URL, this on a bare `href` --
/// keeping them separate avoids a shared helper that has to handle both
/// shapes awkwardly for two call sites that will likely diverge anyway
/// (this one may need to handle relative-vs-absolute hrefs differently
/// if a future doc set's `toc` turns out to format them differently).
fn page_slug(href: &str) -> Option<&str> {
    let htm_pos = href.find(".htm")?;
    let before = &href[..htm_pos];
    let slash_pos = before.rfind('/');
    Some(match slash_pos {
        Some(i) => &before[i + 1..],
        None => before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_a_three_level_real_toc_shape_into_a_flat_parent_map() {
        let toc: Value = serde_json::from_str(
            r#"[{
                "a_attr": {"href": "apex_interface_canvas_ApplicationContext.htm"},
                "children": [{
                    "a_attr": {"href": "apex_canvas_ApplicationContext_methods.htm"},
                    "children": [
                        {"a_attr": {"href": "apex_canvas_ApplicationContext_getCanvasUrl.htm"}, "children": []},
                        {"a_attr": {"href": "apex_canvas_ApplicationContext_getName.htm"}, "children": []}
                    ]
                }]
            }]"#,
        )
        .unwrap();

        let mut parents = HashMap::new();
        for root in toc.as_array().unwrap() {
            walk(root, None, &mut parents);
        }

        assert_eq!(
            parents.get("apex_canvas_ApplicationContext_methods").map(String::as_str),
            Some("apex_interface_canvas_ApplicationContext")
        );
        assert_eq!(
            parents.get("apex_canvas_ApplicationContext_getCanvasUrl").map(String::as_str),
            Some("apex_canvas_ApplicationContext_methods")
        );
        assert_eq!(
            parents.get("apex_canvas_ApplicationContext_getName").map(String::as_str),
            Some("apex_canvas_ApplicationContext_methods")
        );
        // The root itself has no parent.
        assert!(!parents.contains_key("apex_interface_canvas_ApplicationContext"));
    }
}
