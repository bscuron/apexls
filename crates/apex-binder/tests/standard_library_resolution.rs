//! Regression tests for standard-library class/method/property
//! resolution (`apex_stdlib`'s bundled `apex_reference.json` snapshot,
//! consulted from `crate::resolve`'s `Ty::System` arms via the new
//! `crate::stdlib_index::StdlibIndex`). Before this, *every* method
//! call or property access on a non-project-local receiver resolved as
//! `Resolution::Unresolved` unconditionally -- a real `String.isBlank(...)`
//! call was indistinguishable from a genuine typo. No change to how
//! `crate::generics`'s `List`/`Map`/`Set` type-argument substitution
//! works (that's tried first and always wins when it applies); this is
//! purely the fallback for everything else.

use apex_binder::{BoundProgram, Resolution, SchemaObjectRef, StdlibMemberRef, SymbolKind};
use apex_syntax::ast::expr::{FieldExpr, MethodCallExpr, NameExpr};
use apex_syntax::ast::QualifiedName;
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

fn method_call_resolution(program: &BoundProgram, method_name: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(mc) = MethodCallExpr::cast(node) else { continue };
            let Some(name) = mc.method_name_token() else { continue };
            if name.text() == method_name {
                let ptr = apex_binder::SyntaxPtr::new(file, mc.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

fn field_expr_resolution(program: &BoundProgram, member: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(fe) = FieldExpr::cast(node) else { continue };
            let Some(name) = fe.member_token() else { continue };
            if name.text() == member {
                let ptr = apex_binder::SyntaxPtr::new(file, fe.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

fn name_expr_resolution(program: &BoundProgram, name: &str) -> Option<Resolution> {
    for file in program.files() {
        let root = program.syntax(file);
        for node in root.descendants() {
            let Some(ne) = NameExpr::cast(node) else { continue };
            let Some(tok) = ne.name_token() else { continue };
            if tok.text() == name {
                let ptr = apex_binder::SyntaxPtr::new(file, ne.syntax());
                return program.resolution(ptr).cloned();
            }
        }
    }
    None
}

#[test]
fn a_real_static_stdlib_method_call_resolves_to_stdlib_member() {
    let dir = write_fixture_dir(
        "stdlib-static-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { Boolean b = String.isBlank('x'); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "isBlank"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "String".into(),
            member: Some("isBlank".into()),
            arg_count: Some(1),
            narrowed_param_types: None,
        }))),
        "String.isBlank is a real, documented stdlib method"
    );
}

#[test]
fn an_overloaded_stdlib_method_call_also_resolves() {
    let dir = write_fixture_dir(
        "stdlib-overloaded-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run(String soql) { List<SObject> rows = Database.query(soql); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "query"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Database".into(),
            member: Some("query".into()),
            arg_count: Some(1),
            narrowed_param_types: None,
        }))),
        "Database.query is real and overloaded -- existence, not overload-exactness, decides the Resolution"
    );
}

#[test]
fn a_stdlib_property_access_resolves_to_stdlib_member() {
    // `Address.city` isn't itself a "usual" example, but confirms the
    // property path independent of the method-call path -- any real,
    // documented stdlib property works the same way `bind_field_expr`'s
    // schema-field lookup already does for an SObject field.
    let dir = write_fixture_dir(
        "stdlib-property-access",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { PageReference pr = ApexPages.currentPage(); Map<String,String> p = pr.getParameters(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // `ApexPages.currentPage()` and `pr.getParameters()` are both real
    // stdlib methods -- a chained-call smoke test that the propagated
    // `Ty::System` from one stdlib call correctly feeds the next one.
    assert_eq!(
        method_call_resolution(&program, "currentPage"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "ApexPages".into(),
            member: Some("currentPage".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        })))
    );
    assert_eq!(
        method_call_resolution(&program, "getParameters"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "PageReference".into(),
            member: Some("getParameters".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        }))),
        "chaining off Database/ApexPages's stdlib return type should still resolve the next call"
    );
}

