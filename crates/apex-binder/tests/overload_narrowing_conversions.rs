//! `crate::conversions`'s curated Apex implicit-conversion rules, exercised
//! through real overload calls (`crate::resolve::narrow_by_overload`) --
//! complements `conversions.rs`'s own unit tests (which check the
//! compatibility/specificity functions in isolation) by proving they're
//! actually wired into real call-site resolution, and that the "never
//! eliminate outside the curated set" safety property still holds.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

/// The `pick` overload whose sole parameter's declared type is exactly
/// `param_type_name` (e.g. `"Integer"`, `"Long"`) -- picks it out
/// unambiguously among several same-name overloads without depending on
/// declaration order.
fn pick_overload_with_param_type(program: &BoundProgram, param_type_name: &str) -> apex_binder::SymbolId {
    program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "pick")
        .find(|(id, _)| {
            program
                .symbols
                .get(program.symbols.params(*id)[0])
                .type_name
                .as_deref()
                == Some(param_type_name)
        })
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("pick({param_type_name}) should exist"))
}

/// The `pick` overload whose sole parameter is declared as
/// `collection_name<element_type_name>` (e.g. `"List"`, `"Integer"`).
fn pick_overload_with_collection_param(
    program: &BoundProgram,
    collection_name: &str,
    element_type_name: &str,
) -> apex_binder::SymbolId {
    program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "pick")
        .find(|(id, _)| {
            let p = program.symbols.get(program.symbols.params(*id)[0]);
            p.type_name.as_deref() == Some(collection_name)
                && p.type_args.first().map(|s| s.as_str()) == Some(element_type_name)
        })
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("pick({collection_name}<{element_type_name}>) should exist"))
}

fn file_for(program: &BoundProgram, kind: SymbolKind, name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == kind && s.name == name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{name} should have been collected"))
}

/// The single `CallExpr`/`MethodCallExpr` whose callee text is `name`, in
/// `file` -- asserts there's exactly one so a test's intent is
/// unambiguous.
fn the_call_resolution(
    program: &BoundProgram,
    file: apex_binder::FileId,
    name: &str,
) -> Option<Resolution> {
    use apex_syntax::ast::expr::CallExpr;
    use rowan::ast::AstNode;
    let root = program.syntax(file);
    let matches: Vec<_> = root
        .descendants()
        .filter_map(CallExpr::cast)
        .filter(|c| c.callee_token().is_some_and(|t| t.text() == name))
        .collect();
    assert_eq!(matches.len(), 1, "expected exactly one call to `{name}`");
    program
        .resolution(SyntaxPtr::new(file, matches[0].syntax()))
        .cloned()
}

#[test]
fn numeric_literal_resolves_the_exact_overload_over_a_wider_one() {
    let dir = write_fixture_dir(
        "numeric-exact",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Integer x) { } \
             public void pick(Long x) { } \
             public void pick(Decimal x) { } \
             public void run() { pick(1); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let int_overload = pick_overload_with_param_type(&program, "Integer");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(int_overload))
    );
}

#[test]
fn numeric_literal_widens_to_the_nearest_overload_when_no_exact_match_exists() {
    let dir = write_fixture_dir(
        "numeric-widen",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Long x) { } \
             public void pick(Decimal x) { } \
             public void run() { pick(1); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let long_overload = pick_overload_with_param_type(&program, "Long");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(long_overload))
    );
}

#[test]
fn a_string_argument_prefers_the_exact_overload_over_object() {
    let dir = write_fixture_dir(
        "object-vs-string",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(String x) { } \
             public void pick(Object x) { } \
             public void run() { pick('hi'); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let string_overload = pick_overload_with_param_type(&program, "String");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(string_overload))
    );
}

#[test]
fn a_list_of_integer_argument_prefers_the_exact_element_type_over_a_widened_one() {
    // Verified against a real connected org before encoding this rule:
    // Apex's collection generics aren't invariant the way Java's are --
    // `List<Integer>` also satisfies a `List<Long>`-only overload, so
    // *both* declared overloads below are individually applicable; the
    // most-specific tiebreak must still prefer the exact one.
    let dir = write_fixture_dir(
        "list-numeric-widen",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(List<Integer> x) { } \
             public void pick(List<Long> x) { } \
             public void run() { List<Integer> xs = new List<Integer>(); pick(xs); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let list_integer_overload = pick_overload_with_collection_param(&program, "List", "Integer");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(list_integer_overload))
    );
}

