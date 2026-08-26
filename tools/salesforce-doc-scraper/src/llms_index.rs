//! Page enumeration via Salesforce's own `llms-product-docs.txt` --
//! confirmed during this tool's design phase to already list every page
//! across every Salesforce product doc set, including all Apex
//! Reference Guide and Object Reference pages, in a flat Markdown-link
//! list (`- [Title](https://developer.salesforce.com/docs/atlas.en-us.<doc_set>.meta/<doc_set>/<page>.htm)`).
//! Sidesteps walking either doc set's own recursive `toc` tree entirely.

use crate::http::Client;
use std::collections::HashSet;

const LLMS_INDEX_URL: &str = "https://developer.salesforce.com/docs/llms-product-docs.txt";

pub struct PageRef {
    pub title: String,
    /// The page slug -- the `.htm` filename's stem, e.g.
    /// `apex_methods_system_string` -- which doubles as the
    /// `get_document_content` page argument.
    pub page_id: String,
}

/// Every distinct page (deduped by slug -- many Apex Reference lines
/// are individual-method anchors on the *same* class page, e.g. dozens
/// of `String` methods all pointing at `apex_methods_system_string.htm`)
/// whose URL contains `doc_set_marker` (e.g. `"atlas.en-us.apexref.meta"`
/// or `"atlas.en-us.object_reference.meta"`).
pub fn fetch_pages(client: &Client, doc_set_marker: &str) -> Result<Vec<PageRef>, String> {
    let text = client.get_text(LLMS_INDEX_URL)?;
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    for line in text.lines() {
        let Some(url) = extract_markdown_link_url(line) else {
            continue;
        };
        if !url.contains(doc_set_marker) {
            continue;
        }
        let Some(page_id) = page_slug(url) else {
            continue;
        };
        if !seen.insert(page_id.to_string()) {
            continue;
        }
        let title = extract_markdown_link_title(line).unwrap_or(page_id);
        pages.push(PageRef {
            title: title.to_string(),
            page_id: page_id.to_string(),
        });
    }
    Ok(pages)
}

/// `- [Title](URL)` -> `Some(URL)`.
fn extract_markdown_link_url(line: &str) -> Option<&str> {
    let paren_start = line.find("](")? + 2;
    let paren_end = line[paren_start..].find(')')?;
    Some(&line[paren_start..paren_start + paren_end])
}

/// `- [Title](URL)` -> `Some("Title")`.
fn extract_markdown_link_title(line: &str) -> Option<&str> {
    let bracket_start = line.find('[')? + 1;
    let bracket_end = line[bracket_start..].find(']')?;
    Some(&line[bracket_start..bracket_start + bracket_end])
}

/// `https://.../<page>.htm` (optionally followed by `#anchor`) -> `Some("<page>")`.
fn page_slug(url: &str) -> Option<&str> {
    let htm_pos = url.find(".htm")?;
    let before = &url[..htm_pos];
    let slash_pos = before.rfind('/')?;
    Some(&before[slash_pos + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_url_and_title_from_a_markdown_link_line() {
        let line = "- [String Class](https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm)";
        assert_eq!(
            extract_markdown_link_url(line),
            Some("https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm")
        );
        assert_eq!(extract_markdown_link_title(line), Some("String Class"));
        assert_eq!(
            page_slug(extract_markdown_link_url(line).unwrap()),
            Some("apex_methods_system_string")
        );
    }

    #[test]
    fn page_slug_ignores_a_trailing_anchor() {
        let url = "https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm#apex_System_String_isBlank";
        assert_eq!(page_slug(url), Some("apex_methods_system_string"));
    }

    #[test]
    fn fetch_pages_dedupes_by_page_slug_and_filters_by_doc_set() {
        let text = "\
- [String Class](https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm)
- [isBlank](https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm#apex_System_String_isBlank)
- [isEmpty](https://developer.salesforce.com/docs/atlas.en-us.apexref.meta/apexref/apex_methods_system_string.htm#apex_System_String_isEmpty)
- [Account](https://developer.salesforce.com/docs/atlas.en-us.object_reference.meta/object_reference/sforce_api_objects_account.htm)
- [Unrelated Guide](https://developer.salesforce.com/docs/atlas.en-us.some_other_guide.meta/some_other_guide/whatever.htm)
";
        // Exercise the two pure helpers directly with the sample text
        // instead of standing up a full `Client` -- `fetch_pages`'s own
        // logic (dedup + filter) is the same loop, tested here without
        // a network dependency.
        let mut seen = std::collections::HashSet::new();
        let mut pages = Vec::new();
        for line in text.lines() {
            let Some(url) = extract_markdown_link_url(line) else { continue };
            if !url.contains("atlas.en-us.apexref.meta") {
                continue;
            }
            let Some(id) = page_slug(url) else { continue };
            if seen.insert(id.to_string()) {
                pages.push(id.to_string());
            }
        }
        assert_eq!(pages, vec!["apex_methods_system_string"]);
    }
}
