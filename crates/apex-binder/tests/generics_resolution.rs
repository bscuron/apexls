//! Fixtures verifying `BACKLOG.md` §4's generics-aware resolution
//! (`crate::generics::builtin_generic_member_type`, `crate::ty::Ty`): a
//! `List<Account>`'s element type must substitute through `.get(...)` so
//! a further qualified access on the result can still resolve, even
//! though the `List.get` call itself has no real declaration to point at.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
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
fn a_list_elements_type_substitutes_through_get_so_a_further_field_access_resolves() {
    let dir = write_fixture_dir(
        "generics-list-get",
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

    let count_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "count")
        .map(|(id, _)| id)
        .expect("Widget.count should have been collected");

    let widget_file = file_for_class(&program, "Widget");
    let root = program.syntax(widget_file);
    let field_ptr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::FieldExpr::cast)
        .map(|f| SyntaxPtr::new(widget_file, f.syntax()))
        .expect("items.get(0).count should be a FieldExpr");
    assert_eq!(
        program.resolution(field_ptr).cloned(),
        Some(Resolution::Resolved(count_id)),
        "List<Widget>.get(0)'s inferred element type should let `.count` resolve"
    );
}

#[test]
fn map_keyset_infers_a_set_of_the_key_type() {
    // Not directly observable through `Resolution` (`Set<String>.` has no
    // further project-local member to chain into here), but this proves
    // `bind_method_call_expr` doesn't panic or mis-set the *call's own*
    // resolution when walking a `Map` target -- the call itself has no
    // real declaration, so it must stay `Unresolved`, not `Resolved`.
    let dir = write_fixture_dir(
        "generics-map-keyset",
        &[(
            "Widget.cls",
            "public class Widget { \
             public void run() { \
                 Map<String, Widget> byName = new Map<String, Widget>(); \
                 Set<String> names = byName.keySet(); \
             } \
         }",
        )],
    );

    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_file = file_for_class(&program, "Widget");
    let root = program.syntax(widget_file);
    let call_ptr = root
        .descendants()
        .find_map(apex_syntax::ast::expr::MethodCallExpr::cast)
        .map(|c| SyntaxPtr::new(widget_file, c.syntax()))
        .expect("byName.keySet() should be a MethodCallExpr");
    assert_eq!(
        program.resolution(call_ptr).cloned(),
        Some(Resolution::Unresolved),
        "a built-in generic method call has no real declaration to resolve to"
    );
}
