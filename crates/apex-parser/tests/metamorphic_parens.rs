//! Metamorphic test for the property `tests/metamorphic/README.md`
//! already commits to: "wrapping an expression in redundant parens must
//! not change AST shape beyond an explicit 'parenthesized' wrapper node."
//!
//! Seeded from `tests/golden/expressions/*.cls` (real, diverse,
//! known-parseable precedence cases) rather than the NPSP corpus: this
//! keeps the test self-contained (no submodule dependency) and fast,
//! while proptest's combinatorial (fragment, sub-node) selection across
//! ~256 cases per run still exercises the property broadly across the
//! whole precedence table.
//!
//! For each generated case: parse a seed expression, pick one of its
//! `Expr`-castable sub-nodes (by its exact source span), wrap that exact
//! span in `(...)` at the text level, and reparse. The new tree, with
//! every `ParenExpr` wrapper unwrapped, must be structurally identical
//! (same node-kind/token-kind/token-text shape) to the original.

use apex_syntax::ast::Expr;
use apex_syntax::{AstNode, NodeOrToken, SyntaxKind, SyntaxNode};
use proptest::prelude::*;
use std::fmt::Write as _;

fn seed_fragments() -> Vec<String> {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/expressions");
    let mut fragments: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "cls"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    fragments.sort();
    assert!(
        !fragments.is_empty(),
        "no seed fragments found under {dir:?}"
    );
    fragments
}

/// Node-kind/token-kind/token-text shape, recursively, with every
/// `ParenExpr` wrapper transparently unwrapped to its single inner child
/// and all trivia skipped -- what should be *identical* before and after
/// a redundant-paren-wrapping perturbation.
fn shape(node: &SyntaxNode) -> String {
    let mut out = String::new();
    write_shape(node, &mut out);
    out
}

fn write_shape(node: &SyntaxNode, out: &mut String) {
    // Checked on *every* call, not just when a parent's loop happens to
    // see a ParenExpr child: a doubly-wrapped `((expr))` has an inner
    // ParenExpr whose *parent* is itself a ParenExpr, reached via the
    // unwrap branch below rather than the normal per-child dispatch, so
    // it must re-check itself on entry to unwrap all the way through
    // however many redundant-paren layers are actually present.
    if node.kind() == SyntaxKind::ParenExpr {
        for child in node.children() {
            write_shape(&child, out);
        }
        return;
    }

    write!(out, "({:?}", node.kind()).unwrap();
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(n) => write_shape(&n, out),
            NodeOrToken::Token(t) if !t.kind().is_trivia() => {
                write!(out, " {:?}:{:?}", t.kind(), t.text()).unwrap();
            }
            NodeOrToken::Token(_) => {}
        }
    }
    out.push(')');
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn wrapping_any_subexpr_in_parens_preserves_ast_shape(
        fragment_idx in any::<usize>(),
        node_choice in any::<u32>(),
    ) {
        let fragments = seed_fragments();
        let src = &fragments[fragment_idx % fragments.len()];

        let original = apex_parser::parse_expression(src);
        prop_assume!(original.errors.is_empty());

        let exprs: Vec<Expr> = original.syntax().descendants().filter_map(Expr::cast).collect();
        prop_assume!(!exprs.is_empty());
        let target = &exprs[(node_choice as usize) % exprs.len()];
        let range = target.syntax().text_range();
        let (start, end): (usize, usize) = (range.start().into(), range.end().into());

        let wrapped = format!("{}({}){}", &src[..start], &src[start..end], &src[end..]);
        let reparsed = apex_parser::parse_expression(&wrapped);
        prop_assert!(
            reparsed.errors.is_empty(),
            "wrapping {:?} in parens introduced parse errors: {:?}",
            &src[start..end],
            reparsed.errors,
        );

        prop_assert_eq!(
            shape(&original.syntax()),
            shape(&reparsed.syntax()),
            "src={:?} wrapped={:?}", src, wrapped,
        );
    }
}
