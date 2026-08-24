//! Hand-written fixtures for `BoundProgram::resolution_at`/`symbol_at`
//! (`BACKLOG.md` §3's hover/definition groundwork): given a byte offset,
//! find whichever reference or declaration the cursor is on. Follows the
//! same fixture-on-disk pattern as `visibility_resolution.rs`.

use apex_binder::{BoundProgram, FileId, Resolution, SymbolKind};
use apex_syntax::ast::expr::NameExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

/// The single `NameExpr` (not a declaration site) named `name` in `file`,
/// asserting there's exactly one so a test's intent is unambiguous.
fn name_expr_range(program: &BoundProgram, file: FileId, name: &str) -> rowan::TextRange {
    let root = program.syntax(file);
    let matches: Vec<_> = root
        .descendants()
        .filter_map(NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == name))
        .collect();
    assert_eq!(matches.len(), 1, "expected exactly one `{name}` NameExpr");
    matches[0].syntax().text_range()
}

#[test]
fn cursor_anywhere_inside_an_identifier_resolves_the_same_reference() {
    let dir = write_fixture_dir(
        "position-mid-identifier",
        &[(
            "Foo.cls",
            "public class Foo { \
             public Integer value; \
             public void run() { Integer y = value; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let value_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "value")
        .map(|(id, _)| id)
        .expect("Foo.value should have been collected");

    let range = name_expr_range(&program, file, "value");
    let start: u32 = range.start().into();
    let end: u32 = range.end().into();
    for offset in [start, start + 2, end - 1] {
        assert_eq!(
            program.resolution_at(file, offset.into()).cloned(),
            Some(Resolution::Resolved(value_id)),
            "offset {offset} (range {range:?}) should resolve to `value`"
        );
    }
}

#[test]
fn cursor_on_receiver_vs_member_in_a_field_access_resolve_independently() {
    let dir = write_fixture_dir(
        "position-field-access",
        &[(
            "Foo.cls",
            "public class Foo { \
             public Integer bar; \
             public void run() { Foo other = new Foo(); Integer y = other.bar; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let bar_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "bar")
        .map(|(id, _)| id)
        .expect("Foo.bar should have been collected");
    let other_local_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::LocalVar && s.name == "other")
        .map(|(id, _)| id)
        .expect("`other` local should have been collected");

    let other_range = name_expr_range(&program, file, "other");
    assert_eq!(
        program.resolution_at(file, other_range.start()).cloned(),
        Some(Resolution::Resolved(other_local_id)),
        "cursor on the receiver `other` should resolve to the local variable"
    );

    // `bar` (the member) has no `NameExpr` of its own -- it's a bare
    // token inside the `FieldExpr` -- so find its offset relative to
    // `other`'s own end: `other` is immediately followed by `.bar` with
    // no space, so `other_range.end() + 1` skips the `.` and `+ 2` lands
    // on the middle character of `bar`, safely inside the token.
    let other_end: u32 = other_range.end().into();
    let bar_offset = other_end + 2;
    assert_eq!(
        program.resolution_at(file, bar_offset.into()).cloned(),
        Some(Resolution::Resolved(bar_id)),
        "cursor on the member `bar` should resolve to the field, independent of the receiver"
    );
}

#[test]
fn cursor_on_a_declarations_own_name_is_found_via_symbol_at_not_resolution_at() {
    let dir = write_fixture_dir(
        "position-declaration-site",
        &[("Foo.cls", "public class Foo { public Integer bar; }")],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let (bar_id, bar_symbol) = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "bar")
        .expect("Foo.bar should have been collected");
    let mid = bar_symbol.name_range.start() + rowan::TextSize::from(1);

    assert_eq!(
        program.symbol_at(file, mid),
        Some(bar_id),
        "cursor on a field's own declared name should be found via symbol_at"
    );
    assert_eq!(
        program.resolution_at(file, mid),
        None,
        "a declaration site is never itself a recorded reference"
    );
}

#[test]
fn cursor_in_whitespace_finds_nothing_from_either_lookup() {
    let dir = write_fixture_dir(
        "position-whitespace",
        &[("Foo.cls", "public  class Foo { }")], // two spaces: offset 7 is squarely inside
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let offset = rowan::TextSize::from(7);
    assert_eq!(program.resolution_at(file, offset), None);
    assert_eq!(program.symbol_at(file, offset), None);
}
