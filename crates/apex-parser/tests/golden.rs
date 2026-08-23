//! Structural golden tests: snapshot the parsed tree *shape*, not just
//! its rendered text. This catches a class of bug the round-trip test
//! (`tests/roundtrip.rs`) structurally cannot: wrong precedence/nesting
//! (e.g. `a + b * c` parsed as `(a + b) * c`) still renders back to
//! identical text, since rendering just concatenates token text
//! regardless of tree shape.
//!
//! Fixtures live in the repo-root `tests/golden/{expressions,statements,
//! malformed}/` directories (shared with the rest of the verification
//! plan, not apex-parser-private), one minimal snippet per file. Run
//! `INSTA_UPDATE=always cargo test -p apex-parser --test golden` to
//! (re)generate the accepted `.snap` baselines after an intentional
//! grammar change; a plain `cargo test` fails on any unreviewed diff.

use apex_parser::Parse;
use std::fmt::Write as _;
use std::path::Path;

fn snapshot_text(src: &str, parse: &Parse) -> String {
    let mut out = String::new();
    writeln!(out, "# source\n{src}").unwrap();
    writeln!(out, "\n# tree\n{:#?}", parse.syntax()).unwrap();
    if !parse.errors.is_empty() {
        writeln!(out, "# errors").unwrap();
        for e in &parse.errors {
            writeln!(out, "{e:?}").unwrap();
        }
    }
    out
}

fn read_fixture(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn expressions() {
    insta::glob!("../../../tests/golden/expressions", "*.cls", |path| {
        let src = read_fixture(path);
        let parse = apex_parser::parse_expression(&src);
        insta::assert_snapshot!(snapshot_text(&src, &parse));
    });
}

#[test]
fn statements() {
    insta::glob!("../../../tests/golden/statements", "*.cls", |path| {
        let src = read_fixture(path);
        let parse = apex_parser::parse_statement(&src);
        insta::assert_snapshot!(snapshot_text(&src, &parse));
    });
}

/// Deliberately invalid inputs: snapshots both the best-effort tree *and*
/// the collected errors, directly validating `tests/golden/README.md`'s
/// stated recovery contract ("the rest of the file should still produce
/// a best-effort tree").
#[test]
fn malformed() {
    insta::glob!("../../../tests/golden/malformed", "*.cls", |path| {
        let src = read_fixture(path);
        let parse = apex_parser::parse_statement(&src);
        assert!(
            !parse.errors.is_empty(),
            "{path:?} was expected to be malformed but parsed cleanly"
        );
        insta::assert_snapshot!(snapshot_text(&src, &parse));
    });
}
