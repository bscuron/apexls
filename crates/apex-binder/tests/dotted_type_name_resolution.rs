//! Regression coverage for `SymbolTable::resolve_dotted_name`: a plain
//! (already generics/array-suffix-stripped) dotted type-name *string* --
//! as opposed to a `Type` AST node's own tokens, which
//! `crate::resolve::resolve_dotted_top_level` already handled correctly
//! -- needs the identical segment-by-segment (`top_level` then
//! `nested_type`) walk everywhere a `Symbol::type_name` or collected
//! `extends`/`implements` name is looked up. Before this fix, several
//! call sites (`crate::resolve::type_of_symbol` chief among them) called
//! `SymbolTable::top_level` directly on the whole dotted string, which
//! -- `top_level` being keyed by simple declared name only -- silently
//! failed for any qualified nested-type name, project-wide, not just the
//! `extends`/`implements` case `crate::inherit` already had its own fix
//! for.
//!
//! Real user report this covers: NPSP's `UTIL_CurrencyCache.CurrencyData
//! currData = ...;`, declared *inside* `UTIL_CurrencyCache` itself, whose
//! `currData.IsoCode = ...`/`currData.defaultRate = ...` assignments
//! never resolved -- `currData`'s own `type_name` was the whole dotted
//! string `"UTIL_CurrencyCache.CurrencyData"`, which `type_of_symbol`'s
//! plain `top_level` lookup could never match, so every field access off
//! `currData` fell back to `Unresolved`, and `IsoCode`/`defaultRate`
//! showed zero references despite being genuinely written to.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{CallExpr, FieldExpr};
use apex_syntax::ast::QualifiedName;
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

/// The exact real-world shape: a local variable declared with its own
/// enclosing class's dotted-qualified nested type, then a field written
/// through it -- must resolve, not fall back to `Unresolved`.
#[test]
fn a_local_variable_declared_with_a_self_referencing_dotted_nested_type_resolves_field_writes() {
    let dir = write_fixture_dir(
        "dotted-local-var-self-nested",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { public String x; } \
             public void run() { \
                 Outer.Inner v = new Outer.Inner(); \
                 v.x = 'hi'; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let inner_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Inner")
        .map(|(id, _)| id)
        .expect("Inner should have been collected");
    let x_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "x" && s.container == Some(inner_id))
        .map(|(id, _)| id)
        .expect("Inner.x should have been collected");

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let field_access = root
        .descendants()
        .filter_map(FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "x"))
        .expect("v.x should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(x_id)),
        "v.x must resolve to Inner.x, not stay Unresolved"
    );
}

/// The same dotted-nested-type shape, but on a `Field` symbol's own
/// declared type rather than a `LocalVar`'s -- `type_of_symbol` is
/// shared machinery across every declared-type-carrying `SymbolKind`, so
/// a `Field` must benefit from the same fix independently.
#[test]
fn a_class_field_declared_with_a_self_referencing_dotted_nested_type_resolves_member_access() {
    let dir = write_fixture_dir(
        "dotted-field-self-nested",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { public String x; } \
             private Outer.Inner cached = new Outer.Inner(); \
             public void run() { cached.x = 'hi'; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let inner_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Inner")
        .map(|(id, _)| id)
        .expect("Inner should have been collected");
    let x_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "x" && s.container == Some(inner_id))
        .map(|(id, _)| id)
        .expect("Inner.x should have been collected");

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let field_access = root
        .descendants()
        .filter_map(FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "x"))
        .expect("cached.x should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(x_id)),
        "cached.x must resolve to Inner.x, not stay Unresolved"
    );
}

/// A method *parameter* declared with a dotted nested-type reference --
/// the same fix must apply uniformly across every declared-type-carrying
/// symbol kind, not just fields/locals.
#[test]
fn a_parameter_declared_with_a_self_referencing_dotted_nested_type_resolves_member_access() {
    let dir = write_fixture_dir(
        "dotted-param-self-nested",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { public String x; } \
             public void handle(Outer.Inner v) { v.x = 'hi'; } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let inner_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Inner")
        .map(|(id, _)| id)
        .expect("Inner should have been collected");
    let x_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "x" && s.container == Some(inner_id))
        .map(|(id, _)| id)
        .expect("Inner.x should have been collected");

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let field_access = root
        .descendants()
        .filter_map(FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "x"))
        .expect("v.x should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(x_id)),
        "v.x must resolve to Inner.x, not stay Unresolved"
    );
}

