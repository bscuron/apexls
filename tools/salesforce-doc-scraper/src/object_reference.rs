//! Parses one Object Reference page's `content` HTML fragment (as
//! returned by `get_document_content/object_reference/<page>.htm/en-us/<version>`)
//! into an [`ObjectModel`]. Confirmed markup shape: one `<tr>` per field
//! (or per group of fields that share one description -- see below),
//! whose first `<td>` names the field and whose second holds a
//! `<dl class="detailList">` alternating `Type`/`Properties`/
//! `Description` `<dt>`/`<dd>` pairs.
//!
//! Deliberately does **not** key off the name `<td>`'s own `data-title`
//! attribute -- scanning a broad real sample turned up at least four
//! distinct values doc pages use for that same column across different
//! objects: `"Field Name"` (e.g. `Account`), `"Field"` (the more common
//! of those two, by roughly 3-to-1 -- `Contact`/`Opportunity`/most
//! others), `"Name"` (e.g. `ListView`/`ListViewChart`/`UserListView`),
//! and even `""` (empty -- e.g. `ProductRelatedComponent`, whose `<th>`
//! for that column has no text at all). A parser keyed to any specific
//! subset of these silently returns zero fields for every object using
//! a variant it doesn't list, which is exactly what happened here twice
//! before this fix (first only `"Field Name"`, then `"Field Name"`/
//! `"Field"`) -- each fix looked complete against its own sample only
//! because that sample didn't happen to include the next variant.
//! Instead, this matches the one marker every variant shares: the field
//! name itself is always wrapped in `<span class="keyword parmname">`,
//! and the row is always identifiable by containing a
//! `<dl class="detailList">` -- both structural, not tied to whatever
//! label DITA happened to render for that particular table's header.
//!
//! Also handles a shape a data-title-keyed parser silently mishandled:
//! some rows document *several* fields against one shared
//! Type/Properties/Description (e.g. `Account`'s `PersonMailingCity`/
//! `PersonMailingCountry`/`PersonMailingPostalCode`/`PersonMailingState`,
//! each a `<li>` inside the name cell) -- picking only the first
//! `<span class="keyword parmname">` per row would silently drop the
//! other three. This collects every such span found in the name cell
//! specifically (not the whole row -- the Details cell's own
//! Description prose often *references* other fields by name in the
//! same span class, e.g. "Used with `BillingLongitude` to specify...",
//! which would be wrongly swept in by a whole-row search) and emits one
//! [`FieldModel`] per name, all sharing that row's one details block.
//!
//! A lookup/master-detail field additionally has `Relationship Name`/
//! `Relationship Type`/`Refers To` `<dt>`/`<dd>` pairs after `Type`/
//! `Properties`/`Description`; `Refers To` (captured into
//! `FieldModel::reference_to`) is comma-separated for a polymorphic
//! field (e.g. `Task.OwnerId` -> `"Group, User"`). Pairing each `<dt>`
//! with *every* `<dd>` up to the next `<dt>` (not a positional 1:1 zip)
//! is what makes this reliable: a multi-paragraph `Description` really
//! does have more than one `<dd>` for real (confirmed:
//! `Task.AccountId`'s page has 6 `<dt>`s but 7 `<dd>`s), and a
//! positional zip would silently shift every label after it.

use crate::model::{FieldModel, ObjectModel};
use scraper::{ElementRef, Html, Selector};

pub fn parse_object_page(page_id: &str, title: &str, content_html: &str) -> ObjectModel {
    let document = Html::parse_fragment(content_html);
    let row_sel = selector("tr");
    let details_sel = selector("dl.detailList");
    let td_sel = selector("td");
    let name_sel = selector("span.keyword.parmname");

    let fields = document
        .select(&row_sel)
        .filter(|row| row.select(&details_sel).next().is_some())
        .flat_map(|row| parse_field_row(row, &td_sel, &name_sel, &details_sel))
        .collect();

    ObjectModel {
        page_id: page_id.to_string(),
        name: title.to_string(),
        fields,
    }
}