/// A local variable declared with its own namespace spelled out
/// (`Schema.SObjectField token;`, real fflib-style code) must resolve
/// the declared type to the real stdlib class, not fall through to
/// `Unresolved` -- and a method called on that variable must resolve
/// too, since it depends on the declared type having resolved first.
/// Before this fix, `resolve_type_ref` only ever looked up the *whole*
/// dotted string (`"Schema.SObjectField"`) against `StdlibIndex`'s
/// by-bare-name map, which never matched, so both the declaration and
/// every member access through it stayed `Unresolved` -- no hover, no
/// goto-definition, nothing -- despite `SObjectField` being a real,
/// fully-documented stdlib class.
#[test]
fn a_namespace_qualified_declared_type_resolves_and_chains_into_a_method_call() {
    let dir = write_fixture_dir(
        "stdlib-namespace-qualified-declared-type",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { \
             Schema.SObjectField token; \
             Schema.DescribeFieldResult r = token.getDescribe(); \
             } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "getDescribe"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("Schema".into()),
            class_name: "SObjectField".into(),
            member: Some("getDescribe".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        }))),
        "token.getDescribe() must resolve once `Schema.SObjectField` itself resolves as the declared type"
    );
}

/// Negative case: an unmodeled/misspelled member on a real stdlib class
/// must stay `Unresolved`, not be over-eagerly matched -- guards against
/// `StdlibIndex::method`/`property` false-positiving on a name that
/// merely resembles a real one.
#[test]
fn an_unmodeled_stdlib_method_stays_unresolved() {
    let dir = write_fixture_dir(
        "stdlib-typo-call",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { String.definitelyNotARealStdlibMethod(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "definitelyNotARealStdlibMethod"),
        Some(Resolution::Unresolved)
    );
}

/// `generics.rs`'s `List<T>.get` substitution must keep winning over the
/// new stdlib fallback -- a project-local element type should still
/// resolve `Resolved`, not get reduced to `StdlibMember`/`Unresolved` by
/// the raw (unsubstituted) scraped `List.get` signature.
#[test]
fn list_get_still_substitutes_the_project_local_element_type() {
    let dir = write_fixture_dir(
        "stdlib-vs-generics-list-get",
        &[(
            "Widget.cls",
            "public class Widget { \
             public Integer count; \
             public void run() { \
                 List<Widget> items = new List<Widget>(); \
                 Integer c = items.get(0).count; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "count"),
        Some(Resolution::Resolved(
            program
                .symbols
                .iter()
                .find(|(_, s)| s.name == "count")
                .map(|(id, _)| id)
                .expect("Widget.count should have been collected")
        )),
        "List<Widget>.get(0)'s substituted element type must still let .count resolve"
    );
}

/// `List.sort()` isn't in `generics.rs`'s 13-entry table (no type-
/// argument substitution needed for a `void`-returning method), so it
/// must fall through to the new stdlib lookup instead of staying
/// `Unresolved` the way it did before this feature existed.
#[test]
fn list_sort_falls_through_to_the_stdlib_lookup() {
    let dir = write_fixture_dir(
        "stdlib-list-sort",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { List<Integer> xs = new List<Integer>(); xs.sort(); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "sort"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "List".into(),
            member: Some("sort".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        })))
    );
}

/// `LoggingLevel` is an `Enum`, not a `Class`/`Interface` -- confirms
/// enum constant access (`LoggingLevel.INFO`) resolves end-to-end: the
/// bare `LoggingLevel` receiver itself (`bind_name_expr`'s stdlib-class
/// fallback, `member: None`) and `.INFO` (`bind_field_expr`'s stdlib-
/// property fallback, since `apex_stdlib`'s scraper models an enum
/// value as a static property of the enum's own type). Real, motivating
/// example: `System.debug(LoggingLevel.INFO, 'hi')`.
#[test]
fn an_enum_constant_access_resolves_to_stdlib_member() {
    let dir = write_fixture_dir(
        "stdlib-enum-constant",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { System.debug(LoggingLevel.INFO, 'hi'); } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        name_expr_resolution(&program, "LoggingLevel"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: None,
            class_name: "LoggingLevel".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "the bare LoggingLevel receiver itself should resolve as a known stdlib class"
    );
    assert_eq!(
        field_expr_resolution(&program, "INFO"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: None,
            class_name: "LoggingLevel".into(),
            member: Some("INFO".into()),
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "LoggingLevel.INFO should resolve as a known stdlib property (an enum value)"
    );
}

/// A real object's *own* generic instance methods (`get`/`put`/
/// `getSObjectType`/`clone`/`addError`/`getErrors`/...) are declared
/// once on the scraped `SObject` class, not repeated per concrete object
/// type -- `Account.put(...)` used to fall to `Resolution::Unresolved`
/// unconditionally, since `stdlib.class("Account")` finds nothing (a
/// standard *object* is never itself a stdlib *class*). Uses the
/// bundled standard `Account` object, no `.object-meta.xml` fixture
/// needed.
#[test]
fn a_real_objects_generic_sobject_method_resolves_via_the_sobject_fallback() {
    let dir = write_fixture_dir(
        "stdlib-sobject-generic-method",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run() { \
                 Account a = new Account(); \
                 a.put('Name', 'test'); \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "put"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "SObject".into(),
            member: Some("put".into()),
            arg_count: Some(2),
            narrowed_param_types: None,
        }))),
        "Account.put should fall back to the generic SObject.put method"
    );
}

