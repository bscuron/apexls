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
