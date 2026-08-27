//! Regression tests for Apex's SObject constructor field-init sugar
//! (`new Contact(LastName = 'foo', Primary_Affiliation__c = acc.id)`).
//! Before this, each `field = value` pair's LHS parsed as an ordinary
//! `Expr::Bin` (the grammar has no special case for it -- `arg_list` is
//! shared verbatim by `CallExpr`/`MethodCallExpr`/`NewExpr`), so the LHS
//! fell through `bind_name_expr`'s ordinary local/member/type lookups
//! straight to `Resolution::Unresolved`, and goto-definition on it did
//! nothing. `crate::resolve::BodyBinder::bind_new_expr` now recognizes
//! this shape (only for a real schema-object constructor target) and
//! resolves the LHS against `SchemaIndex` the same way `bind_field_expr`
//! already does for `object.field` access.

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef, UnknownSchemaRef};
use apex_syntax::ast::expr::NameExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        let path = dir.join(file_name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    dir
}

/// The `Resolution` recorded for the bare `NameExpr` spelled `name` --
/// there's only ever one file and one matching identifier in these
/// fixtures, so this doesn't need to disambiguate further.
fn name_expr_resolution(program: &BoundProgram, name: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(n) = NameExpr::cast(node) else { continue };
            let Some(tok) = n.name_token() else { continue };
            if tok.text() == name {
                let ptr = apex_binder::SyntaxPtr::new(file, n.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

#[test]
fn a_standard_field_set_via_sobject_constructor_sugar_resolves() {
    let dir = write_fixture_dir(
        "sobject-ctor-standard-field",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Contact con = new Contact(LastName = 'foo'); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        name_expr_resolution(&program, "LastName"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Contact".into(),
            field: Some("LastName".into()),
        }))),
        "new Contact(LastName = 'foo')'s LastName should resolve against the bundled standard schema"
    );
}

/// The exact shape from the original bug report: a *custom* field
/// (locally declared on the standard `Contact` object, the real NPSP
/// shape) set via the constructor sugar, alongside a standard field in
/// the same call.
#[test]
fn a_custom_field_set_via_sobject_constructor_sugar_resolves() {
    let dir = write_fixture_dir(
        "sobject-ctor-custom-field",
        &[
            (
                "Foo.cls",
                "public class Foo { \
                     public void run(Account acc) { \
                         Contact con = new Contact( \
                             LastName = 'foo', \
                             Primary_Affiliation__c = acc.Id \
                         ); \
                     } \
                 }",
            ),
            (
                "objects/Contact/fields/Primary_Affiliation__c.field-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Primary_Affiliation__c</fullName><type>Lookup</type><referenceTo>Account</referenceTo></CustomField>",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        name_expr_resolution(&program, "Primary_Affiliation__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Contact".into(),
            field: Some("Primary_Affiliation__c".into()),
        }))),
        "new Contact(...)'s Primary_Affiliation__c should resolve against the locally-declared \
         custom field, letting goto-definition reach its .field-meta.xml"
    );
    assert_eq!(
        name_expr_resolution(&program, "LastName"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Contact".into(),
            field: Some("LastName".into()),
        }))),
        "a standard field in the same constructor call should still resolve too"
    );
}

/// A genuine typo (or a field this project's metadata doesn't know
/// about) must stay honestly `UnknownSchema`, not silently guessed as
/// `Resolved`/`Unresolved` -- the same distinction every other schema
/// reference already makes.
#[test]
fn an_unknown_field_set_via_sobject_constructor_sugar_stays_unknown_schema() {
    let dir = write_fixture_dir(
        "sobject-ctor-unknown-field",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Contact con = new Contact(TotallyMadeUpField__c = 'x'); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        name_expr_resolution(&program, "TotallyMadeUpField__c"),
        Some(Resolution::UnknownSchema(Box::new(UnknownSchemaRef {
            object: Some("Contact".into()),
            field: Some("TotallyMadeUpField__c".into()),
        }))),
    );
}

/// A project-local class's constructor call never gets this treatment --
/// `new Foo(x = 5)` (nonsensical Apex, but the generic grammar parses it
/// the same way) must not be mistaken for SObject field-init sugar just
/// because it has the same `Expr::Bin`-with-`=`-and-a-bare-name shape.
#[test]
fn a_project_local_constructor_call_is_not_treated_as_field_init_sugar() {
    let dir = write_fixture_dir(
        "sobject-ctor-project-local",
        &[(
            "Foo.cls",
            "public class Foo { \
                 public Foo(Integer x) {} \
                 public void run() { \
                     Integer x = 5; \
                     Foo f = new Foo(x = 5); \
                 } \
             }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // `x` is a real local variable here -- it should resolve as an
    // ordinary local reference (via the generic `Expr::Bin` path), not
    // as a schema field lookup, since `Foo` is project-local.
    assert!(matches!(name_expr_resolution(&program, "x"), Some(Resolution::Resolved(_))));
}

/// The exact shape of a real bug found (via a real-NPSP-corpus diagnostic)
/// while fixing the original report: before this fix, a field-init name
/// that happened to also match an in-scope local/parameter (a very real
/// collision -- `CloseDate`, `AccountId`, `Name`, `Id`, ... are common
/// both as SObject fields and as ordinary variable names) silently
/// resolved to that unrelated local instead of the SObject field, since
/// the LHS was bound through the exact same `bind_name_expr` path as any
/// other identifier *read*, and a local always wins there. Goto-definition
/// on `CloseDate` in `new Opportunity(CloseDate = ...)` would jump to some
/// unrelated local variable's declaration, not `Opportunity.CloseDate` --
/// wrong, not just unhelpful. Confirmed via `resolution_regression_baseline.rs`'s
/// own real-corpus count (81 such coincidental matches found, 80 of which
/// were this exact bug).
#[test]
fn a_field_name_matching_an_in_scope_local_still_resolves_as_the_schema_field() {
    let dir = write_fixture_dir(
        "sobject-ctor-shadowed-by-local",
        &[(
            "Foo.cls",
            "public class Foo { \
                 public void run() { \
                     Date CloseDate = Date.today(); \
                     Opportunity opp = new Opportunity(CloseDate = CloseDate); \
                 } \
             }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let mut resolutions: Vec<Resolution> = Vec::new();
    for file in program.files() {
        let root = program.syntax(file);
        for n in root.descendants().filter_map(NameExpr::cast) {
            if !n.name_token().is_some_and(|t| t.text() == "CloseDate") {
                continue;
            }
            let ptr = apex_binder::SyntaxPtr::new(file, n.syntax());
            if let Some(r) = program.resolution(ptr) {
                resolutions.push(r.clone());
            }
        }
    }

    // Two `CloseDate` identifiers exist: the field-init LHS (must resolve
    // to the schema field, never the local) and the RHS (a genuine read of
    // the local variable, which should still resolve as `Resolved` --
    // this fix must not accidentally suppress that).
    assert!(
        resolutions.contains(&Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Opportunity".into(),
            field: Some("CloseDate".into()),
        }))),
        "the field-init LHS must resolve as Opportunity.CloseDate, not the shadowing local: {resolutions:?}"
    );
    assert!(
        resolutions.iter().any(|r| matches!(r, Resolution::Resolved(_))),
        "the RHS `CloseDate` local-variable read should still resolve normally: {resolutions:?}"
    );
}
