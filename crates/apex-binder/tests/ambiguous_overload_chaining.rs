//! Regression coverage for `BodyBinder::result_type_of`: a method call
//! that resolves to `Resolution::Candidates` (genuinely ambiguous under
//! this binder's conservative overload-narrowing rules) must still
//! propagate a `Ty` for anything chained off it, as long as every
//! surviving candidate happens to declare the *identical* return type --
//! which an overloaded fluent-builder method very often does.
//!
//! Real user report this covers: NPSP's `UTIL_Finder.withSearchQuery`
//! flagged dead despite being called (via `new UTIL_Finder(...)
//! .withSelectFields(new List<String>(...)).withSearchQuery(...)`) --
//! `withSelectFields` has three overloads (`Set<Schema.sObjectField>`,
//! `List<String>`, `List<FieldSetMember>`), and `crate::conversions`'s
//! curated type model has no rule for the unmodeled system type
//! `Schema.FieldSetMember`, so it can't positively rule out the
//! `List<FieldSetMember>` overload for a `List<String>` argument --
//! genuinely ambiguous by this binder's own conservative "can't prove
//! wrong never eliminates" rule, even though a real compiler resolves it
//! immediately. Before this fix, that ambiguity alone (`Resolution::Candidates`)
//! discarded the call's type entirely, so `.withSearchQuery(...)` chained
//! after it always fell to `Unresolved`, regardless of how unambiguous
//! `withSearchQuery` itself was.
//!
//! This file reproduces the same *shape* with a self-contained fixture
//! (an `Object`-typed argument against a `String`/`Integer` overload
//! pair -- `Object` is outside `crate::conversions`'s curated-mismatch
//! rule the same way `Schema.FieldSetMember` is, so it triggers the
//! identical "can't eliminate, stays Candidates" path) rather than
//! depending on Salesforce schema modeling specifics.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{CallExpr, MethodCallExpr};
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
fn a_call_chained_off_an_ambiguous_but_same_return_type_overload_still_resolves() {
    let dir = write_fixture_dir(
        "ambiguous-chain-same-return",
        &[(
            "Widget.cls",
            "public class Widget { \
             public Widget withValue(String x) { return this; } \
             public Widget withValue(Integer x) { return this; } \
             public Widget chained() { return this; } \
             public void run(Object obj) { \
                 new Widget().withValue(obj).chained(); \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Widget")
        .map(|(id, _)| id)
        .expect("Widget should have been collected");
    let with_value_overloads: Vec<_> = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "withValue" && s.container == Some(widget_id))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(with_value_overloads.len(), 2, "expected both withValue overloads collected");
    let chained_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Method && s.name == "chained" && s.container == Some(widget_id))
        .map(|(id, _)| id)
        .expect("Widget.chained should have been collected");

    let widget_file = program.symbols.get(widget_id).file;
    let root = program.syntax(widget_file);

    let with_value_call = root
        .descendants()
        .filter_map(MethodCallExpr::cast)
        .find(|c| c.method_name_token().is_some_and(|t| t.text() == "withValue"))
        .expect("withValue(obj) should be a MethodCallExpr");
    let with_value_ptr = SyntaxPtr::new(widget_file, with_value_call.syntax());
    match program.resolution(with_value_ptr).cloned() {
        Some(Resolution::Candidates(ids)) => {
            assert_eq!(ids.len(), 2, "expected withValue(obj) to stay genuinely ambiguous: {ids:?}");
            assert!(
                with_value_overloads.iter().all(|id| ids.contains(id)),
                "expected both withValue overloads as candidates: {ids:?}"
            );
        }
        other => panic!(
            "expected withValue(obj) to resolve Candidates (an Object argument can't rule out \
             either the String or Integer overload) -- got {other:?}, so this fixture no longer \
             exercises the ambiguous-overload case this test is about"
        ),
    }

    let chained_call = root
        .descendants()
        .filter_map(MethodCallExpr::cast)
        .find(|c| c.method_name_token().is_some_and(|t| t.text() == "chained"))
        .expect("chained() should be a MethodCallExpr");
    let chained_ptr = SyntaxPtr::new(widget_file, chained_call.syntax());
    assert_eq!(
        program.resolution(chained_ptr).cloned(),
        Some(Resolution::Resolved(chained_id)),
        "chained() must still resolve even though the preceding withValue(obj) call in the same \
         chain was ambiguous -- both withValue overloads return Widget, so the call's type \
         should still propagate"
    );
}

/// The contrasting negative case: when an ambiguous call's surviving
/// candidates *disagree* on return type, nothing chained after it may
/// resolve -- `result_type_of` must never guess one of the disagreeing
/// types, only ever propagate a type all candidates share.
#[test]
fn a_call_chained_off_an_ambiguous_overload_with_differing_return_types_stays_unresolved() {
    let dir = write_fixture_dir(
        "ambiguous-chain-differing-return",
        &[(
            "Widget.cls",
            "public class Widget { \
             public Widget withValueW(String x) { return this; } \
             public Gadget withValueW(Integer x) { return new Gadget(); } \
             public void run(Object obj) { \
                 withValueW(obj).chained(); \
             } \
         }",
        ),
        (
            "Gadget.cls",
            "public class Gadget { public void chained() { } }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let widget_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Widget")
        .map(|(id, _)| id)
        .expect("Widget should have been collected");
    let widget_file = program.symbols.get(widget_id).file;
    let root = program.syntax(widget_file);

    let with_value_call = root
        .descendants()
        .find_map(CallExpr::cast)
        .expect("withValueW(obj) should be an unqualified CallExpr");
    let with_value_ptr = SyntaxPtr::new(widget_file, with_value_call.syntax());
    let Some(Resolution::Candidates(ids)) = program.resolution(with_value_ptr).cloned() else {
        panic!("expected withValueW(obj) to stay Candidates -- fixture no longer exercises this case");
    };
    assert_eq!(ids.len(), 2, "{ids:?}");

    let chained_call = root
        .descendants()
        .find_map(MethodCallExpr::cast)
        .expect("chained() should be a MethodCallExpr");
    let chained_ptr = SyntaxPtr::new(widget_file, chained_call.syntax());
    assert_eq!(
        program.resolution(chained_ptr).cloned(),
        Some(Resolution::Unresolved),
        "chained() must stay Unresolved -- the two withValueW overloads return different types \
         (Widget vs Gadget), so there's no single shared type to propagate, and guessing either \
         one would be wrong for the other"
    );
}
