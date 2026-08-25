//! A precise, low-noise regression guard for exactly the bug shape this
//! session hit three times independently (dotted nested-type names,
//! interface/virtual dynamic dispatch, ambiguous-overload-chain type
//! propagation): a `MethodCallExpr`/`FieldExpr` whose receiver's type is
//! *independently confirmed* project-local, accessing a name that *is* a
//! real declared member on that exact type, must never come out
//! `Unresolved`. If it does, something in `crate::resolve`/
//! `crate::inherit` failed to connect a real reference to its real
//! declaration -- exactly what happened in all three bugs.
//!
//! This is deliberately *not* the naive "does this name appear as a call
//! anywhere in the text" heuristic (rejected during design: `List.add()`/
//! `Map.get()`/`System.assertEquals()` would swamp it with false
//! positives from unrelated same-named project methods). The receiver's
//! type is confirmed independently of the very `type_of_symbol` chain
//! that produced the `Unresolved` outcome -- via the receiver's own
//! already-recorded `Resolution` (a name reference) or its own `Type`
//! node text (`new Foo()`) -- and then checked against
//! `SymbolTable::lookup_member` for *that exact type*, not a project-
//! wide name search. That combination has no cross-class collision risk
//! at all: `List`/`Map`/`String` receivers never confirm as project-
//! local in the first place, so their members never appear here
//! regardless of what any unrelated project class happens to be named.
//!
//! Validated (see the commit that added this test) by temporarily
//! reverting to the pre-fix source and confirming this test does fail --
//! loudly: 3,040 hits across 518 distinct type/member pairs, not just
//! the 3 originally reported. That's strong evidence the three general
//! fixes already eliminated this entire bug *class* project-wide, not
//! only the specific instances that happened to get reported.
//!
//! Only handles the simplest, most common receiver shapes (`new Foo()`,
//! a bare local/field/parameter/type-name reference, `this`) --
//! deliberately skips a chained `MethodCallExpr`/`FieldExpr` receiver
//! (recursing into that reintroduces the same "trust the buggy chain"
//! risk this test exists to avoid). A future bug two rungs deep in a
//! chain could still slip past this test uncaught -- an accepted gap in
//! coverage, not a guarantee of completeness, same as
//! `resolution_regression_baseline.rs`'s own acknowledged limits.

use apex_binder::{BoundProgram, FileId, Resolution, SymbolId, SymbolKind, SyntaxPtr};
use apex_syntax::ast::expr::{Expr, FieldExpr, MethodCallExpr};
use apex_syntax::SyntaxKind;
use rowan::ast::AstNode;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// The receiver expression's project-local type, *independently*
/// confirmed via its own already-recorded `Resolution` (for a name
/// reference) or its own `Type` node text (for `new Foo()`) -- never by
/// calling `type_of_symbol` again, since that's the exact function this
/// test exists to cross-check.
fn confirmed_receiver_type(program: &BoundProgram, file: FileId, target: &Expr) -> Option<SymbolId> {
    match target {
        Expr::New(ne) => program.symbols.resolve_dotted_name(&ne.type_ref()?.text()),
        Expr::Name(n) => {
            let ptr = SyntaxPtr::new(file, n.syntax());
            match program.resolution(ptr)? {
                Resolution::Resolved(id) => {
                    let sym = program.symbols.get(*id);
                    match sym.kind {
                        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => Some(*id),
                        _ => sym
                            .type_name
                            .as_deref()
                            .and_then(|tn| program.symbols.resolve_dotted_name(tn)),
                    }
                }
                _ => None,
            }
        }
        Expr::This(t) => {
            let ptr = SyntaxPtr::new(file, t.syntax());
            match program.resolution(ptr)? {
                Resolution::Resolved(id) => Some(*id),
                _ => None,
            }
        }
        _ => None,
    }
}

#[test]
fn no_confirmed_project_local_member_access_is_ever_unresolved() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );
    let program = BoundProgram::from_files(&root);

    let mut hits: FxHashMap<(String, String), usize> = FxHashMap::default();
    let mut total = 0usize;

    for (ptr, resolution) in program.all_resolutions() {
        if !matches!(resolution, Resolution::Unresolved) {
            continue;
        }
        let is_method = ptr.kind() == SyntaxKind::MethodCallExpr;
        let is_field = ptr.kind() == SyntaxKind::FieldExpr;
        if !is_method && !is_field {
            continue;
        }
        let root_node = program.syntax(ptr.file());
        let Some(node) = ptr.to_node(&root_node) else {
            continue;
        };

        let (target, name) = if is_method {
            let Some(mc) = MethodCallExpr::cast(node) else { continue };
            let Some(target) = mc.target() else { continue };
            let Some(name) = mc.method_name_token() else { continue };
            (target, name.text().to_string())
        } else {
            let Some(fe) = FieldExpr::cast(node) else { continue };
            let Some(target) = fe.target() else { continue };
            let Some(name) = fe.member_token() else { continue };
            (target, name.text().to_string())
        };

        let Some(container) = confirmed_receiver_type(&program, ptr.file(), &target) else {
            continue;
        };

        let found = if is_method {
            program
                .symbols
                .lookup_member(container, &name)
                .into_iter()
                .any(|id| program.symbols.get(id).kind == SymbolKind::Method)
        } else {
            program
                .symbols
                .lookup_member(container, &name)
                .into_iter()
                .any(|id| matches!(program.symbols.get(id).kind, SymbolKind::Field | SymbolKind::Property))
        };
        if found {
            total += 1;
            let type_name = program.symbols.get(container).name.to_string();
            *hits.entry((type_name, name)).or_default() += 1;
        }
    }

    if total == 0 {
        return;
    }

    let mut ranked: Vec<((String, String), usize)> = hits.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut report = format!(
        "{total} reference(s) access a real declared member on a confirmed project-local \
         receiver type, yet resolved Unresolved -- each one is a likely resolver bug (see this \
         file's own module doc comment for the invariant this violates):\n"
    );
    for ((ty, member), count) in ranked.iter().take(40) {
        report.push_str(&format!("  {count:>4}  {ty}.{member}\n"));
    }
    if ranked.len() > 40 {
        report.push_str(&format!("  ... and {} more distinct pairs\n", ranked.len() - 40));
    }
    panic!("{report}");
}