#[test]
fn a_list_of_a_project_local_subtype_eliminates_a_mismatched_element_leaf_type() {
    // A `List<Dog>` (`Dog extends Animal`) must still resolve
    // `pick(List<Animal>)` over a `pick(List<String>)` sibling overload --
    // the project-local element type's own `extends` upcast (already
    // exact before this work) combines with the new system-vs-project
    // elimination (a `Dog` element can never satisfy a `List<String>`
    // parameter) to leave exactly one candidate.
    let dir = write_fixture_dir(
        "list-project-element",
        &[
            ("Animal.cls", "public virtual class Animal { }"),
            ("Dog.cls", "public class Dog extends Animal { }"),
            (
                "Toolbox.cls",
                "public class Toolbox { \
                 public void pick(List<Animal> x) { } \
                 public void pick(List<String> x) { } \
                 public void run() { List<Dog> dogs = new List<Dog>(); pick(dogs); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let list_animal_overload = pick_overload_with_collection_param(&program, "List", "Animal");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(list_animal_overload))
    );
}

#[test]
fn argument_and_parameter_types_outside_the_curated_set_stay_ambiguous() {
    // `Exception`/`PageReference` are real Apex system types, but neither
    // is in `crate::conversions`'s curated set -- an `Exception`-typed
    // argument must never eliminate a `PageReference` overload (or vice
    // versa), even though a real compiler would reject this call. "Can't
    // prove wrong" must keep winning outside the curated rules, exactly
    // as it did before this work for every system type.
    let dir = write_fixture_dir(
        "uncurated-stays-ambiguous",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Exception x) { } \
             public void pick(PageReference x) { } \
             public void run() { Exception anEx; pick(anEx); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let Some(Resolution::Candidates(remaining)) = the_call_resolution(&program, file, "pick") else {
        panic!("expected pick(anEx) to stay Candidates when both overloads are uncurated system types");
    };
    assert_eq!(remaining.len(), 2);
}

#[test]
fn id_and_blob_are_now_curated_and_correctly_disambiguate() {
    // `Id`/`Blob` used to be the exact pairing the test above used to
    // demonstrate "stays ambiguous outside the curated set" -- both are
    // curated now (see `crate::conversions`'s widened set, verified
    // against a real org), and are positively incompatible with each
    // other, so an `Id`-typed argument now correctly eliminates the
    // `Blob` overload instead of leaving both as candidates.
    let dir = write_fixture_dir(
        "id-blob-now-curated",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Id x) { } \
             public void pick(Blob x) { } \
             public void run() { Id anId; pick(anId); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let id_overload = pick_overload_with_param_type(&program, "Id");
    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(id_overload))
    );
}

#[test]
fn a_date_argument_prefers_the_exact_overload_over_datetime() {
    // `Date` widens to `Datetime` (verified against a real org), so both
    // overloads are individually applicable to a `Date` argument -- the
    // most-specific tiebreak must still prefer the exact `Date` overload,
    // the same "exact beats widened" shape numeric widening already has.
    let dir = write_fixture_dir(
        "date-exact-over-datetime",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Date x) { } \
             public void pick(Datetime x) { } \
             public void run() { Date d = Date.today(); pick(d); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let date_overload = pick_overload_with_param_type(&program, "Date");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(date_overload))
    );
}

#[test]
fn a_datetime_argument_eliminates_the_date_only_overload() {
    // The reverse direction: `Datetime` does not widen to `Date`
    // (a real `Illegal assignment from Datetime to Date` compile error),
    // so a `Datetime` argument must positively eliminate `pick(Date)`
    // and resolve straight to `pick(Datetime)`.
    let dir = write_fixture_dir(
        "datetime-eliminates-date",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(Date x) { } \
             public void pick(Datetime x) { } \
             public void run() { Datetime dt = Datetime.now(); pick(dt); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let datetime_overload = pick_overload_with_param_type(&program, "Datetime");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(datetime_overload))
    );
}

#[test]
fn a_real_object_typed_argument_prefers_its_exact_overload_over_sobject() {
    // Mirrors `a_list_of_a_project_local_subtype_eliminates_a_mismatched_element_leaf_type`'s
    // shape, but for the schema-verified `SObject` rule instead of a
    // project-local `extends` chain: `Account` widens to `SObject`
    // (confirmed real), but the exact-match `Account` overload must
    // still win over the wider `SObject` one.
    let dir = write_fixture_dir(
        "account-exact-over-sobject",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(SObject x) { } \
             public void pick(Account x) { } \
             public void run() { Account a = new Account(); pick(a); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let account_overload = pick_overload_with_param_type(&program, "Account");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(account_overload))
    );
}

/// The exact user-reported bug: a real NPSP call
/// (`BDI_DataImport_API.processDataImportRecords`) between a
/// `List<Id>`-typed overload and a `List<CustomObject__c>`-typed one
/// stayed an unbreakable `Resolution::Candidates` tie forever, since
/// nothing eliminated a real object argument against a curated scalar
/// like `Id` (see `conversions::system_type_compatible`'s own doc
/// comment on the org verification for this rule). Uses the bundled
/// standard `Contact` object rather than a custom-object fixture --
/// same underlying rule, no `.object-meta.xml` fixture needed.
#[test]
fn a_list_of_a_real_object_type_eliminates_a_list_of_id_overload() {
    let dir = write_fixture_dir(
        "list-of-object-vs-list-of-id",
        &[(
            "Toolbox.cls",
            "public class Toolbox { \
             public void pick(List<Id> ids) { } \
             public void pick(List<Contact> contacts) { } \
             public void run() { pick(new List<Contact>{ new Contact() }); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Toolbox");
    let contact_list_overload = pick_overload_with_collection_param(&program, "List", "Contact");

    assert_eq!(
        the_call_resolution(&program, file, "pick"),
        Some(Resolution::Resolved(contact_list_overload))
    );
}

/// The exact user-reported bug: fflib's real `checkFieldIsUpdateable`
/// three-way overload (`SObjectType,String` / `SObjectType,SObjectField`
/// / `SObjectType,DescribeFieldResult`), called with a `String`-typed
/// second argument, stayed a three-way `Resolution::Candidates` tie
/// forever -- goto-definition on the call showed all three overloads --
/// since neither `SObjectField` nor `DescribeFieldResult` is in
/// `crate::conversions`'s curated set, so a `String` argument couldn't
/// eliminate either one. Fixed by `conversions::system_type_compatible`'s
/// new rule that a curated *scalar* (`String` here) is never compatible
/// with a differently-named system type regardless of whether that other
/// name is itself curated (verified against a real org -- see that
/// function's own doc comment).
#[test]
fn a_string_argument_eliminates_sobjectfield_and_describefieldresult_overloads() {
    let dir = write_fixture_dir(
        "checkfieldisupdateable-string-vs-sobjectfield",
        &[(
            "fflib_SecurityUtils.cls",
            "public class fflib_SecurityUtils { \
             public static void checkFieldIsUpdateable(SObjectType objType, String fieldName) { } \
             public static void checkFieldIsUpdateable(SObjectType objType, SObjectField fieldToken) { } \
             public static void checkFieldIsUpdateable(SObjectType objType, DescribeFieldResult fieldDescribe) { } \
             public static void checkUpdate(SObjectType objType, List<String> fieldNames) { \
                 for (String fieldName : fieldNames) { \
                     checkFieldIsUpdateable(objType, fieldName); \
                 } \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "fflib_SecurityUtils");
    let string_overload = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "checkFieldIsUpdateable")
        .find(|(id, _)| {
            program
                .symbols
                .get(program.symbols.params(*id)[1])
                .type_name
                .as_deref()
                == Some("String")
        })
        .map(|(id, _)| id)
        .expect("checkFieldIsUpdateable(SObjectType, String) should exist");

    assert_eq!(
        the_call_resolution(&program, file, "checkFieldIsUpdateable"),
        Some(Resolution::Resolved(string_overload))
    );
}

/// A second, related real-reported shape off the same `checkFieldIsUpdateable`
/// overload trio: `fflib_SObjectDescribe.getDescribe(objType).getField(fieldName)`
/// -- a real `fflib_SObjectDescribe.getField` call, whose own declared
/// return type is spelled with its namespace (`Schema.SObjectField`),
/// resolving to the `SObjectField` overload, not `DescribeFieldResult`.
/// Needed two independent fixes to get here: `Schema.SObjectField` as a
/// declared/return type resolving to the real stdlib class at all (a
/// namespace-qualified-reference gap fixed earlier -- see
/// `crate::stdlib_index::StdlibIndex::class_in_namespace`), and then
/// `conversions::system_type_compatible`'s new "two different, both
/// real, uncurated stdlib classes are never compatible" rule to actually
/// eliminate the `DescribeFieldResult` overload once the argument's type
/// correctly propagated as `SObjectField`.
#[test]
fn a_resolved_sobjectfield_return_value_eliminates_the_describefieldresult_overload() {
    let dir = write_fixture_dir(
        "checkfieldisupdateable-sobjectfield-vs-describefieldresult",
        &[
            (
                "fflib_SecurityUtils.cls",
                "public class fflib_SecurityUtils { \
                 public static void checkFieldIsUpdateable(SObjectType objType, SObjectField fieldToken) { } \
                 public static void checkFieldIsUpdateable(SObjectType objType, DescribeFieldResult fieldDescribe) { } \
                 public static void run(SObjectType objType, String fieldName) { \
                     checkFieldIsUpdateable(objType, fflib_SObjectDescribe.getDescribe(objType).getField(fieldName)); \
                 } \
             }",
            ),
            (
                "fflib_SObjectDescribe.cls",
                "public class fflib_SObjectDescribe { \
                 public static fflib_SObjectDescribe getDescribe(SObjectType objType) { return null; } \
                 public Schema.SObjectField getField(String fieldName) { return null; } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "fflib_SecurityUtils");
    let sobjectfield_overload = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "checkFieldIsUpdateable")
        .find(|(id, _)| {
            program
                .symbols
                .get(program.symbols.params(*id)[1])
                .type_name
                .as_deref()
                == Some("SObjectField")
        })
        .map(|(id, _)| id)
        .expect("checkFieldIsUpdateable(SObjectType, SObjectField) should exist");

    assert_eq!(
        the_call_resolution(&program, file, "checkFieldIsUpdateable"),
        Some(Resolution::Resolved(sobjectfield_overload))
    );
}
