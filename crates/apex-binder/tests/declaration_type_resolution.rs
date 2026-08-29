//! A declaration's own type reference -- a field/property/parameter's
//! type, a method's return type, or an `extends`/`implements` supertype
//! -- used to have no recorded `Resolution` at all: Pass 2
//! (`crate::resolve`) only ever walked method/constructor/property-
//! accessor *bodies*, never a declaration's own signature, and Pass 1.5
//! (`crate::inherit`) resolved `extends`/`implements` purely into the
//! internal inheritance graph without ever recording a per-reference
//! `Resolution`. Confirms that gap is closed: clicking any of these
//! type names now resolves the same way a body-context type reference
//! (a cast, a local variable's type, ...) always did.

use apex_binder::{BoundProgram, FileId, Resolution, SymbolId, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::FieldExpr;
use apex_syntax::ast::Type;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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

fn file_for(program: &BoundProgram, kind: SymbolKind, name: &str) -> FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == kind && s.name == name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{name} should have been collected"))
}

fn symbol_id(program: &BoundProgram, kind: SymbolKind, name: &str) -> SymbolId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == kind && s.name == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("{name} should have been collected"))
}

/// A byte offset strictly *inside* the single `Type` node whose own text
/// is exactly `name` (`start + 1`, not `start` -- when a parameter type
/// immediately follows `(` with no space, e.g. `run(Bar b)`, `start`
/// itself sits exactly on the `(`/`Bar` token boundary, which
/// `BoundProgram::resolution_at`'s documented tie-break rule resolves
/// toward the real-content *left* token rather than `Bar`; a mid-token
/// offset sidesteps that ambiguity entirely, matching `position_lookup.rs`'s
/// own `start + 2` convention). Asserts there's exactly one match so a
/// test's intent is unambiguous.
fn type_ref_mid_offset(program: &BoundProgram, file: FileId, name: &str) -> rowan::TextSize {
    let root = program.syntax(file);
    let matches: Vec<_> = root
        .descendants()
        .filter_map(Type::cast)
        .filter(|t| t.text().as_str() == name)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one `{name}` Type reference"
    );
    matches[0].syntax().text_range().start() + rowan::TextSize::from(1)
}

#[test]
fn a_fields_own_declared_type_resolves() {
    let dir = write_fixture_dir(
        "decl-type-field",
        &[
            ("Foo.cls", "public class Foo { public Bar b; }"),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let bar_id = symbol_id(&program, SymbolKind::Class, "Bar");
    let offset = type_ref_mid_offset(&program, file, "Bar");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(bar_id)),
        "a field's own declared type should resolve"
    );
}

#[test]
fn a_propertys_own_declared_type_resolves() {
    let dir = write_fixture_dir(
        "decl-type-property",
        &[
            ("Foo.cls", "public class Foo { public Bar b { get; set; } }"),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let bar_id = symbol_id(&program, SymbolKind::Class, "Bar");
    let offset = type_ref_mid_offset(&program, file, "Bar");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(bar_id)),
        "a property's own declared type should resolve"
    );
}

#[test]
fn a_parameters_own_declared_type_resolves() {
    let dir = write_fixture_dir(
        "decl-type-param",
        &[
            ("Foo.cls", "public class Foo { public void run(Bar b) { } }"),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let bar_id = symbol_id(&program, SymbolKind::Class, "Bar");
    let offset = type_ref_mid_offset(&program, file, "Bar");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(bar_id)),
        "a parameter's own declared type should resolve"
    );
}

#[test]
fn a_methods_own_return_type_resolves() {
    let dir = write_fixture_dir(
        "decl-type-return",
        &[
            (
                "Foo.cls",
                "public class Foo { public Bar get() { return null; } }",
            ),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let bar_id = symbol_id(&program, SymbolKind::Class, "Bar");
    let offset = type_ref_mid_offset(&program, file, "Bar");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(bar_id)),
        "a method's own return type should resolve"
    );
}

/// A field with no initializer used to skip Pass 2 entirely (`bind_symbol_body`'s
/// `Field` arm bailed out via `decl.init()?` before this fix) -- so its
/// type reference resolving at all, not just resolving *correctly*, is
/// itself part of what this test confirms.
#[test]
fn a_field_with_no_initializer_still_resolves_its_own_type() {
    let dir = write_fixture_dir(
        "decl-type-field-no-init",
        &[
            ("Foo.cls", "public class Foo { public Bar b; }"),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let bar_id = symbol_id(&program, SymbolKind::Class, "Bar");
    let offset = type_ref_mid_offset(&program, file, "Bar");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(bar_id))
    );
}