/// A generic collection's own type *argument* can itself be a dotted
/// nested-type reference (`List<Outer.Inner>`) -- `type_of_symbol`'s
/// `type_args` mapping needs the same dotted-aware resolution as the
/// base `type_name` does, not a plain `top_level` lookup, so element
/// access still chains through to a `Ty::Project`.
#[test]
fn a_generic_type_argument_that_is_a_dotted_nested_type_still_resolves_element_access() {
    let dir = write_fixture_dir(
        "dotted-generic-arg-self-nested",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { public String x; } \
             public void run() { \
                 List<Outer.Inner> items = new List<Outer.Inner>(); \
                 items.get(0).x = 'hi'; \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let inner_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Inner")
        .map(|(id, _)| id)
        .expect("Inner should have been collected");
    let x_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Field && s.name == "x" && s.container == Some(inner_id))
        .map(|(id, _)| id)
        .expect("Inner.x should have been collected");

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let field_access = root
        .descendants()
        .filter_map(FieldExpr::cast)
        .find(|f| f.member_token().is_some_and(|t| t.text() == "x"))
        .expect("items.get(0).x should be a FieldExpr");
    let ptr = SyntaxPtr::new(outer_file, field_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(x_id)),
        "items.get(0).x must resolve to Inner.x through the generic type argument"
    );
}

/// `catch (Outer.InnerException e)` -- a dotted, self-nested custom
/// exception type -- must resolve the same way a plain top-level
/// exception class name already did.
#[test]
fn a_catch_clauses_dotted_nested_exception_type_resolves() {
    let dir = write_fixture_dir(
        "dotted-catch-exception-self-nested",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class InnerException extends Exception { } \
             public void run() { \
                 try { \
                     Integer x = 1; \
                 } catch (Outer.InnerException e) { \
                     System.debug(e); \
                 } \
             } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let inner_ex_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "InnerException")
        .map(|(id, _)| id)
        .expect("InnerException should have been collected");

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let exception_type_node = root
        .descendants()
        .find_map(QualifiedName::cast)
        .expect("the catch clause's exception type should be a QualifiedName node");
    let ptr = SyntaxPtr::new(outer_file, exception_type_node.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(inner_ex_id)),
        "the catch clause's dotted nested exception type must resolve"
    );
}

/// Overload narrowing (`crate::conversions`) must recognize a
/// dotted-nested-type parameter as an exact match for an argument of
/// that same type -- previously `type_compatible`'s `table.top_level(param_name)`
/// call could never find a qualified parameter type, silently treating
/// it as an unmodeled system type instead of the real project-local
/// match it is.
#[test]
fn overload_resolution_recognizes_a_dotted_nested_type_parameter_as_an_exact_match() {
    let dir = write_fixture_dir(
        "dotted-overload-param",
        &[(
            "Outer.cls",
            "public class Outer { \
             public class Inner { } \
             public void handle(Outer.Inner v) { } \
             public void handle(Integer i) { } \
             public void run() { Outer.Inner v = new Outer.Inner(); handle(v); } \
         }",
        )],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let outer_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == "Outer")
        .map(|(id, _)| id)
        .expect("Outer should have been collected");
    let handle_inner = program
        .symbols
        .iter()
        .filter(|(_, s)| s.kind == SymbolKind::Method && s.name == "handle" && s.container == Some(outer_id))
        .map(|(id, _)| id)
        .find(|&id| {
            let params = program.symbols.params(id);
            params.len() == 1 && program.symbols.get(params[0]).type_name.as_deref() == Some("Outer.Inner")
        })
        .expect("handle(Outer.Inner) should have been collected");

    let outer_file = program.symbols.get(outer_id).file;
    let root = program.syntax(outer_file);
    let call = root
        .descendants()
        .find_map(CallExpr::cast)
        .expect("handle(v) should be a CallExpr");
    let ptr = SyntaxPtr::new(outer_file, call.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(handle_inner)),
        "handle(v) must resolve unambiguously to handle(Outer.Inner), eliminating handle(Integer) \
         by type -- not stay ambiguous Candidates because the parameter's dotted type name never \
         matched a project-local type at all"
    );
}
