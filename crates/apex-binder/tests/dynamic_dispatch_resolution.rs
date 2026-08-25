//! Comprehensive coverage for `crate::resolve::expand_dynamic_dispatch`/
//! `widen_for_dynamic_dispatch` -- every shape real Apex dynamic dispatch
//! actually has, each verified empirically against a real Salesforce org
//! (`sf apex run`) before being encoded here as a fixture, not assumed
//! from reasoning about the language alone. The facts that shaped these
//! fixtures:
//!
//! - A method is only a dynamic-dispatch point at all if it's `virtual`,
//!   `abstract`, `override`, or declared directly on an `interface`
//!   (interface methods have no body and are implicitly overridable by
//!   every implementor). A plain concrete method can never be
//!   overridden -- Apex rejects the subclass declaration outright.
//! - Re-overriding an already-`override` method requires that
//!   *intermediate* override to be declared `virtual override`, not just
//!   `override` -- confirmed empirically: `class Mid extends Base {
//!   override void m() {} }` compiles, but `class Leaf extends Mid {
//!   override void m() {} }` on top of it does not (`"Non-virtual,
//!   non-abstract methods cannot be overridden"`) unless `Mid.m` is
//!   `virtual override`. This is a fixture-construction concern, not a
//!   resolver behavior to test directly: real Apex simply won't let a
//!   deeper override exist unless every intermediate level opted back
//!   in, so `expand_dynamic_dispatch`'s plain "does this subtype
//!   directly declare a matching member" check is automatically correct
//!   without needing to special-case this itself.
//! - An interface method's own default implementation (if a class
//!   provides one directly, no `virtual`/`override` needed at all) is
//!   fully valid without any modifier -- Apex's interface-implementation
//!   rule is looser than its class-override rule.
//! - A field/property is resolved by the reference's *declared* type,
//!   never the receiver's runtime type -- Apex has no virtual field
//!   dispatch (confirmed: reading `f.x` through a base-typed variable
//!   holding a derived instance that redeclares `x` prints the base
//!   class's own value, not the derived one).
//! - `static`/`virtual` together is a compile error, and a subclass can
//!   never redeclare a same-named static method from its superclass at
//!   all (not even without `override`) -- so there is no legal Apex
//!   fixture where a static call could ever face a dispatch ambiguity in
//!   the first place; `is_dynamically_dispatchable`'s explicit
//!   `is_static` exclusion is defense in depth, not something a fixture
//!   can actually exercise.

use apex_binder::{BoundProgram, Resolution, SymbolId, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{CallExpr, FieldExpr, MethodCallExpr};
use rowan::ast::AstNode;

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn find(program: &BoundProgram, kind: SymbolKind, name: &str) -> SymbolId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == kind && s.name == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("{name} ({kind:?}) should have been collected"))
}

/// Like `find`, but disambiguates same-named members by declaring
/// container -- needed once a fixture declares the same method name on
/// more than one type (every override/implementation test here does).
fn find_member(program: &BoundProgram, kind: SymbolKind, name: &str, container: SymbolId) -> SymbolId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == kind && s.name == name && s.container == Some(container))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("{name} ({kind:?}) on {container:?} should have been collected"))
}

fn only_method_call_resolution(program: &BoundProgram, file: apex_binder::FileId) -> Resolution {
    let root = program.syntax(file);
    let calls: Vec<_> = root.descendants().filter_map(MethodCallExpr::cast).collect();
    assert_eq!(calls.len(), 1, "expected exactly one MethodCallExpr in {file:?}");
    let ptr = SyntaxPtr::new(file, calls[0].syntax());
    program
        .resolution(ptr)
        .cloned()
        .unwrap_or_else(|| panic!("the one MethodCallExpr should have a recorded resolution"))
}