/// The exact user-reported bug: the same `SObject`-fallback rule, but
/// against a real *custom* object (`.object-meta.xml` fixture, like real
/// NPSP's `DataImport__c`) rather than a bundled standard one --
/// confirms the fallback checks `self.schema.object` (every real
/// object), not just the bundled standard-schema snapshot.
#[test]
fn a_custom_objects_generic_sobject_method_resolves_via_the_sobject_fallback() {
    let dir = write_fixture_dir(
        "stdlib-sobject-generic-method-custom",
        &[
            (
                "Foo.cls",
                "public class Foo { \
                 public void run(Custom__c c) { \
                     Schema.SObjectType t = c.getSObjectType(); \
                 } \
             }",
            ),
            (
                "objects/Custom__c/Custom__c.object-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Custom</label></CustomObject>",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "getSObjectType"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "SObject".into(),
            member: Some("getSObjectType".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        }))),
        "a real custom object's getSObjectType() should fall back to the generic SObject method"
    );
}

/// The exact user-reported bug: `sobj.Id` where `sobj` is declared as the
/// bare generic `SObject` type (e.g. a `Map<Id, SObject>` value), not a
/// concrete object -- real Apex allows `.Id` directly on any `SObject`-
/// typed value with no cast, since every SObject has one, unlike every
/// other field (`.Name`, say, isn't guaranteed the same way). `SObject`
/// itself is never a `self.schema.object(...)` entry (it isn't a real,
/// queryable object) and the bundled stdlib snapshot's own `SObject`
/// class genuinely has no scraped `properties` at all (Salesforce's own
/// docs treat `Id` as a schema field, not a class member) -- so both the
/// schema-field and stdlib-property fallbacks this test file otherwise
/// exercises miss it, and `sobj.Id` fell to `Resolution::Unresolved`
/// unconditionally before this fix.
#[test]
fn the_generic_sobject_types_id_field_resolves_without_a_cast() {
    let dir = write_fixture_dir(
        "stdlib-sobject-generic-id",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run(Map<Id, SObject> sObjectMap, SObject sobj) { \
                 SObject inMap = sObjectMap.get(sobj.Id); \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Id"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "SObject".into(),
            field: Some("Id".into()),
        }))),
        "sobj.Id should resolve as a field access on the generic SObject type, not Unresolved"
    );
}