fn parse_field_row(
    row: ElementRef,
    td_sel: &Selector,
    name_sel: &Selector,
    details_sel: &Selector,
) -> Vec<FieldModel> {
    let Some(name_cell) = row.select(td_sel).next() else {
        return Vec::new();
    };
    let names: Vec<String> = name_cell
        .select(name_sel)
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Vec::new();
    }

    let Some(details) = row.select(details_sel).next() else {
        return names
            .into_iter()
            .map(|name| FieldModel { name, ..Default::default() })
            .collect();
    };

    // A `<dt>` can be followed by *more than one* `<dd>` -- confirmed
    // real, not hypothetical: a multi-paragraph `Description` routinely
    // has a second, unlabeled `<dd>` (e.g. `Task.AccountId`'s real page
    // has 6 `<dt>`s but 7 `<dd>`s, the extra one being a bare "This is a
    // relationship field." sentence tacked onto `Description`). Pairing
    // `dt`s and `dd`s positionally (`zip`) silently misaligns every
    // label *after* the first multi-`<dd>` one -- `Relationship Name`
    // would wrongly get that stray sentence's text, cascading through
    // `Relationship Type` and `Refers To` and dropping the real, final
    // `Refers To` value off the end entirely. Selecting `dt, dd` in one
    // pass preserves true document order, so each `<dt>` can correctly
    // claim every `<dd>` up to the next `<dt>` instead.
    let dt_dd_sel = selector("dt, dd");
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for el in details.select(&dt_dd_sel) {
        let text = el.text().collect::<String>().trim().to_string();
        if el.value().name() == "dt" {
            sections.push((text, Vec::new()));
        } else if let Some((_, values)) = sections.last_mut() {
            values.push(text);
        }
    }

    let mut field_type = None;
    let mut properties = Vec::new();
    let mut description = None;
    let mut reference_to = Vec::new();
    for (label, values) in &sections {
        let joined = values.join(" ");
        match label.as_str() {
            "Type" => field_type = Some(joined),
            "Properties" => {
                properties = joined
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }
            "Description" => description = Some(joined),
            "Refers To" => {
                reference_to = joined
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }
            _ => {}
        }
    }

    names
        .into_iter()
        .map(|name| FieldModel {
            name,
            field_type: field_type.clone(),
            properties: properties.clone(),
            description: description.clone(),
            reference_to: reference_to.clone(),
        })
        .collect()
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).unwrap_or_else(|e| panic!("invalid selector {css:?}: {e:?}"))
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
    fn parses_the_real_account_object_page_fixture() {
        let fixture = load_fixture("sforce_api_objects_account.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object = parse_object_page("sforce_api_objects_account", title, content);

        assert_eq!(object.name, "Account");
        assert!(
            object.fields.len() > 20,
            "expected Account to have more than 20 documented fields, got {}",
            object.fields.len()
        );

        let billing_city = object
            .fields
            .iter()
            .find(|f| f.name == "BillingCity")
            .expect("BillingCity should have been parsed");
        assert_eq!(billing_city.field_type.as_deref(), Some("string"));
        assert!(billing_city.properties.contains(&"Create".to_string()));
        assert!(billing_city.properties.contains(&"Nillable".to_string()));
        assert!(billing_city.description.is_some());
    }

    /// `Contact` uses `data-title="Field"` for its field-name column,
    /// not `"Field Name"` like `Account` does -- the more common of the
    /// two variants across the whole doc set, by roughly 3-to-1.
    /// Regression test for a real bug: matching `"Field Name"` only
    /// silently returned zero fields for `Contact`/`Opportunity`/most
    /// other objects, discovered by noticing `Contact` came out with an
    /// empty `fields` list in a full scrape despite `Account` (the one
    /// fixture the original test used) working fine.
    #[test]
    fn parses_the_real_contact_object_page_using_the_field_data_title_variant() {
        let fixture = load_fixture("sforce_api_objects_contact.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object = parse_object_page("sforce_api_objects_contact", title, content);

        assert_eq!(object.name, "Contact");
        assert!(
            object.fields.len() > 20,
            "expected Contact to have more than 20 documented fields, got {}",
            object.fields.len()
        );

        let account_id = object
            .fields
            .iter()
            .find(|f| f.name == "AccountId")
            .expect("AccountId should have been parsed");
        assert_eq!(account_id.field_type.as_deref(), Some("reference"));
        assert!(account_id.properties.contains(&"Nillable".to_string()));
        assert!(account_id.description.is_some());
        assert_eq!(account_id.reference_to, vec!["Account".to_string()]);
    }

    /// `ListView` uses `data-title="Name"` for its field-name column --
    /// a third variant distinct from both `"Field Name"` and `"Field"`.
    /// Regression test for a real bug found by auditing every object
    /// with zero parsed fields in a full scrape rather than trusting
    /// that the two known variants were exhaustive: `ListView`,
    /// `ListViewChart`, `UserListView`, and `UserListViewCriterion` all
    /// silently returned zero fields under the old data-title-keyed
    /// parser despite having real, populated field tables.
    #[test]
    fn parses_the_real_listview_object_page_using_the_name_data_title_variant() {
        let fixture = load_fixture("sforce_api_objects_listview.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object = parse_object_page("sforce_api_objects_listview", title, content);

        assert_eq!(object.name, "ListView");
        assert!(
            !object.fields.is_empty(),
            "expected ListView to have documented fields, got none"
        );

        let developer_name = object
            .fields
            .iter()
            .find(|f| f.name == "DeveloperName")
            .expect("DeveloperName should have been parsed");
        assert_eq!(developer_name.field_type.as_deref(), Some("string"));
        assert!(developer_name.properties.contains(&"Sort".to_string()));
        assert!(developer_name.description.is_some());
    }

    /// `ProductRelatedComponent` uses an *empty* `data-title=""` for its
    /// field-name column -- its `<th>` header cell has no text at all.
    /// A fourth variant, found the same way as `ListView`'s: this is
    /// exactly why field extraction keys off the structural
    /// `<span class="keyword parmname">` marker instead of enumerating
    /// `data-title` strings -- there's no reason to expect that
    /// enumeration is ever complete.
    #[test]
    fn parses_the_real_productrelatedcomponent_page_using_the_blank_data_title_variant() {
        let fixture = load_fixture("sforce_api_objects_productrelatedcomponent.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object =
            parse_object_page("sforce_api_objects_productrelatedcomponent", title, content);

        assert_eq!(object.name, "ProductRelatedComponent");
        let child_product_id = object
            .fields
            .iter()
            .find(|f| f.name == "ChildProductId")
            .expect("ChildProductId should have been parsed");
        assert_eq!(child_product_id.field_type.as_deref(), Some("reference"));
        assert!(child_product_id.properties.contains(&"Create".to_string()));
        assert_eq!(child_product_id.reference_to, vec!["Product2".to_string()]);
    }

    /// Real, confirmed bug this fix closes: a `<dt>` can be followed by
    /// *more than one* `<dd>` (a multi-paragraph `Description`, e.g. a
    /// bare "This is a relationship field." sentence tacked on after the
    /// real description text) -- `Task.AccountId`'s real page has 6
    /// `<dt>`s but 7 `<dd>`s. Pairing them positionally (the original
    /// implementation) silently shifts every label after the first
    /// multi-`<dd>` one: `Relationship Name` would wrongly get that
    /// stray sentence, cascading through `Relationship Type` and
    /// `Refers To`, dropping the real `Refers To` value off the end
    /// entirely. `Task.OwnerId` additionally confirms a *polymorphic*
    /// lookup's `Refers To` is comma-separated (`"Group, User"`), not a
    /// single value.
    #[test]
    fn pairs_multi_paragraph_descriptions_correctly_and_splits_polymorphic_refers_to() {
        let fixture = load_fixture("sforce_api_objects_task.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object = parse_object_page("sforce_api_objects_task", title, content);

        let account_id = object
            .fields
            .iter()
            .find(|f| f.name == "AccountId")
            .expect("AccountId should have been parsed");
        assert_eq!(account_id.reference_to, vec!["Account".to_string()]);
        assert!(
            account_id.description.as_deref().unwrap_or_default().contains("relationship field"),
            "the second, unlabeled Description <dd> should still be captured, got {:?}",
            account_id.description
        );

        let owner_id = object
            .fields
            .iter()
            .find(|f| f.name == "OwnerId")
            .expect("OwnerId should have been parsed");
        assert_eq!(
            owner_id.reference_to,
            vec!["Group".to_string(), "User".to_string()]
        );
    }

    /// Some rows document several fields against one shared
    /// Type/Properties/Description -- `Account`'s own
    /// `PersonMailingCity`/`PersonMailingCountry`/`PersonMailingPostalCode`/
    /// `PersonMailingState` row is a real, checked-in example. Picking
    /// only the first name per row (as an earlier version of this parser
    /// did) silently drops the other three; this asserts all four exist
    /// as their own `FieldModel`s, sharing the row's one description.
    #[test]
    fn splits_a_shared_row_into_one_field_per_name() {
        let fixture = load_fixture("sforce_api_objects_account.json");
        let title = fixture["title"].as_str().unwrap();
        let content = fixture["content"].as_str().unwrap();
        let object = parse_object_page("sforce_api_objects_account", title, content);

        let grouped = [
            "PersonMailingCity",
            "PersonMailingCountry",
            "PersonMailingPostalCode",
            "PersonMailingState",
        ];
        for name in grouped {
            let field = object
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} should have been parsed as its own field"));
            assert_eq!(field.field_type.as_deref(), Some("string"));
            assert!(
                field.description.as_deref().unwrap_or_default().contains("mailing address"),
                "{name} should share the group's own description, got {:?}",
                field.description
            );
        }
    }
}
