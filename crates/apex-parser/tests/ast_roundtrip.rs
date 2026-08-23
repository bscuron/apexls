//! Proves the full pipeline is still lossless after the typed AST layer
//! landed: source -> tokens -> parse -> **cast through
//! `apex_syntax::ast`** -> render -> source, byte-for-byte, over every
//! real NPSP compilation unit. `roundtrip.rs` already proves this at the
//! raw-`SyntaxNode` level for extracted statement fragments; this proves
//! it at the *whole-file* level and explicitly through the typed AST
//! wrappers (`CompilationUnit`/`TriggerUnit`), not just the parser's own
//! `Parse::syntax()` -- confirming the AST layer really is a zero-cost,
//! read-only view (`AstNode::syntax()` returns the identical
//! `SyntaxNode`/`GreenNode` the parser built, not a copy), so nothing
//! about adding `DeclName` nodes or typed accessors on top changed what
//! gets rendered back out.

use apex_syntax::ast::decl::{CompilationUnit, TriggerUnit};
use apex_syntax::AstNode;

fn is_known_non_compilation_unit(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/scripts/") || s.ends_with("datasets/rd2/config_npsp_for_ldv_data_load.cls")
}

#[test]
fn every_real_npsp_file_round_trips_through_the_ast_layer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let files = apex_discover::find_apex_files(&root);
    assert!(
        !files.is_empty(),
        "expected the NPSP submodule to be checked out"
    );

    let mut checked = 0usize;
    for path in &files {
        if is_known_non_compilation_unit(path) {
            continue;
        }
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let is_trigger = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));

        let rendered = if is_trigger {
            let parse = apex_parser::parse_trigger_unit(&src);
            let trigger = TriggerUnit::cast(parse.syntax())
                .unwrap_or_else(|| panic!("{}: root isn't a TriggerUnit", path.display()));
            apex_printer::render(trigger.syntax())
        } else {
            let parse = apex_parser::parse_compilation_unit(&src);
            let cu = CompilationUnit::cast(parse.syntax())
                .unwrap_or_else(|| panic!("{}: root isn't a CompilationUnit", path.display()));
            apex_printer::render(cu.syntax())
        };

        assert_eq!(
            rendered,
            src,
            "{}: did not round-trip byte-for-byte through the AST layer",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked > 1000,
        "expected to check over 1000 real NPSP files, only checked {checked}"
    );
}