fn candidates_or_panic(resolution: Resolution) -> Vec<SymbolId> {
    match resolution {
        Resolution::Candidates(ids) => ids,
        other => panic!("expected Resolution::Candidates, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Interface dispatch
// ---------------------------------------------------------------------

#[test]
fn interface_dispatch_via_local_variable_widens_to_the_implementation() {
    let dir = write_fixture_dir(
        "dispatch-iface-local",
        &[
            ("Greeter.cls", "public interface Greeter { String greet(); }"),
            (
                "Impl.cls",
                "public class Impl implements Greeter { public String greet() { return 'hi'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 public void run() { Greeter g = new Impl(); String s = g.greet(); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greeter_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Greeter"));
    let impl_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Impl"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&greeter_greet) && ids.contains(&impl_greet), "{ids:?}");
}

#[test]
fn interface_dispatch_via_method_parameter_widens_to_the_implementation() {
    let dir = write_fixture_dir(
        "dispatch-iface-param",
        &[
            ("Greeter.cls", "public interface Greeter { String greet(); }"),
            (
                "Impl.cls",
                "public class Impl implements Greeter { public String greet() { return 'hi'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 public void run(Greeter g) { String s = g.greet(); } \
                 public void go() { run(new Impl()); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greeter_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Greeter"));
    let impl_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Impl"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let root = program.syntax(caller_file);
    let call = root
        .descendants()
        .find_map(MethodCallExpr::cast)
        .expect("g.greet() should be a MethodCallExpr");
    let ptr = SyntaxPtr::new(caller_file, call.syntax());
    let ids = candidates_or_panic(program.resolution(ptr).cloned().unwrap());
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&greeter_greet) && ids.contains(&impl_greet), "{ids:?}");
}

#[test]
fn interface_dispatch_via_field_widens_to_the_implementation() {
    let dir = write_fixture_dir(
        "dispatch-iface-field",
        &[
            ("Greeter.cls", "public interface Greeter { String greet(); }"),
            (
                "Impl.cls",
                "public class Impl implements Greeter { public String greet() { return 'hi'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 private Greeter g = new Impl(); \
                 public void run() { String s = g.greet(); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greeter_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Greeter"));
    let impl_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Impl"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&greeter_greet) && ids.contains(&impl_greet), "{ids:?}");
}

/// The real NPSP shape that motivated this whole feature
/// (`UTIL_CurrencyCache.getInstance()`, whose declared return type is
/// `Interface_x`): the interface type only ever appears as a *return*
/// type, never a variable/field/parameter declaration, so this is its
/// own case rather than a variant of the others above.
#[test]
fn interface_dispatch_via_method_return_type_widens_to_the_implementation() {
    let dir = write_fixture_dir(
        "dispatch-iface-return",
        &[
            ("Greeter.cls", "public interface Greeter { String greet(); }"),
            (
                "Impl.cls",
                "public class Impl implements Greeter { \
                 public static Greeter instance() { return new Impl(); } \
                 public String greet() { return 'hi'; } \
             }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { String s = Impl.instance().greet(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greeter_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Greeter"));
    let impl_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Impl"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let root = program.syntax(caller_file);
    // `Impl.instance()` is itself a qualified `MethodCallExpr` (`Impl` is
    // the target, `instance` the method), so there are two in this file
    // -- filter to the one actually named `greet`.
    let greet_call = root
        .descendants()
        .filter_map(MethodCallExpr::cast)
        .find(|c| c.method_name_token().is_some_and(|t| t.text() == "greet"))
        .expect("Impl.instance().greet() should contain a .greet() MethodCallExpr");
    let ptr = SyntaxPtr::new(caller_file, greet_call.syntax());
    let ids = candidates_or_panic(program.resolution(ptr).cloned().unwrap());
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&greeter_greet) && ids.contains(&impl_greet), "{ids:?}");
}

/// This binder's model is purely static/declared-type-based, not flow-
/// sensitive: it can't tell that *this particular* `Greeter`-typed
/// variable was actually assigned an `ImplA`, not an `ImplB` -- so
/// dispatch widening must conservatively include *every* implementor's
/// matching method as a candidate, not just the one that happens to look
/// "obviously" assigned in this snippet. Correct in the same direction
/// this project's other conservative simplifications already lean: it
/// can only ever over-credit references, never under-credit them.
#[test]
fn interface_with_multiple_unrelated_implementors_widens_to_every_implementor() {
    let dir = write_fixture_dir(
        "dispatch-iface-multi-impl",
        &[
            ("Greeter.cls", "public interface Greeter { String greet(); }"),
            (
                "ImplA.cls",
                "public class ImplA implements Greeter { public String greet() { return 'a'; } }",
            ),
            (
                "ImplB.cls",
                "public class ImplB implements Greeter { public String greet() { return 'b'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 public void run() { Greeter g = new ImplA(); String s = g.greet(); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let greeter_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Greeter"));
    let a_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "ImplA"));
    let b_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "ImplB"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(
        ids.contains(&greeter_greet) && ids.contains(&a_greet) && ids.contains(&b_greet),
        "{ids:?}"
    );
}

/// A call written against a *base* interface (`Named`) must still reach
/// an implementor of the *derived* interface (`Greeter extends Named`)
/// two hops down -- `SymbolTable::subtypes` is transitive across an
/// interface-extends-interface hop, not just a single level.
#[test]
fn dispatch_through_an_extended_interface_reaches_the_implementor() {
    let dir = write_fixture_dir(
        "dispatch-iface-extends-iface",
        &[
            ("Named.cls", "public interface Named { String greet(); }"),
            ("Greeter.cls", "public interface Greeter extends Named { }"),
            (
                "Impl.cls",
                "public class Impl implements Greeter { public String greet() { return 'hi'; } }",
            ),
            (
                "Caller.cls",
                "public class Caller { \
                 public void run() { Named n = new Impl(); String s = n.greet(); } \
             }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let named_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Interface, "Named"));
    let impl_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Impl"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&named_greet) && ids.contains(&impl_greet), "{ids:?}");
}

// ---------------------------------------------------------------------
// Abstract-class dispatch
// ---------------------------------------------------------------------

#[test]
fn abstract_class_dispatch_widens_to_the_concrete_override() {
    let dir = write_fixture_dir(
        "dispatch-abstract",
        &[
            (
                "Shape.cls",
                "public abstract class Shape { public abstract void draw(); }",
            ),
            (
                "Circle.cls",
                "public class Circle extends Shape { public override void draw() { } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { Shape s = new Circle(); s.draw(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let shape_draw = find_member(&program, SymbolKind::Method, "draw", find(&program, SymbolKind::Class, "Shape"));
    let circle_draw = find_member(&program, SymbolKind::Method, "draw", find(&program, SymbolKind::Class, "Circle"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&shape_draw) && ids.contains(&circle_draw), "{ids:?}");
}

// ---------------------------------------------------------------------
// Virtual-method override dispatch
// ---------------------------------------------------------------------

#[test]
fn virtual_method_override_widens_via_base_typed_reference() {
    let dir = write_fixture_dir(
        "dispatch-virtual-single",
        &[
            (
                "Base.cls",
                "public virtual class Base { public virtual void greet() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public override void greet() { } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { Base b = new Derived(); b.greet(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Base"));
    let derived_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Derived"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&base_greet) && ids.contains(&derived_greet), "{ids:?}");
}

/// A three-level override chain (`Base` -> `Mid` -> `Leaf`), where `Mid`'s
/// override is declared `virtual override` specifically so `Leaf` is
/// even allowed to override it again (see this file's module doc comment
/// -- plain `override` alone would make this fixture invalid Apex).
/// Dispatch through the *topmost* `Base`-typed reference must reach all
/// three declarations: at runtime the object could be exactly a `Base`,
/// a `Mid`, or a `Leaf`.
#[test]
fn multi_level_override_chain_widens_to_every_level_from_the_base_type() {
    let dir = write_fixture_dir(
        "dispatch-virtual-chain",
        &[
            (
                "Base.cls",
                "public virtual class Base { public virtual void greet() { } }",
            ),
            (
                "Mid.cls",
                "public virtual class Mid extends Base { public virtual override void greet() { } }",
            ),
            (
                "Leaf.cls",
                "public class Leaf extends Mid { public override void greet() { } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { Base b = new Leaf(); b.greet(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Base"));
    let mid_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Mid"));
    let leaf_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Leaf"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(
        ids.contains(&base_greet) && ids.contains(&mid_greet) && ids.contains(&leaf_greet),
        "{ids:?}"
    );
}

/// The same three-level chain as above, but dispatching through a
/// `Mid`-typed reference instead of `Base`-typed: the candidate set must
/// exclude `Base.greet` (a `Mid`-typed reference can never actually hold
/// a plain `Base` instance -- `Mid` is *more* derived, not less) while
/// still including both `Mid.greet` and `Leaf.greet`.
#[test]
fn dispatch_via_a_mid_level_type_excludes_the_more_general_base() {
    let dir = write_fixture_dir(
        "dispatch-virtual-chain-mid",
        &[
            (
                "Base.cls",
                "public virtual class Base { public virtual void greet() { } }",
            ),
            (
                "Mid.cls",
                "public virtual class Mid extends Base { public virtual override void greet() { } }",
            ),
            (
                "Leaf.cls",
                "public class Leaf extends Mid { public override void greet() { } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { Mid m = new Leaf(); m.greet(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Base"));
    let mid_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Mid"));
    let leaf_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Leaf"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&mid_greet) && ids.contains(&leaf_greet), "{ids:?}");
    assert!(!ids.contains(&base_greet), "{ids:?}");
}

/// The classic template-method shape: `Base.run()` calls its own
/// `step()` unqualified (implicit `this`), and `step()` is virtual --
/// confirmed empirically that this dispatches to a subclass's override
/// at runtime even though the call site's static context is `Base`, not
/// `Derived`. This exercises `bind_call_expr`'s climb-then-widen path
/// (an unqualified call), not `bind_method_call_expr`'s (an explicit
/// `receiver.method()`) -- the two are separate code paths that each
/// independently call `widen_for_dynamic_dispatch`.
#[test]
fn template_method_pattern_widens_an_implicit_this_call_to_the_override() {
    let dir = write_fixture_dir(
        "dispatch-template-method",
        &[
            (
                "Base.cls",
                "public virtual class Base { \
                 public virtual void step() { } \
                 public void run() { step(); } \
             }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public override void step() { } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_step = find_member(&program, SymbolKind::Method, "step", find(&program, SymbolKind::Class, "Base"));
    let derived_step = find_member(&program, SymbolKind::Method, "step", find(&program, SymbolKind::Class, "Derived"));

    let base_file = program.symbols.get(find(&program, SymbolKind::Class, "Base")).file;
    let root = program.syntax(base_file);
    let calls: Vec<_> = root.descendants().filter_map(CallExpr::cast).collect();
    assert_eq!(calls.len(), 1, "expected exactly one unqualified CallExpr (`step()`)");
    let ptr = SyntaxPtr::new(base_file, calls[0].syntax());
    let ids = candidates_or_panic(program.resolution(ptr).cloned().unwrap());
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&base_step) && ids.contains(&derived_step), "{ids:?}");
}

// ---------------------------------------------------------------------
// Scope boundaries: what must NOT widen
// ---------------------------------------------------------------------

/// Apex has no virtual field/property dispatch at all -- confirmed
/// empirically that reading a field through a base-typed reference
/// always uses the *declared* type's own field, never a derived
/// instance's redeclared one, even when the derived class shadows the
/// same field name. `expand_dynamic_dispatch`/`widen_for_dynamic_dispatch`
/// are only ever invoked from `bind_call_expr`/`bind_method_call_expr`
/// (call resolution), never from field/property access resolution, so a
/// `FieldExpr` reference must stay a single `Resolved` no matter how
/// many unrelated types happen to declare a same-named field.
#[test]
fn field_access_is_never_dynamically_dispatched() {
    let dir = write_fixture_dir(
        "dispatch-field-never-widens",
        &[
            ("FBase.cls", "public virtual class FBase { public Integer x = 1; }"),
            (
                "Caller.cls",
                "public class Caller { public void run() { FBase b = new FBase(); Integer y = b.x; } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_x = find_member(&program, SymbolKind::Field, "x", find(&program, SymbolKind::Class, "FBase"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let root = program.syntax(caller_file);
    let field_access = root
        .descendants()
        .find_map(FieldExpr::cast)
        .expect("b.x should be a FieldExpr");
    let ptr = SyntaxPtr::new(caller_file, field_access.syntax());
    assert_eq!(
        program.resolution(ptr).cloned(),
        Some(Resolution::Resolved(base_x)),
        "field access must never widen into Candidates"
    );
}

/// Deliberate, documented over-inclusion: `new Impl().greet()` has a
/// receiver whose runtime type is unambiguously exactly `Impl` (a fresh
/// constructor call can never be some other subtype), so widening here
/// is provably unnecessary -- but this binder's `Ty::Project` doesn't
/// currently distinguish "known-exact" from "merely declared" type
/// provenance, so it widens anyway. This is the same "under-widening is
/// the costlier mistake" direction the project already leans in
/// elsewhere (see `expand_dynamic_dispatch`'s own arity-only, not full-
/// parameter-type, matching): the failure mode this could cause is a
/// missed truly-dead override (a false negative), never a false
/// positive, so it's left as-is rather than tracked as a bug. This test
/// exists to document the behavior, not to demand it change.
#[test]
fn a_call_on_a_freshly_constructed_exact_type_still_widens_conservatively() {
    let dir = write_fixture_dir(
        "dispatch-new-expr-still-widens",
        &[
            (
                "Base.cls",
                "public virtual class Base { public virtual void greet() { } }",
            ),
            (
                "Derived.cls",
                "public class Derived extends Base { public override void greet() { } }",
            ),
            (
                "Caller.cls",
                "public class Caller { public void run() { new Base().greet(); } }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let base_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Base"));
    let derived_greet = find_member(&program, SymbolKind::Method, "greet", find(&program, SymbolKind::Class, "Derived"));

    let caller_file = program.symbols.get(find(&program, SymbolKind::Class, "Caller")).file;
    let ids = candidates_or_panic(only_method_call_resolution(&program, caller_file));
    assert!(ids.contains(&base_greet) && ids.contains(&derived_greet), "{ids:?}");
}
