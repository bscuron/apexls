//! Regression coverage for a real bug: this parser attaches trailing
//! trivia (almost always at least one whitespace character) as a child
//! *inside* a `DeclName`/`NameExpr`/`Type`/`QualifiedName` node itself,
//! rather than as leading trivia of whatever token follows. Using such a
//! node's raw `.text_range()` for `Symbol::name_range` or a reference's
//! highlight range therefore silently over-extended by however much
//! trailing trivia followed the identifier -- invisible to anything that
//! only cares about the *start* of a range (cursor placement, existence
//! checks), but a real, user-visible bug for anything sensitive to the
//! exact *end* boundary: a `textDocument/rename` `TextEdit` computed this
//! way ate one extra character (typically a space) past the identifier
//! it meant to replace, corrupting the surrounding source. Found via a
//! user report: renaming a variable to a short name left the file with
//! merged tokens (`Integer x= 0;` instead of `Integer x = 0;`).
//!
//! Every case below asserts the *exact* text at a range/highlight_range
//! equals just the identifier -- not merely that renaming "worked" by
//! some looser measure -- since that exact-text check is precisely what
//! the original bug silently failed.

use apex_binder::{BoundProgram, SymbolKind, SyntaxPtr};

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn text_at(src: &str, range: rowan::TextRange) -> &str {
    &src[usize::from(range.start())..usize::from(range.end())]
}

#[test]
fn every_declaration_kinds_name_range_is_exactly_the_identifier() {
    const SRC: &str = "public class Widget implements Comparable {\n    \
         public Integer count;\n    \
         public Integer Total { get; set; }\n    \
         public Integer add(Integer x) { return count + x; }\n    \
         public Widget() { }\n    \
         public enum Color { Red, Green }\n\
     }\n";
    let dir = write_fixture_dir("name-range-precision", &[("Widget.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let expected: &[(SymbolKind, &str, &str)] = &[
        (SymbolKind::Class, "Widget", SRC),
        (SymbolKind::Field, "count", SRC),
        (SymbolKind::Property, "Total", SRC),
        (SymbolKind::Method, "add", SRC),
        (SymbolKind::Parameter, "x", SRC),
        (SymbolKind::Constructor, "Widget", SRC),
        (SymbolKind::Enum, "Color", SRC),
        (SymbolKind::EnumConstant, "Red", SRC),
        (SymbolKind::EnumConstant, "Green", SRC),
    ];
    for &(kind, name, src) in expected {
        let symbol = program
            .symbols
            .iter()
            .find(|(_, s)| s.kind == kind && s.name == name)
            .unwrap_or_else(|| panic!("{kind:?} {name} should have been collected"))
            .1;
        assert_eq!(
            text_at(src, symbol.name_range),
            name,
            "{kind:?} {name}'s name_range should be exactly the identifier, got {:?}",
            text_at(src, symbol.name_range)
        );
    }
}

#[test]
fn a_local_variable_reference_and_declaration_highlight_exactly_the_identifier() {
    const SRC: &str = "public class Widget {\n    public void run() {\n        \
         Integer accountRecordTypeId = 0;\n        \
         accountRecordTypeId = accountRecordTypeId + 1;\n    }\n}\n";
    let dir = write_fixture_dir("name-range-local", &[("Widget.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let (id, symbol) = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::LocalVar && s.name == "accountRecordTypeId")
        .unwrap();
    assert_eq!(text_at(SRC, symbol.name_range), "accountRecordTypeId");

    let references: Vec<_> = program.references_to(id).collect();
    assert_eq!(references.len(), 2, "expected both usages on the next line");
    for ptr in references {
        let hr = program.highlight_range(ptr);
        assert_eq!(
            text_at(SRC, hr),
            "accountRecordTypeId",
            "reference highlight_range should be exactly the identifier, got {:?}",
            text_at(SRC, hr)
        );
    }
}

#[test]
fn a_type_references_highlight_range_excludes_trailing_trivia() {
    const SRC: &str = "public class Widget { }\n";
    const CALLER_SRC: &str = "public class Caller {\n    public void run() { Widget w = new Widget(); }\n}\n";
    let dir = write_fixture_dir(
        "name-range-type-ref",
        &[("Widget.cls", SRC), ("Caller.cls", CALLER_SRC)],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let (id, _) = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Widget")
        .unwrap();

    let caller_file = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Caller")
        .map(|(_, s)| s.file)
        .unwrap();

    // The bare `Widget` type reference (`Widget w = ...`), not the `new
    // Widget()` constructor call -- find it via the `NameExpr`-adjacent
    // `Type` node directly, since both references resolve to the same
    // `SymbolId` and `references_to` alone can't distinguish them.
    let root = program.syntax(caller_file);
    let type_ref_range = root
        .descendants()
        .filter(|n| n.kind() == apex_syntax::SyntaxKind::Type)
        .find(|n| n.text().to_string().trim() == "Widget")
        .map(|n| SyntaxPtr::new(caller_file, &n))
        .map(|ptr| program.highlight_range(ptr))
        .expect("expected a `Widget` Type reference in Caller.cls");
    assert_eq!(text_at(CALLER_SRC, type_ref_range), "Widget");

    // Sanity: the reverse index does reach both references (the `Type`
    // reference and the `new Widget()` `NewExpr`), still exactly ranged.
    let all_refs: Vec<_> = program.references_to(id).collect();
    assert_eq!(all_refs.len(), 2);
    for ptr in all_refs {
        assert_eq!(text_at(CALLER_SRC, program.highlight_range(ptr)), "Widget");
    }
}

/// A dead-simple end-to-end proof the fix actually prevents source
/// corruption: apply every returned reference/declaration range as a
/// literal string-splice rename (the same operation `rename_edits`
/// performs), and confirm the result is exactly the expected, correctly-
/// spaced new source -- not just "the right number of edits."
#[test]
fn applying_every_reference_range_as_a_splice_produces_correctly_spaced_source() {
    const SRC: &str = "public class Widget {\n    public void run() {\n        \
         Integer accountRecordTypeId = 0;\n        \
         accountRecordTypeId = accountRecordTypeId + 1;\n    }\n}\n";
    const EXPECTED: &str = "public class Widget {\n    public void run() {\n        \
         Integer x = 0;\n        \
         x = x + 1;\n    }\n}\n";
    let dir = write_fixture_dir("name-range-splice", &[("Widget.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let (id, symbol) = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::LocalVar && s.name == "accountRecordTypeId")
        .unwrap();

    let mut sites: Vec<rowan::TextRange> = vec![symbol.name_range];
    sites.extend(program.references_to(id).map(|ptr| program.highlight_range(ptr)));
    sites.sort_by_key(|r| std::cmp::Reverse(r.start()));

    let mut new_src = SRC.to_string();
    for range in sites {
        new_src.replace_range(usize::from(range.start())..usize::from(range.end()), "x");
    }
    assert_eq!(new_src, EXPECTED);
}