/// The `X.class` reflection idiom (`MyInterface.class`, real, common Apex
/// for building a `Map<System.Type, System.Type>`-style binding registry)
/// -- `class` is a reserved word, never a real declared member, so
/// `bind_field_expr`'s ordinary member lookup always missed it regardless
/// of whether the receiver was a project type or a stdlib one. Distinct
/// from the already-working `List<Foo>.class` generic-collection form
/// (handled entirely differently, via `bind_name_expr`'s own `type_ref()`
/// check) -- this is the plain-name case that form doesn't cover.
#[test]
fn the_x_class_reflection_idiom_resolves_to_the_real_type_class() {
    let dir = write_fixture_dir(
        "stdlib-class-reflection",
        &[
            (
                "Foo.cls",
                "public class Foo { \
                 public void run() { \
                     System.Type t = MyInterface.class; \
                 } \
             }",
            ),
            ("MyInterface.cls", "public interface MyInterface {}"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "class"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Type".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "MyInterface.class should resolve as a reference to the real System.Type class"
    );
}

/// The exact user-reported bug: `Map<System.Type, System.Type> bindings;`'s
/// own `System.Type` generic type arguments never got the same
/// `class_in_namespace` fallback `type_of_symbol`'s *outer* declared type
/// already has a few lines below in the same function -- a namespace-
/// qualified stdlib type as a generic *argument* stayed the literal
/// unsplit dotted string (`"System.Type"`, never a real `StdlibIndex`
/// key), so a further hop off a `.get(...)`-substituted argument type
/// stayed `Unresolved` even though `bindings`'s own top-level `Map` type
/// resolved fine.
#[test]
fn a_namespace_qualified_generic_type_argument_resolves_for_chained_calls() {
    let dir = write_fixture_dir(
        "stdlib-generic-arg-namespace",
        &[(
            "Foo.cls",
            "public class Foo { \
             private Map<System.Type, System.Type> bindings; \
             public Object run(System.Type interfaceType) { \
                 return this.bindings.get(interfaceType).newInstance(); \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "newInstance"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Type".into(),
            member: Some("newInstance".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        }))),
        "chaining off a namespace-qualified generic type argument (Map<System.Type, System.Type>.get(...)) should keep resolving"
    );
}

/// `Trigger` (the special context-variable pseudo-class) had no
/// `apex_stdlib::standard_classes()` entry at all -- the same scraper-
/// extraction gap as `Exception`: a real page found, but its content (the
/// trigger context variables themselves) never captured as structured
/// properties, so `Trigger.oldMap`/`Trigger.isBefore`/... all stayed
/// `Unresolved` unconditionally. Real, common Apex: every trigger
/// handler class references at least one of these.
#[test]
fn trigger_context_variables_resolve_to_stdlib_members() {
    let dir = write_fixture_dir(
        "stdlib-trigger-context",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run() { \
                 Map<Id, SObject> oldMap = Trigger.oldMap; \
                 Boolean before = Trigger.isBefore; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "oldMap"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Trigger".into(),
            member: Some("oldMap".into()),
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "Trigger.oldMap should resolve as a real Trigger context variable"
    );
    assert_eq!(
        field_expr_resolution(&program, "isBefore"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Trigger".into(),
            member: Some("isBefore".into()),
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "Trigger.isBefore should resolve as a real Trigger context variable"
    );
}

/// A same-named `static` member declared independently on both a class
/// and its supertype used to leave `SymbolTable::lookup_member` with no
/// way to prefer the more-derived one: a real Apex compiler resolves
/// `Sub.member` (where `Sub extends Base`, both declaring their own
/// unrelated `static ... member`) to `Sub`'s own declaration without any
/// ambiguity (field-hiding semantics) -- this used to land in
/// `Resolution::Candidates` forever instead. Exact real NPSP shape:
/// `fflib_SObjectDomain extends fflib_SObjects`, both independently
/// declaring `static fflib_SObjectDomain.ErrorFactory Errors`-shaped
/// members.
#[test]
fn a_subclasss_own_static_member_shadows_a_same_named_one_on_its_supertype() {
    let dir = write_fixture_dir(
        "stdlib-static-member-shadowing",
        &[
            (
                "Base.cls",
                "public virtual class Base { public static String Config { get; private set; } }",
            ),
            (
                "Sub.cls",
                "public class Sub extends Base { \
                 public static Integer Config { get; private set; } \
                 public void run() { Integer c = Sub.Config; } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let sub_config = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Property && s.name == "Config" && s.type_name.as_deref() == Some("Integer"))
        .map(|(id, _)| id)
        .expect("Sub's own Config property should be collected");
    assert_eq!(
        field_expr_resolution(&program, "Config"),
        Some(Resolution::Resolved(sub_config)),
        "Sub.Config should resolve to Sub's own property, not stay Candidates against Base's unrelated one"
    );
}

/// The exact user-reported bug: `sObjectList[0].Name` where `sObjectList`
/// is `List<Opportunity>` -- `Expr::Index` (`list[0]`) never propagated a
/// `List<T>`'s own element type at all (a documented "v1" gap), so
/// `[0]`'s own result stayed untyped and every further chained access
/// (`.Name`) stayed `Unresolved` regardless of how real the field was.
#[test]
fn indexing_a_list_propagates_the_element_type_for_chained_access() {
    let dir = write_fixture_dir(
        "index-expr-element-type",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run(List<Opportunity> sObjectList) { \
                 String n = sObjectList[0].Name; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "Name"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Opportunity".into(),
            field: Some("Name".into()),
        }))),
        "sObjectList[0].Name should resolve as a real field access, not stay Unresolved"
    );
}

/// A schema field access outside SOQL only ever got an inferred `Ty` for
/// a *relationship* field (via its own `reference_to`) -- every *scalar*
/// field (`opp.Name`, a plain `String`) had none at all, so a chained
/// call on it (`opp.Name.equals(...)`) always stayed `Unresolved`, real
/// and common Apex, not an edge case. Also confirms the companion fix:
/// `sobjectExpr.Field.addError(msg)`, a real Apex compiler idiom (valid
/// on *any* field-value access chained off an SObject record, regardless
/// of that field's own scalar type -- verified against a real org), which
/// has no real method to find on the field's own type otherwise.
#[test]
fn a_scalar_schema_fields_value_resolves_a_chained_stdlib_call_and_add_error() {
    let dir = write_fixture_dir(
        "schema-scalar-field-type",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run(Opportunity opp) { \
                 Boolean b = opp.Name.equals('x'); \
                 opp.Type.addError('bad type'); \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "equals"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "String".into(),
            member: Some("equals".into()),
            arg_count: Some(1),
            narrowed_param_types: None,
        }))),
        "opp.Name.equals(...) should resolve now that opp.Name carries a real String Ty"
    );
    assert_eq!(
        method_call_resolution(&program, "addError"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "SObject".into(),
            member: Some("addError".into()),
            arg_count: Some(1),
            narrowed_param_types: None,
        }))),
        "opp.Type.addError(...) should resolve via the field-value addError idiom"
    );
}

