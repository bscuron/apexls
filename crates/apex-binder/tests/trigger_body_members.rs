//! Regression test for a real, if rare, Apex feature: a `.trigger` file's
//! body can declare its own helper members directly (any member with an
//! explicit leading modifier, or `void`/`class`/`interface`/`enum` --
//! see `grammar::declarations::trigger_block_member`'s own doc comment
//! on why only those route into member-declaration parsing there rather
//! than `statement`). No existing test anywhere in this suite ever
//! exercised `crate::collect::collect_trigger_unit`'s own
//! `block.members()` loop, so a member declared inside a trigger body
//! was never actually confirmed to get collected at all.

use apex_binder::{BoundProgram, SymbolKind};
use apex_syntax::ast::expr::CallExpr;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

#[test]
fn a_helper_method_declared_directly_in_a_trigger_body_is_collected_and_callable() {
    const SRC: &str = "trigger FooTrigger on Account (before insert) { \
        helper(); \
        static void helper() { \
            System.debug('hi'); \
        } \
    }";
    let dir = write_fixture_dir("trigger-body-member", &[("FooTrigger.trigger", SRC)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let trigger_sym = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Trigger && s.name == "FooTrigger")
        .map(|(id, _)| id)
        .expect("expected the trigger itself to be collected");

    let helper_sym = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "helper")
        .map(|(id, s)| (id, s.container))
        .expect("expected the trigger body's own `helper` method to be collected");
    assert_eq!(
        helper_sym.1,
        Some(trigger_sym),
        "the helper method's container should be the trigger itself"
    );

    // And it must actually be *callable* -- the bare `helper()` call
    // statement earlier in the trigger body should resolve to it, not
    // stay Unresolved.
    let file = program
        .files()
        .find(|&f| program.file_path(f).ends_with("FooTrigger.trigger"))
        .expect("expected the trigger file to be discovered");
    let root = program.syntax(file);
    let call = root
        .descendants()
        .find_map(CallExpr::cast)
        .expect("expected a bare `helper()` CallExpr");
    let ptr = apex_binder::SyntaxPtr::new(file, call.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(apex_binder::Resolution::Resolved(helper_sym.0)),
        "the trigger-body call site should resolve to its own sibling helper method"
    );
}
