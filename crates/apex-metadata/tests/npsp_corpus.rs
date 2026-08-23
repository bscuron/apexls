//! Verifies `discover_sobjects` against the real NPSP corpus (same
//! vendored submodule `apex-parser`/`apex-lexer` test against), matching
//! this project's verification-first convention of checking new code
//! against real-world data rather than only hand-written fixtures.

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

#[test]
fn discovers_known_custom_and_standard_objects_in_npsp() {
    let objects = apex_metadata::discover_sobjects(corpus_root());
    assert!(
        !objects.is_empty(),
        "expected the NPSP submodule to be checked out"
    );

    // `objects/Account_Soft_Credit__c/Account_Soft_Credit__c.object-meta.xml`
    // exists -- a declared custom object.
    let soft_credit = objects
        .iter()
        .find(|o| o.api_name == "Account_Soft_Credit__c")
        .expect("Account_Soft_Credit__c should be discovered");
    assert!(soft_credit.is_custom);

    // `objects/Account/` has no `Account.object-meta.xml` (Account is a
    // standard object) but does have custom fields, including the
    // `Batch__c` lookup used as the running example in this crate's docs.
    let account = objects
        .iter()
        .find(|o| o.api_name == "Account")
        .expect("Account should be discovered");
    assert!(
        !account.is_custom,
        "Account has no object-meta.xml of its own"
    );
    let batch_field = account
        .fields
        .iter()
        .find(|f| f.api_name == "Batch__c")
        .expect("Account.Batch__c should be discovered");
    assert_eq!(batch_field.field_type.as_deref(), Some("Lookup"));
    assert_eq!(batch_field.reference_to, vec!["Batch__c".to_string()]);

    // Sanity bounds -- NPSP has 51 raw `.object-meta.xml` files and 765
    // raw `.field-meta.xml` files across several package directories;
    // some objects/fields repeat across package dirs and get merged by
    // API name, so exact equality isn't expected, just the right order
    // of magnitude.
    let custom_object_count = objects.iter().filter(|o| o.is_custom).count();
    assert!(
        custom_object_count >= 40,
        "expected at least 40 custom objects, found {custom_object_count}"
    );
    let total_fields: usize = objects.iter().map(|o| o.fields.len()).sum();
    assert!(
        total_fields >= 600,
        "expected at least 600 fields total, found {total_fields}"
    );
}
