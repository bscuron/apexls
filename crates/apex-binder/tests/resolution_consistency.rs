//! A permanent, whole-corpus consistency guard -- distinct from
//! `resolution_regression_baseline.rs`'s golden-ratchet counts, which
//! only ever ask "how many references resolved," never "did the ones
//! that resolved actually resolve to something real." Every
//! `Resolution::Resolved` recorded for a call-shaped reference
//! (`MethodCallExpr`/`CallExpr`/`NewExpr`) must genuinely be *callable*
//! at that arity, and (except `this(...)`/`super(...)`, whose callee
//! token is never the target's own name) must actually share the
//! resolved symbol's name -- Apex has no varargs or default parameter
//! values for user-defined methods, so `crate::resolve::narrow_by_overload`'s
//! own module-doc-comment claim ("arity alone is authoritative, not a
//! heuristic") is a real, checkable invariant, not just documentation.
//!
//! This deliberately does **not** re-run the binder's own narrowing
//! logic (`narrow_by_overload`/`is_argument_type_compatible`/...) --
//! doing so would only ever agree with itself, since it's the same
//! deterministic function fed the same inputs. Instead it independently
//! re-derives the call site's actual argument count and callee text
//! straight from the AST, and the resolved candidate's own declared
//! arity/name straight from `SymbolTable`, checking the two agree --
//! catching a genuine implementation bug in *which* candidate a
//! resolution path picks (an indexing slip, a fallback branch that
//! skips a filter it should have applied) rather than a missing
//! feature. See the project's own `BACKLOG.md` for the broader
//! automated-bug-finding context this complements (a corpus-wide
//! `Unresolved` clustering tool, `examples/unresolved_clusters.rs`,
//! covers the complementary "missing" side of this same effort).

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::ast::expr::{CallExpr, MethodCallExpr, NewExpr};
use apex_syntax::SyntaxKind;
use rowan::ast::AstNode;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

/// `(callee_text, actual_arg_count, skip_name_check)` for a call-shaped
/// node -- `skip_name_check` is only ever true for `this(...)`/`super(...)`,
/// whose own callee token is the `this`/`super` keyword, never the
/// resolved constructor's real name.
fn call_shape(kind: SyntaxKind, node: apex_syntax::SyntaxNode) -> Option<(String, usize, bool)> {
    match kind {
        SyntaxKind::MethodCallExpr => {
            let mc = MethodCallExpr::cast(node)?;
            let tok = mc.method_name_token()?;
            let count = mc.args().map_or(0, |a| a.args().count());
            Some((tok.text().to_string(), count, false))
        }
        SyntaxKind::CallExpr => {
            let c = CallExpr::cast(node)?;
            let tok = c.callee_token()?;
            let count = c.args().map_or(0, |a| a.args().count());
            let is_this_or_super = matches!(tok.kind(), SyntaxKind::This | SyntaxKind::Super);
            Some((tok.text().to_string(), count, is_this_or_super))
        }
        SyntaxKind::NewExpr => {
            let ne = NewExpr::cast(node)?;
            let type_ref = ne.type_ref()?;
            let tokens = type_ref.base_name_tokens();
            let tok = tokens.last()?;
            let count = ne.args().map_or(0, |a| a.args().count());
            Some((tok.text().to_string(), count, false))
        }
        _ => None,
    }
}

#[test]
fn every_resolved_call_site_genuinely_matches_its_own_arity_and_name() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let program = BoundProgram::from_files(&root);
    let mut violations: Vec<String> = Vec::new();

    for (ptr, resolution) in program.all_resolutions() {
        let Resolution::Resolved(id) = resolution else {
            continue;
        };
        if !matches!(
            ptr.kind(),
            SyntaxKind::MethodCallExpr | SyntaxKind::CallExpr | SyntaxKind::NewExpr
        ) {
            continue;
        }
        let root_node = program.syntax(ptr.file());
        let Some(node) = ptr.to_node(&root_node) else {
            continue;
        };
        let Some((callee_text, actual_arity, skip_name_check)) = call_shape(ptr.kind(), node) else {
            continue;
        };

        let symbol = program.symbols.get(*id);
        let declared_arity = program.symbols.params(*id).len();
        let arity_mismatch = actual_arity != declared_arity;
        let name_mismatch = !skip_name_check && !callee_text.eq_ignore_ascii_case(&symbol.name);
        if !arity_mismatch && !name_mismatch {
            continue;
        }

        // Only reached for an actual violation -- expected to be rare (or
        // zero), so paying for a line-number lookup (re-stringifies the
        // whole file's text) here, instead of for every candidate checked,
        // is what keeps this a fast whole-corpus sweep rather than an
        // accidentally-quadratic one.
        let file = program.file_path(ptr.file());
        let line = source_line(&program, ptr.file(), ptr.range().start());

        if arity_mismatch {
            violations.push(format!(
                "{}:{line}: `{callee_text}({actual_arity} args)` resolved to `{}` \
                 (declared with {declared_arity} params) -- arity mismatch",
                file.display(),
                symbol.name
            ));
        }
        if name_mismatch {
            violations.push(format!(
                "{}:{line}: call site spelled `{callee_text}` resolved to a symbol named `{}` \
                 -- name mismatch",
                file.display(),
                symbol.name
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "found {} call-shaped Resolved reference(s) whose own arity/name disagrees with the \
         resolved symbol's real declaration -- a genuine resolver bug, not a missing feature \
         (first 20 shown):\n{}",
        violations.len(),
        violations.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// 1-based line number of `offset` within `file`'s own source text.
fn source_line(program: &BoundProgram, file: apex_binder::FileId, offset: rowan::TextSize) -> usize {
    let text = program.syntax(file).text().to_string();
    text[..usize::from(offset)].matches('\n').count() + 1
}
