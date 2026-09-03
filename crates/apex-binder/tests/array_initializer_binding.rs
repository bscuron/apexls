//! Regression test for `crate::resolve::BodyBinder::bind_initializer_expr`'s
//! `Initializer::Array` arm (`new Integer[]{1, 2, x}`) -- unlike its
//! `Set`/`Map` siblings, no existing test anywhere in this suite ever
//! bound an array-literal initializer's elements, so a reference inside
//! one (`x` above) was never actually confirmed to resolve.

use apex_binder::{BoundProgram, Resolution, SymbolKind};
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

fn file_for_class(program: &BoundProgram, class_name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

#[test]
fn a_local_referenced_inside_an_array_literal_initializer_resolves() {
    const SRC: &str = "public class Foo { \
        public void run() { \
            Integer count = 5; \
            Integer[] nums = new Integer[]{1, 2, count}; \
        } \
    }";
    let dir = write_fixture_dir("array-initializer", &[("Foo.cls", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let count_sym = program
        .symbols
        .iter()
        .find(|(_, s)| s.file == file && s.kind == SymbolKind::LocalVar && s.name == "count")
        .map(|(id, _)| id)
        .expect("local `count` should have been collected");

    let root = program.syntax(file);
    // Two `count` NameExprs exist: the declaration's own initializer
    // (`= 5`, no reference there) and the read inside the array literal
    // -- only the latter is a `NameExpr` at all.
    let reads: Vec<_> = root
        .descendants()
        .filter_map(NameExpr::cast)
        .filter(|n| n.name_token().is_some_and(|t| t.text() == "count"))
        .collect();
    assert_eq!(reads.len(), 1, "expected exactly one `count` read: {reads:?}");
    let ptr = apex_binder::SyntaxPtr::new(file, reads[0].syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(count_sym)),
        "the array literal's own element should resolve to the local `count`"
    );
}