#[test]
fn a_classs_extends_clause_resolves() {
    let dir = write_fixture_dir(
        "decl-type-extends",
        &[
            ("Foo.cls", "public virtual class Foo { }"),
            ("Bar.cls", "public class Bar extends Foo { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Bar");
    let foo_id = symbol_id(&program, SymbolKind::Class, "Foo");
    let offset = type_ref_mid_offset(&program, file, "Foo");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(foo_id)),
        "a class's `extends` clause should resolve to its base class"
    );
}

#[test]
fn a_classs_implements_clause_resolves() {
    let dir = write_fixture_dir(
        "decl-type-implements",
        &[
            ("Foo.cls", "public interface Foo { }"),
            ("Bar.cls", "public class Bar implements Foo { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Bar");
    let foo_id = symbol_id(&program, SymbolKind::Interface, "Foo");
    let offset = type_ref_mid_offset(&program, file, "Foo");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(foo_id)),
        "a class's `implements` clause should resolve to the interface"
    );
}

#[test]
fn an_interfaces_extends_clause_resolves_each_supertype_independently() {
    let dir = write_fixture_dir(
        "decl-type-interface-extends",
        &[
            ("A.cls", "public interface A { }"),
            ("B.cls", "public interface B { }"),
            ("C.cls", "public interface C extends A, B { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Interface, "C");
    let a_id = symbol_id(&program, SymbolKind::Interface, "A");
    let b_id = symbol_id(&program, SymbolKind::Interface, "B");
    let a_offset = type_ref_mid_offset(&program, file, "A");
    let b_offset = type_ref_mid_offset(&program, file, "B");

    assert_eq!(
        program.resolution_at(file, a_offset).cloned(),
        Some(Resolution::Resolved(a_id))
    );
    assert_eq!(
        program.resolution_at(file, b_offset).cloned(),
        Some(Resolution::Resolved(b_id))
    );
}

/// A qualified `Outer.Inner` supertype reference (the fflib/Enterprise-
/// pattern shape: `extends fflib_Application.UnitOfWorkFactory`) used to
/// silently fall through to `Unresolved` -- `resolve_type_ref` looked up
/// the *whole* dotted string as one name via `SymbolTable::top_level`,
/// which only indexes undotted top-level type names, so a nested type
/// referenced through its outer class was indistinguishable from a
/// genuinely nonexistent name. Covers both `extends` and a field's own
/// type, since both go through the same `resolve_type_ref`.
#[test]
fn a_qualified_supertypes_first_segment_resolves_to_the_outer_type() {
    let dir = write_fixture_dir(
        "decl-type-qualified-supertype",
        &[
            (
                "fflib_Application.cls",
                "public class fflib_Application { public virtual class UnitOfWorkFactory { } }",
            ),
            (
                "fflib_ClassicUnitOfWorkFactory.cls",
                "public virtual class fflib_ClassicUnitOfWorkFactory extends fflib_Application.UnitOfWorkFactory { }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(
        &program,
        SymbolKind::Class,
        "fflib_ClassicUnitOfWorkFactory",
    );
    // `type_ref_mid_offset` lands inside the *first* segment
    // (`fflib_Application`), which should resolve to itself, not the
    // nested `UnitOfWorkFactory` the whole path names -- see
    // `each_segment_of_a_qualified_type_resolves_independently`.
    let outer_id = symbol_id(&program, SymbolKind::Class, "fflib_Application");
    let offset = type_ref_mid_offset(&program, file, "fflib_Application.UnitOfWorkFactory");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(outer_id)),
        "cursor on a qualified extends clause's first segment should resolve to the outer type"
    );
}

#[test]
fn a_qualified_field_types_first_segment_resolves_to_the_outer_type() {
    let dir = write_fixture_dir(
        "decl-type-qualified-field",
        &[(
            "Foo.cls",
            "public class Foo { public class Inner { } public Foo.Inner i; }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    // `type_ref_mid_offset` lands inside the *first* segment (`Foo`),
    // which should resolve to itself, not the nested `Inner` the whole
    // path names.
    let outer_id = symbol_id(&program, SymbolKind::Class, "Foo");
    let offset = type_ref_mid_offset(&program, file, "Foo.Inner");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(outer_id)),
        "cursor on a qualified field type's first segment should resolve to the outer type"
    );
}

/// An `extends`/`implements` supertype name that doesn't exist anywhere
/// in the project stays `Unresolved`, same as any other unmodeled type
/// reference -- not silently unrecorded.
#[test]
fn an_unresolvable_supertype_name_is_recorded_as_unresolved() {
    let dir = write_fixture_dir(
        "decl-type-extends-unresolved",
        &[("Foo.cls", "public class Foo extends NoSuchClass { }")],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let offset = type_ref_mid_offset(&program, file, "NoSuchClass");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Unresolved)
    );
}

/// A field declared with a real stdlib type (`String`) resolves as
/// `StdlibMember` -- previously `resolve_type_ref` only ever checked
/// `SchemaIndex::object`, never the stdlib index, so a declared type's
/// own reference stayed `Unresolved` even though the *identical* name
/// used as an expression (`String.isBlank(...)`) already resolved
/// correctly. Confirmed a real, large-volume gap via a whole-corpus
/// `Unresolved`-clustering sweep, not a rare edge case.
#[test]
fn a_fields_stdlib_declared_type_resolves_as_stdlib_member() {
    let dir = write_fixture_dir(
        "decl-type-stdlib-field",
        &[("Foo.cls", "public class Foo { public String name; }")],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let offset = type_ref_mid_offset(&program, file, "String");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::StdlibMember(Box::new(apex_binder::StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "String".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "a field declared with a real stdlib type should resolve as StdlibMember"
    );
}

/// The generic-collection form of the same fix: `List`'s own `Type` node
/// resolves as `StdlibMember` even though `Contact` (its type argument)
/// is a *different* kind of reference (a real schema object) -- proving
/// the two don't interfere, since both go through the same recursive
/// `resolve_type_ref` call.
#[test]
fn a_local_variables_generic_stdlib_type_resolves_as_stdlib_member() {
    let dir = write_fixture_dir(
        "decl-type-stdlib-generic",
        &[(
            "Foo.cls",
            "public class Foo { public void run() { List<Contact> contacts; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let list_offset = type_ref_mid_offset(&program, file, "List");
    let contact_offset = type_ref_mid_offset(&program, file, "Contact");

    assert_eq!(
        program.resolution_at(file, list_offset).cloned(),
        Some(Resolution::StdlibMember(Box::new(apex_binder::StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "List".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "List's own Type node should resolve as StdlibMember"
    );
    assert_eq!(
        program.resolution_at(file, contact_offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(apex_binder::SchemaObjectRef {
            object: "Contact".into(),
            field: None,
        }))),
        "List<Contact>'s own type argument should still resolve as a real schema object"
    );
}

/// A method's parameter type and return type both go through the same
/// fix (both are `resolve_type_ref` call sites via `bind_symbol_body`'s
/// `declared_type` closure in `crates/apex-binder/src/lib.rs`).
#[test]
fn a_methods_stdlib_param_and_return_types_both_resolve() {
    let dir = write_fixture_dir(
        "decl-type-stdlib-method",
        &[(
            "Foo.cls",
            "public class Foo { public Boolean run(Decimal amount) { return true; } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let return_offset = type_ref_mid_offset(&program, file, "Boolean");
    let param_offset = type_ref_mid_offset(&program, file, "Decimal");

    assert_eq!(
        program.resolution_at(file, return_offset).cloned(),
        Some(Resolution::StdlibMember(Box::new(apex_binder::StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Boolean".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "a method's own return type should resolve as StdlibMember"
    );
    assert_eq!(
        program.resolution_at(file, param_offset).cloned(),
        Some(Resolution::StdlibMember(Box::new(apex_binder::StdlibMemberRef {
            namespace: Some("System".into()),
            class_name: "Decimal".into(),
            member: None,
            arg_count: None,
            narrowed_param_types: None,
        }))),
        "a method's own parameter type should resolve as StdlibMember"
    );
}

/// An `Exception` subtype used as a declared type stays honestly
/// `Unresolved` -- the scraped stdlib snapshot has no entry for
/// `Exception`/`DmlException`/etc. at all (the real Apex Reference Guide
/// only documents them on grouped, empty-methods "Built-In Exceptions"
/// pages `apex_stdlib::standard_classes` already filters out), so this
/// fix must not silently misreport a genuinely unmodeled name as
/// `StdlibMember` just because it looks similar in shape.
#[test]
fn an_exception_subtype_declared_type_stays_unresolved() {
    let dir = write_fixture_dir(
        "decl-type-exception-unresolved",
        &[("Foo.cls", "public class Foo { public DmlException err; }")],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let offset = type_ref_mid_offset(&program, file, "DmlException");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Unresolved)
    );
}

/// A for-each loop variable referenced multiple times, including inside
/// a `new List<T>{ ... }` collection-initializer -- every occurrence
/// (not just the first) resolves back to the same `ForEachVar` symbol.
#[test]
fn a_foreach_variable_resolves_at_every_reference_including_inside_a_collection_initializer() {
    let src = "public class Foo {\n    Map<String, List<SObject>> sObjectsByType = new Map<String, List<SObject>>();\n    public void run(List<SObject> sObjects) {\n        for (SObject sObj : sObjects) {\n            String sObjType = sObj.getSObjectType().getDescribe().getName();\n            if (sObjectsByType.containsKey(sObjType)) {\n                sObjectsByType.get(sObjType).add(sObj);\n            } else {\n                sObjectsByType.put(sObjType, new List<SObject>{ sObj });\n            }\n        }\n    }\n}\n";
    let dir = write_fixture_dir("foreach-repro", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let sobj_id = symbol_id(&program, SymbolKind::ForEachVar, "sObj");

    let root = program.syntax(file);
    let sobj_refs: Vec<_> = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == "sObj"))
        .collect();
    assert_eq!(
        sobj_refs.len(),
        3,
        "expected 3 references to `sObj` (receiver, `.add(sObj)`, and inside the collection initializer)"
    );
    for r in sobj_refs {
        let offset = r.syntax().text_range().start() + rowan::TextSize::from(1);
        assert_eq!(
            program.resolution_at(file, offset).cloned(),
            Some(Resolution::Resolved(sobj_id)),
            "every `sObj` reference (range {:?}) should resolve to the for-each variable",
            r.syntax().text_range()
        );
    }
}

/// Each segment of a qualified `Outer.Inner` type reference resolves
/// independently, matching whichever part the cursor is actually on --
/// `TDTM_Runnable` in `TDTM_Runnable.DmlWrapper` used to resolve to
/// `DmlWrapper` (the *whole* path's answer) regardless of cursor
/// position, since the dotted path is one flat `Type` node with no
/// sub-node of its own for `TDTM_Runnable` to climb to independently
/// (unlike a `FieldExpr`'s receiver, which does have its own `NameExpr`).
#[test]
fn each_segment_of_a_qualified_type_resolves_independently() {
    let dir = write_fixture_dir(
        "decl-type-qualified-segments",
        &[
            (
                "TDTM_Runnable.cls",
                "public class TDTM_Runnable { public class DmlWrapper { } }",
            ),
            (
                "Foo.cls",
                "public class Foo { public TDTM_Runnable.DmlWrapper w; }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let outer_id = symbol_id(&program, SymbolKind::Class, "TDTM_Runnable");
    let inner_id = symbol_id(&program, SymbolKind::Class, "DmlWrapper");

    let outer_offset = type_ref_mid_offset(&program, file, "TDTM_Runnable.DmlWrapper");
    // `type_ref_mid_offset` lands inside the *first* segment (`start + 1`
    // is inside `TDTM_Runnable`); the second segment's own offset is
    // computed directly since it isn't its own `Type` node.
    let whole_range = {
        let root = program.syntax(file);
        root.descendants()
            .filter_map(Type::cast)
            .find(|t| t.text().as_str() == "TDTM_Runnable.DmlWrapper")
            .unwrap()
            .syntax()
            .text_range()
    };
    let inner_offset = whole_range.end() - rowan::TextSize::from(1);

    assert_eq!(
        program.resolution_at(file, outer_offset).cloned(),
        Some(Resolution::Resolved(outer_id)),
        "cursor on `TDTM_Runnable` should resolve to `TDTM_Runnable` itself, not `DmlWrapper`"
    );
    assert_eq!(
        program.resolution_at(file, inner_offset).cloned(),
        Some(Resolution::Resolved(inner_id)),
        "cursor on `DmlWrapper` should resolve to `DmlWrapper`"
    );
}

/// An *unqualified* reference to a nested type from within its own
/// enclosing type's scope used to never resolve at all -- real NPSP
/// shape: `TDTM_Runnable`'s own abstract `run` method returns
/// `List<DmlWrapper>`, not `List<TDTM_Runnable.DmlWrapper>` (Apex
/// allows dropping the qualifier from inside the enclosing type).
/// `resolve_type_ref`'s single-segment path only ever called
/// `SymbolTable::top_level`, which indexes top-level type names only --
/// a nested type was never reachable that way, so it silently landed on
/// `Unresolved` (indistinguishable from a genuinely nonexistent name)
/// even from directly inside its own declaring class.
#[test]
fn an_unqualified_nested_type_reference_resolves_from_within_its_enclosing_type() {
    let dir = write_fixture_dir(
        "unqualified-nested",
        &[(
            "TDTM_Runnable.cls",
            "global abstract class TDTM_Runnable {\n    global virtual class DmlWrapper {\n    }\n    global abstract List<DmlWrapper> run();\n}\n",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "TDTM_Runnable");
    let dml_wrapper_id = symbol_id(&program, SymbolKind::Class, "DmlWrapper");
    let offset = type_ref_mid_offset(&program, file, "DmlWrapper");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(dml_wrapper_id)),
        "an unqualified reference to a sibling nested type should resolve from inside the enclosing class"
    );
}

/// Regression test for a real bug found via a user report: an unqualified
/// reference to a *sibling* nested type -- from inside one nested class
/// to another nested class declared in the same enclosing outer class,
/// neither one being the outer class itself nor extending the other --
/// failed to resolve (`new TestSObjectDomain(...)` from inside
/// `TestSObjectDomainConstructor`, both nested in `fflib_SObjectDomain`).
/// `resolve_type_ref`'s single-segment fallback only ever tried
/// `nested_type_visible_from` against the reference site's *immediate*
/// enclosing type (plus what it extends/implements) -- never walked
/// outward to that type's *own* container, so a name declared as a
/// sibling one level up was never reachable, even though real Apex
/// resolves it lexically through the whole enclosing-scope chain.
#[test]
fn an_unqualified_reference_to_a_sibling_nested_type_resolves_through_their_shared_outer_class() {
    let dir = write_fixture_dir(
        "unqualified-nested-sibling",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { } \
             public class Other { public Object make() { return new Inner(); } } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let other_file = file_for(&program, SymbolKind::Class, "Other");
    let inner_id = symbol_id(&program, SymbolKind::Class, "Inner");
    let offset = type_ref_mid_offset(&program, other_file, "Inner");

    assert_eq!(
        program.resolution_at(other_file, offset).cloned(),
        Some(Resolution::Resolved(inner_id)),
        "an unqualified reference to a sibling nested type must resolve through their shared outer class"
    );
}

/// A chained member access through a property/field whose *declared
/// type* is a nested type (no name collision between the two -- see the
/// test below for that variant) needs `BodyBinder::type_of_symbol` to
/// infer a member's `Ty` from its `type_name` text so a *following*
/// `.member` access has something to look up against. It had the exact
/// same gap `resolve_type_ref` did: only checked `SymbolTable::top_level`
/// (top-level names only), so a property/field typed as a *nested* type
/// could never chain further.
#[test]
fn a_chained_member_access_through_a_nested_typed_property_resolves() {
    let dir = write_fixture_dir(
        "chained-nested-typed-property",
        &[(
            "Outer.cls",
            "public class Outer { \
             public Settings config { get; set; } \
             public void run() { Boolean x = config.flag; } \
             public class Settings { public Boolean flag; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let outer_file = file_for(&program, SymbolKind::Class, "Outer");
    let flag_id = symbol_id(&program, SymbolKind::Field, "flag");

    let root = program.syntax(outer_file);
    let field_expr = root
        .descendants()
        .find_map(FieldExpr::cast)
        .expect("config.flag should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_expr.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(flag_id)),
        "a member access chained through a nested-typed property must resolve the member after the dot"
    );
}

/// Regression test for a real bug found via a user report: a property
/// named *identically* to its own nested-class type -- real NPSP shape:
/// `fflib_SObjectDomain`'s `public Configuration Configuration { get;
/// private set; }`, with a nested `public class Configuration { ... }`
/// declared alongside it -- broke goto-definition/find-references on
/// anything chained off it (`handleAfterUpdate` reading
/// `Configuration.OldOnUpdateValidateBehaviour`). `lookup_member` finds
/// *both* the property and the nested type for a bare "Configuration"
/// reference (nested types are indexed as ordinary members, no kind
/// partitioning), which used to land in `bind_name_expr`'s `Candidates`
/// arm and return `None` immediately -- never even reaching
/// `type_of_symbol`, so `.OldOnUpdateValidateBehaviour` had no type to
/// look itself up against and stayed `Unresolved`, indistinguishable
/// from a genuine typo. Real Apex (and this idiom's whole point) treats
/// the value as winning over the same-named type in expression position.
#[test]
fn a_property_named_identically_to_its_own_nested_type_resolves_when_chained() {
    let dir = write_fixture_dir(
        "same-name-property-and-nested-type",
        &[(
            "Outer.cls",
            "public class Outer { \
             public Settings Settings { get; private set; } \
             public void run() { Boolean x = Settings.flag; } \
             public class Settings { public Boolean flag; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let outer_file = file_for(&program, SymbolKind::Class, "Outer");
    let flag_id = symbol_id(&program, SymbolKind::Field, "flag");

    let root = program.syntax(outer_file);
    let field_expr = root
        .descendants()
        .find_map(FieldExpr::cast)
        .expect("Settings.flag should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_expr.syntax());

    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(flag_id)),
        "a property named identically to its own nested type must still resolve a chained member access, \
         preferring the property (value) over the type"
    );
}

/// The same fallback also walks the *inherited* chain -- a subclass
/// referencing its base class's nested type unqualified, same as an
/// inherited field or method doesn't need qualifying either.
#[test]
fn an_unqualified_nested_type_reference_resolves_through_a_subclass() {
    let dir = write_fixture_dir(
        "unqualified-nested-inherited",
        &[
            (
                "Base.cls",
                "public virtual class Base { public class Inner { } }",
            ),
            (
                "Sub.cls",
                "public class Sub extends Base { public Inner make() { return null; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Sub");
    let inner_id = symbol_id(&program, SymbolKind::Class, "Inner");
    let offset = type_ref_mid_offset(&program, file, "Inner");

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(inner_id)),
        "an unqualified reference to an inherited nested type should resolve from a subclass"
    );
}

/// A nested enum's constant, referenced through the full
/// `Outer.NestedEnum.CONSTANT` chain from a *different* top-level class
/// -- the real shape `UTIL_IntegrationConfig.Integration.ArchiveBridge`
/// takes. Two independent bugs combined to break this:
/// 1. `EnumConstant` symbols got `ModifierSet::default()` at collection
///    time, defaulting their visibility to `Private` -- correct for a
///    genuinely-unmarked *member*, but Apex enum values have no
///    visibility syntax of their own at all; access is governed by the
///    enum type's own visibility. `is_visible_from` then rejected every
///    enum constant referenced from outside its declaring top-level
///    type.
/// 2. Even with visibility fixed, `type_of_symbol` returned `None` for
///    a type-declaration symbol (`Class`/`Interface`/`Enum`) itself, so
///    resolving the `Integration` link of the chain produced no `Ty` to
///    chain `.ArchiveBridge` off of -- any reference chained past a
///    nested type/enum used as a qualifier dead-ended at `Unresolved`
///    regardless of the final member's own visibility.
#[test]
fn a_nested_enum_constant_resolves_through_the_full_qualifier_chain_from_another_class() {
    let dir = write_fixture_dir(
        "enum-constant-chain",
        &[
            (
                "UTIL_IntegrationConfig.cls",
                "public class UTIL_IntegrationConfig {\n    public enum Integration { ArchiveBridge, Other }\n    public static Integration getConfig(Integration i) { return i; }\n}\n",
            ),
            (
                "Foo.cls",
                "public class Foo {\n    public void run() {\n        Object archiveBridgeConfig = UTIL_IntegrationConfig.getConfig(UTIL_IntegrationConfig.Integration.ArchiveBridge);\n    }\n}\n",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let archive_bridge_id = symbol_id(&program, SymbolKind::EnumConstant, "ArchiveBridge");

    let root = program.syntax(file);
    let chain_range = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::FieldExpr::cast)
        .map(|f| f.syntax().text_range())
        .max_by_key(|r| r.len())
        .expect("expected at least one FieldExpr");
    let offset = chain_range.end() - rowan::TextSize::from(1);

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(archive_bridge_id)),
        "the full Outer.NestedEnum.CONSTANT chain should resolve to the enum constant"
    );
}

/// A custom SObject field accessed through plain dot-notation in a body
/// expression (`this.dataImport.Status__c`), *not* inside a SOQL query
/// -- `bind_field_expr` only ever handled `Ty::Project` (a project-local
/// class/interface/enum) as a chain target; a `Ty::System` target (a
/// schema SObject value, e.g. `dataImport`'s declared type `DataImport__c`)
/// fell straight to `Unresolved` without ever consulting `SchemaIndex`,
/// even though the exact same object/field lookup `crate::soql` already
/// does for a SOQL query was available and correct here too.
#[test]
fn a_custom_sobject_field_accessed_via_dot_notation_resolves() {
    let dir = write_fixture_dir(
        "sobject-field-access",
        &[
            (
                "Foo.cls",
                "public class Foo {\n    DataImport__c dataImport;\n    public String getStatus() {\n        return this.dataImport.Status__c;\n    }\n}\n",
            ),
            (
                "objects/DataImport__c/DataImport__c.object-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Data Import</label></CustomObject>",
            ),
            (
                "objects/DataImport__c/fields/Status__c.field-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Status__c</fullName><type>Picklist</type></CustomField>",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let root = program.syntax(file);
    let chain_range = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::FieldExpr::cast)
        .map(|f| f.syntax().text_range())
        .max_by_key(|r| r.len())
        .expect("expected at least one FieldExpr");
    let offset = chain_range.end() - rowan::TextSize::from(1);

    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(
            apex_binder::SchemaObjectRef {
                object: "DataImport__c".into(),
                field: Some("Status__c".into()),
            }
        ))),
        "this.dataImport.Status__c should resolve against the schema, not fall to Unresolved"
    );
}

/// The `SObjectType.Field` token form (`DataImport__c.Elevate_Payment_Status__c`,
/// most often passed straight to `String.valueOf(...)` to get a field's
/// API name) -- a *bare* SObject name used directly as an expression,
/// not preceded by `this.`/a variable. `bind_name_expr`'s final
/// fallback only ever checked `SymbolTable::top_level` (project-local
/// types); `resolve_type_ref` already had the equivalent
/// `SchemaIndex::object` fallback for a *type* reference, but the
/// *expression* path never got it. Without it, `DataImport__c` itself
/// never resolved, so the field chained off it never got a target type
/// to resolve against either -- both the object and every field on it
/// failed together.
#[test]
fn a_bare_sobject_type_token_and_its_field_both_resolve() {
    let dir = write_fixture_dir(
        "sobject-field-token",
        &[
            (
                "Foo.cls",
                "public class Foo {\n    private List<String> elevateFields() {\n        List<String> names = new List<String>{\n            String.valueOf(DataImport__c.Elevate_Payment_Status__c)\n        };\n        return names;\n    }\n}\n",
            ),
            (
                "objects/DataImport__c/DataImport__c.object-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Data Import</label></CustomObject>",
            ),
            (
                "objects/DataImport__c/fields/Elevate_Payment_Status__c.field-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Elevate_Payment_Status__c</fullName><type>Picklist</type></CustomField>",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let root = program.syntax(file);

    let object_offset = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::NameExpr::cast)
        .find(|n| n.name_token().is_some_and(|t| t.text() == "DataImport__c"))
        .map(|n| n.syntax().text_range().start() + rowan::TextSize::from(1))
        .expect("expected a DataImport__c NameExpr");
    assert_eq!(
        program.resolution_at(file, object_offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(
            apex_binder::SchemaObjectRef {
                object: "DataImport__c".into(),
                field: None,
            }
        ))),
        "the bare SObject type name itself should resolve"
    );

    let chain_range = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::FieldExpr::cast)
        .map(|f| f.syntax().text_range())
        .max_by_key(|r| r.len())
        .expect("expected at least one FieldExpr");
    let field_offset = chain_range.end() - rowan::TextSize::from(1);
    assert_eq!(
        program.resolution_at(file, field_offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(
            apex_binder::SchemaObjectRef {
                object: "DataImport__c".into(),
                field: Some("Elevate_Payment_Status__c".into()),
            }
        ))),
        "the field chained off the bare SObject type should also resolve"
    );
}

/// A lookup field's `__r` relationship alias, accessed in a plain body
/// expression (not SOQL): `dataImport.Related__r.Name__c`. `Related__r`
/// itself should resolve to the real field `Related__c` (the alias, not
/// a field in its own right), and the chain should keep going onto the
/// *related* object's own `Name__c` -- mirroring `crate::soql`'s
/// existing relationship-hop resolution, now shared via
/// `schema_index::relationship_field_api_name` rather than duplicated.
#[test]
fn a_relationship_alias_resolves_and_chains_to_the_related_objects_field() {
    let dir = write_fixture_dir(
        "relationship-lookup",
        &[
            (
                "Foo.cls",
                "public class Foo {\n    DataImport__c dataImport;\n    public String getName() {\n        return this.dataImport.Related__r.Name__c;\n    }\n}\n",
            ),
            (
                "objects/DataImport__c/DataImport__c.object-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Data Import</label></CustomObject>",
            ),
            (
                "objects/DataImport__c/fields/Related__c.field-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Related__c</fullName><type>Lookup</type><referenceTo>Related_Object__c</referenceTo></CustomField>",
            ),
            (
                "objects/Related_Object__c/Related_Object__c.object-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>Related Object</label></CustomObject>",
            ),
            (
                "objects/Related_Object__c/fields/Name__c.field-meta.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Name__c</fullName><type>Text</type></CustomField>",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let root = program.syntax(file);
    let mut chains: Vec<rowan::TextRange> = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::FieldExpr::cast)
        .map(|f| f.syntax().text_range())
        .collect();
    chains.sort_by_key(|r| r.len());
    // Three nested `FieldExpr`s in source order of increasing range:
    // `this.dataImport`, `...Related__r`, `...Name__c` (the full chain).
    assert_eq!(chains.len(), 3, "expected three nested FieldExprs");
    let relationship_offset = chains[1].end() - rowan::TextSize::from(1);
    let field_offset = chains[2].end() - rowan::TextSize::from(1);

    assert_eq!(
        program.resolution_at(file, relationship_offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(
            apex_binder::SchemaObjectRef {
                object: "DataImport__c".into(),
                field: Some("Related__c".into()),
            }
        ))),
        "Related__r should resolve to the real field Related__c"
    );
    assert_eq!(
        program.resolution_at(file, field_offset).cloned(),
        Some(Resolution::SchemaObject(Box::new(
            apex_binder::SchemaObjectRef {
                object: "Related_Object__c".into(),
                field: Some("Name__c".into()),
            }
        ))),
        "the chain should continue onto the related object's own field"
    );
}

/// The exact real NPSP shape that surfaced this gap:
/// `fflib_SObjectDomain.ObjectError extends Error`, where `Error` is a
/// *sibling* nested class -- both declared directly inside the same
/// enclosing `fflib_SObjectDomain`, `Error` referenced unqualified.
/// `SymbolTable::resolve_dotted_name` alone only ever checks `top_level`,
/// which a nested class is never keyed under, so `Error` never became
/// `ObjectError`'s recorded `direct_super`/`inherited_chain` member at
/// all -- `Error`'s own inherited field (`message`) then stayed
/// permanently unreachable from `ObjectError`, even though a real
/// compiler resolves this without any ambiguity.
#[test]
fn extends_resolves_an_unqualified_sibling_nested_type() {
    let dir = write_fixture_dir(
        "decl-type-sibling-extends",
        &[(
            "Foo.cls",
            "public class Foo { \
             public class ObjectError extends Error { \
                 public void run() { this.message = 'x'; } \
             } \
             public abstract class Error { \
                 public String message; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let object_error = symbol_id(&program, SymbolKind::Class, "ObjectError");
    let error = symbol_id(&program, SymbolKind::Class, "Error");
    assert_eq!(
        program.symbols.direct_super(object_error),
        Some(error),
        "ObjectError's direct_super should resolve to the sibling nested Error class"
    );

    let file = file_for(&program, SymbolKind::Class, "Foo");
    let root = program.syntax(file);
    let message_field = root
        .descendants()
        .filter_map(apex_syntax::ast::expr::FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "message"))
        .expect("expected a this.message FieldExpr");
    let offset = message_field.syntax().text_range().end() - rowan::TextSize::from(1);
    let message_field_id = symbol_id(&program, SymbolKind::Field, "message");
    assert_eq!(
        program.resolution_at(file, offset).cloned(),
        Some(Resolution::Resolved(message_field_id)),
        "this.message should resolve to Error's inherited field, not stay Unresolved"
    );
}