/// `SObjectTypeName.SObjectType` (`Opportunity.SObjectType`,
/// `Schema.Opportunity.SObjectType`) -- a compiler-magic universal
/// property on any real SObject type name, confirmed against a real org.
/// The namespace-qualified form also confirms the companion fix:
/// `Schema.Opportunity` itself resolving as a real schema object, not
/// just a stdlib class-in-namespace lookup (`Opportunity` isn't a stdlib
/// class at all).
#[test]
fn sobjecttype_resolves_on_a_bare_and_namespace_qualified_sobject_name() {
    let dir = write_fixture_dir(
        "stdlib-sobjecttype-token",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run() { \
                 Schema.SObjectType t1 = Opportunity.SObjectType; \
                 Schema.SObjectType t2 = Schema.Opportunity.SObjectType; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        field_expr_resolution(&program, "SObjectType"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("Schema".into()),
            class_name: "SObjectType".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "Opportunity.SObjectType should resolve as a reference to the real Schema.SObjectType class"
    );
    assert_eq!(
        field_expr_resolution(&program, "Opportunity"),
        Some(Resolution::SchemaObject(Box::new(SchemaObjectRef {
            object: "Opportunity".into(),
            field: None,
        }))),
        "Schema.Opportunity should resolve as a real schema object, not stay Unresolved"
    );
}

/// The exact user-reported bug: a `catch (Type e)` clause's own variable
/// used to be declared with no type at all (`declare_local(..., None)`),
/// discarding it *unconditionally* -- so `e.getMessage()` stayed
/// `Unresolved` inside *every* catch block, not just for an unmodeled
/// exception type. Chains a further call (`.getMessage().contains(...)`)
/// on a *built-in* exception subtype specifically (`DmlException`, which
/// has no `apex_stdlib` entry of its own at all) to also confirm the
/// companion `*Exception`-name fallback in `bind_method_call_expr`'s
/// `Ty::System` arm, and that the stdlib fallback added for a project
/// type's own inherited call (steps 12/14) computes a real result type,
/// not just a resolution, so the chained call keeps resolving too.
#[test]
fn a_catch_variables_declared_type_resolves_for_chained_calls() {
    let dir = write_fixture_dir(
        "catch-var-type",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run() { \
                 try { \
                     doSomething(); \
                 } catch (System.DmlException dmlex) { \
                     Boolean has = dmlex.getMessage().contains('bad'); \
                 } \
             } \
             private void doSomething() {} \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        method_call_resolution(&program, "getMessage"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Exception".into(),
            member: Some("getMessage".into()),
            arg_count: Some(0),
            narrowed_param_types: None,
        }))),
        "dmlex.getMessage() should resolve via Exception's own methods, DmlException having none of its own modeled"
    );
    assert_eq!(
        method_call_resolution(&program, "contains"),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "String".into(),
            member: Some("contains".into()),
            arg_count: Some(1),
            narrowed_param_types: None,
        }))),
        "chaining .contains(...) off getMessage()'s own String result should keep resolving"
    );
}

