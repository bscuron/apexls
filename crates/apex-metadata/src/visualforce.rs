//! Extracts which Apex classes a Visualforce page names as its
//! `controller`/`extensions` -- the one thing `apex_binder::dead_code`
//! needs to know a class is reachable from VF markup with no annotation
//! required at all (unlike Flow/Aura/LWC/REST, all annotation-gated).
//!
//! Deliberately **does not parse a whole `.page` file as XML**. Real VF
//! pages routinely aren't well-formed XML: embedded HTML fragments,
//! unescaped `&`/`<` inside `<script>` blocks, Apex expression-language
//! interpolations (`{!expr}`) anywhere in attribute values or text. A
//! strict parser (`roxmltree`, already a dependency for `.object-meta.xml`/
//! `.field-meta.xml` -- see `crate::xml`) would reject a real page file
//! far more often than not. Instead: find just the root `<apex:page ...>`
//! opening tag as a raw substring, isolate it into its own small,
//! reliably well-formed fragment, and parse *only that* with
//! `roxmltree` -- gets correct attribute-value unescaping for free
//! (`&amp;` etc.) instead of hand-rolled attribute parsing, without
//! needing the rest of the file to be valid XML at all.

use std::collections::HashSet;
use std::path::PathBuf;

/// Every class name (lowercased, since Apex class names are
/// case-insensitive and this is checked against symbol names elsewhere)
/// any of `page_files` names as its `controller` or `extensions`.
/// Unreadable files and pages with no recognizable `<apex:page>` tag are
/// silently skipped, matching `apex_discover`'s own error-tolerance --
/// a page this can't confidently parse contributes no exemptions rather
/// than failing the whole sweep.
pub fn referenced_controller_classes(page_files: &[PathBuf]) -> HashSet<String> {
    let mut classes = HashSet::new();
    for path in page_files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for name in controller_and_extension_classes(&content) {
            classes.insert(name.to_ascii_lowercase());
        }
    }
    classes
}

/// The `controller`/`extensions` (comma-separated) attribute values of
/// one page's root `<apex:page>` tag, as declared (not yet lowercased --
/// callers needing the normalized form should lowercase after collecting
/// from every attribute, matching `referenced_controller_classes`).
fn controller_and_extension_classes(content: &str) -> Vec<String> {
    let Some(fragment) = isolate_apex_page_tag(content) else {
        return Vec::new();
    };
    let Ok(doc) = roxmltree::Document::parse(&fragment) else {
        return Vec::new();
    };
    let root = doc.root_element();
    let mut names = Vec::new();
    if let Some(controller) = root.attribute("controller") {
        names.push(controller.trim().to_string());
    }
    if let Some(extensions) = root.attribute("extensions") {
        names.extend(extensions.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    }
    names
}

/// Finds the first case-insensitive `<apex:page` in `content` and
/// isolates it through its own closing `>` -- tracking quoted-attribute
/// state so a `>` inside a quoted value (unusual for `controller`/
/// `extensions` specifically, but not impossible for other attributes on
/// the same tag) doesn't end the scan early -- then self-closes it
/// (`... />`) if it wasn't already, so the result is a small, complete,
/// independently well-formed fragment regardless of whatever the rest of
/// the file contains. Also injects an `xmlns:apex="..."` declaration
/// right after the tag name: a real `.page` file never declares one
/// itself (Salesforce's own tooling resolves the `apex:` prefix
/// implicitly), but a real XML parser -- `roxmltree` included -- rejects
/// any element with an unbound namespace prefix outright. The URI itself
/// is a throwaway placeholder; nothing here cares about namespace
/// identity, only about `roxmltree` accepting the fragment at all and
/// `Node::attribute` finding `controller`/`extensions` by local name
/// (unaffected by which namespace the element itself resolves to).
/// Assumes the conventional, near-universal lowercase `apex:page` casing
/// -- XML prefix binding is case-sensitive, so an unusually-cased
/// `<APEX:PAGE>` wouldn't bind against this fixed-case injection, an
/// honest, low-risk limitation given how uniformly Salesforce's own
/// tooling generates this tag.
fn isolate_apex_page_tag(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let start = lower.find("<apex:page")?;
    let bytes = content.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => break,
                _ => {}
            },
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None; // unterminated tag -- not a fragment we can trust
    }
    let tag = &content[start..=i];
    let body = tag.strip_prefix("<apex:page")?;
    let body = body.strip_suffix("/>").or_else(|| body.strip_suffix('>')).unwrap_or(body);
    Some(format!(r#"<apex:page xmlns:apex="urn:apexls-visualforce"{body}/>"#))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_simple_controller() {
        let page = r#"<apex:page controller="MyController">Hello</apex:page>"#;
        assert_eq!(controller_and_extension_classes(page), vec!["MyController"]);
    }

    #[test]
    fn extracts_comma_separated_extensions() {
        let page = r#"<apex:page extensions="A, B,C">Hello</apex:page>"#;
        assert_eq!(controller_and_extension_classes(page), vec!["A", "B", "C"]);
    }

    #[test]
    fn extracts_both_controller_and_extensions() {
        let page = r#"<apex:page controller="Base" extensions="Ext1,Ext2">Hello</apex:page>"#;
        assert_eq!(controller_and_extension_classes(page), vec!["Base", "Ext1", "Ext2"]);
    }

    #[test]
    fn attributes_in_a_different_order_and_spread_across_lines_still_extract() {
        let page = "<apex:page\n    extensions=\"Ext1\"\n    controller=\"Base\"\n    showHeader=\"false\">\n</apex:page>";
        let mut names = controller_and_extension_classes(page);
        names.sort();
        assert_eq!(names, vec!["Base", "Ext1"]);
    }

    #[test]
    fn tolerates_a_malformed_body_elsewhere_in_the_file() {
        // Unescaped `&`/stray `<` inside a <script> block, well after the
        // root tag -- a real XML parser run over the *whole* file would
        // reject this; isolating just the root tag must not care.
        let page = r#"<apex:page controller="MyController">
    <script>if (a < b && c) { doThing(); }</script>
</apex:page>"#;
        assert_eq!(controller_and_extension_classes(page), vec!["MyController"]);
    }

    #[test]
    fn a_page_with_neither_attribute_extracts_nothing() {
        let page = r#"<apex:page>Hello</apex:page>"#;
        assert!(controller_and_extension_classes(page).is_empty());
    }

    #[test]
    fn no_apex_page_tag_at_all_extracts_nothing() {
        assert!(controller_and_extension_classes("<html><body>not a VF page</body></html>").is_empty());
    }

    #[test]
    fn referenced_controller_classes_lowercases_and_unions_across_files() {
        let dir = std::env::temp_dir().join(format!(
            "apex-metadata-visualforce-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.page"), r#"<apex:page controller="FooBar">x</apex:page>"#).unwrap();
        std::fs::write(dir.join("B.page"), r#"<apex:page extensions="Baz,FOOBAR">x</apex:page>"#).unwrap();
        let files = vec![dir.join("A.page"), dir.join("B.page")];
        let classes = referenced_controller_classes(&files);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(classes, HashSet::from(["foobar".to_string(), "baz".to_string()]));
    }
}
