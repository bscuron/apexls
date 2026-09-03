//! Coverage for `crate::soql`'s rarer real-SOQL/SOSL binding paths --
//! `TYPEOF`/`WHEN`/`THEN`/`ELSE`, a `FORMAT(soqlFunction)` nested call, a
//! parenthesized subquery's own `LIMIT :bind`, a top-level `OFFSET
//! :bind`, and SOSL's `RETURNING` field-spec `WHERE`/`ORDER BY`/`LIMIT`
//! tails plus its own top-level `WITH .../LIMIT :bind`. Every existing
//! SOQL-shaped test in this suite only ever exercised the common
//! `SELECT ... FROM ... WHERE ...` shape, so these binder-side branches
//! (as opposed to `apex-parser`'s grammar, covered separately) never ran.

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::NameExpr;
use apex_syntax::ast::soql::SoqlFieldName;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        let path = dir.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
    }
    dir
}

fn object_field_xml(object: &str, field: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("objects/{object}/{object}.object-meta.xml"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Obj</label></CustomObject>".to_string(),
        ),
        (
            format!("objects/{object}/fields/{field}.field-meta.xml"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>{field}</fullName><type>Text</type></CustomField>"
            ),
        ),
    ]
}

fn soql_field_resolution(program: &BoundProgram, text: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(fname) = SoqlFieldName::cast(node) else {
                continue;
            };
            if fname.text() == text {
                let ptr = SyntaxPtr::new(file, fname.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

/// How many `:n` bind-expression occurrences resolved to the method's
/// own local `n` -- used where several occurrences of the same bind
/// variable exist and the point is "did every one of these binder call
/// sites actually run", not any single occurrence's identity.
fn count_n_resolutions(program: &BoundProgram, file: apex_binder::FileId) -> usize {
    let n_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.file == file && s.kind == SymbolKind::LocalVar && s.name == "n")
        .map(|(id, _)| id)
        .expect("local `n` should have been collected");
    let root = program.syntax(file);
    root.descendants()
        .filter_map(NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == "n"))
        .filter(|n| {
            let ptr = SyntaxPtr::new(file, n.syntax());
            program.resolution(ptr) == Some(&Resolution::Resolved(n_id))
        })
        .count()
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

#[test]
fn typeof_when_then_else_all_resolve_against_their_own_object() {
    let mut files = vec![(
        "Foo.cls".to_string(),
        "public class Foo {\n    public void run() {\n        List<Root__c> a = [SELECT TYPEOF What__c WHEN TypeA__c THEN FieldA__c WHEN TypeB__c THEN FieldB__c ELSE Name END FROM Root__c];\n    }\n}\n".to_string(),
    )];
    for (path, content) in object_field_xml("Root__c", "What__c") {
        files.push((path, content));
    }
    for (path, content) in object_field_xml("TypeA__c", "FieldA__c") {
        files.push((path, content));
    }
    for (path, content) in object_field_xml("TypeB__c", "FieldB__c") {
        files.push((path, content));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let dir = write_fixture_dir("soql-typeof", &files);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        soql_field_resolution(&program, "What__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Root__c".into(),
            field: Some("What__c".into()),
        }))),
        "TYPEOF's own polymorphic field should resolve against the query's FROM object"
    );
    assert_eq!(
        soql_field_resolution(&program, "TypeA__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "TypeA__c".into(),
            field: None,
        }))),
        "a WHEN clause's object type should resolve as an object reference"
    );
    assert_eq!(
        soql_field_resolution(&program, "FieldA__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "TypeA__c".into(),
            field: Some("FieldA__c".into()),
        }))),
        "a THEN field should resolve against its own WHEN clause's object, not the FROM object"
    );
    assert_eq!(
        soql_field_resolution(&program, "FieldB__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "TypeB__c".into(),
            field: Some("FieldB__c".into()),
        }))),
    );
    assert_eq!(
        soql_field_resolution(&program, "Name"),
        Some(Resolution::Unresolved),
        "an ELSE field applies across every non-matched type, so it can't resolve against any single object"
    );
}

#[test]
fn a_nested_soql_function_resolves_its_inner_field() {
    let mut files = vec![(
        "Foo.cls".to_string(),
        "public class Foo {\n    public void run() {\n        List<Root__c> a = [SELECT FORMAT(SUM(Amount__c)) FROM Root__c];\n    }\n}\n".to_string(),
    )];
    for (path, content) in object_field_xml("Root__c", "Amount__c") {
        files.push((path, content));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let dir = write_fixture_dir("soql-nested-fn", &files);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        soql_field_resolution(&program, "Amount__c"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Root__c".into(),
            field: Some("Amount__c".into()),
        }))),
        "FORMAT(SUM(Amount__c))'s inner field should resolve through the nested-function recursion"
    );
}

/// A subquery's own `LIMIT :n`, a top-level `OFFSET :n`, and SOSL's
/// field-spec `WHERE`/`ORDER BY`/`LIMIT` tails plus its own top-level
/// `WITH .../LIMIT :n` -- five independent `:n` bind sites, each backed
/// by a different, previously-uncovered `bind_bound_expr` call site in
/// `crate::soql`. All five should resolve to the same local `n`.
#[test]
fn every_rare_bind_expression_site_resolves_the_same_local() {
    let mut files = vec![(
        "Foo.cls".to_string(),
        "public class Foo {\n    \
            public void run() {\n        \
                Integer n = 5;\n        \
                List<Root__c> a = [SELECT Id FROM Root__c WHERE Id IN (SELECT Id FROM TypeA__c LIMIT :n)];\n        \
                List<Root__c> b = [SELECT Id FROM Root__c OFFSET :n];\n        \
                List<List<SObject>> c = [find 'test' RETURNING \
                    TypeA__c(FieldA__c WHERE FieldA__c != null ORDER BY FieldA__c LIMIT :n) \
                    WITH DIVISION = :n \
                    LIMIT :n];\n    \
            }\n\
        }\n"
            .to_string(),
    )];
    for (path, content) in object_field_xml("Root__c", "Id") {
        files.push((path, content));
    }
    for (path, content) in object_field_xml("TypeA__c", "FieldA__c") {
        files.push((path, content));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let dir = write_fixture_dir("soql-rare-binds", &files);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    assert_eq!(
        count_n_resolutions(&program, file),
        5,
        "expected all five `:n` bind sites (subquery LIMIT, top-level OFFSET, SOSL field-spec LIMIT, SOSL WITH DIVISION, SOSL top-level LIMIT) to resolve to the local `n`"
    );
}
