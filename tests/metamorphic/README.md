# Metamorphic tests

Transformation-invariant properties that need no external oracle, e.g.:

- Trivia-only edits (added/removed whitespace or comments) must not change
  the resulting typed AST.
- Wrapping an expression in redundant parens must not change AST shape
  beyond an explicit "parenthesized" wrapper node.
- Consistent, mechanical identifier renaming must not change tree shape —
  useful both as a correctness invariant and as a free, license-safe way to
  multiply corpus size by re-testing renamed derivatives of vendored files.