/// The catch clause's own exception-type reference (a `QualifiedName`,
/// distinct from the catch *variable*'s declared type covered above) used
/// to have no stdlib fallback at all -- `catch (Exception e)` stayed
/// `Unresolved` even though bare `Exception` is a real, fully modeled
/// `apex_stdlib` class, since the catch-clause binder only ever consulted
/// project-local types (`SymbolTable::resolve_dotted_name`). Real-world
/// case this covers: NPSP's `fflib_QueryFactoryTest.cls`.
#[test]
fn a_bare_stdlib_exception_type_in_a_catch_clause_resolves() {
    let dir = write_fixture_dir(
        "catch-clause-stdlib-exception-type",
        &[(
            "Foo.cls",
            "public class Foo { \
             public void run() { \
                 try { \
                     doSomething(); \
                 } catch (Exception e) { \
                     System.debug(e); \
                 } \
             } \
             private void doSomething() {} \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = program.files().next().expect("one file");
    let root = program.syntax(file);
    let exception_type_node = root
        .descendants()
        .find_map(QualifiedName::cast)
        .expect("the catch clause's exception type should be a QualifiedName node");
    let ptr = apex_binder::SyntaxPtr::new(file, exception_type_node.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::StdlibMember(Box::new(StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Exception".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "catch (Exception e)'s own exception-type reference should resolve to the bundled \
         stdlib Exception class, not stay Unresolved"
    );
}
