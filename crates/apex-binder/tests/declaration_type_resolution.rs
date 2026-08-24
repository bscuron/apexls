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

use apex_binder::{BoundProgram, FileId, Resolution, SymbolId, SymbolKind};
use apex_syntax::ast::Type;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
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
            (
                "Foo.cls",
                "public class Foo { public Bar b { get; set; } }",
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
        "a property's own declared type should resolve"
    );
}

#[test]
fn a_parameters_own_declared_type_resolves() {
    let dir = write_fixture_dir(
        "decl-type-param",
        &[
            (
                "Foo.cls",
                "public class Foo { public void run(Bar b) { } }",
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

    let file = file_for(&program, SymbolKind::Class, "fflib_ClassicUnitOfWorkFactory");
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
