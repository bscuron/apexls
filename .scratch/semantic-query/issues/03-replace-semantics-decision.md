Type: grilling
Status: open
Blocked by: 02

## Question

Given a match (ticket 02), what exactly does the replacement produce, and how does it reach disk?

Replace is specced by this map and built afterward, but it is specced *now* because an ellipsis
that is easy to search with is often ambiguous to write back -- deciding replace late risks
discovering the search syntax cannot host it. Read the map's Notes first, plus §13's axis 6
(rewrite fidelity) and §12 in
[`research/01-structural-search-syntax-landscape.md`](../research/01-structural-search-syntax-landscape.md).

Decide, concretely:

1. **What the replacement string is.** Near-universally in the survey, `fix` is a *string
   template* with captures substituted, not a tree transform -- ast-grep states the constraint
   outright ("can only fix one target node at one time by replacing the target node text with a
   new string"). Confirm apexls does the same, and define what `$NAME` substitutes: the captured
   node's significant text, or its raw text including trivia. An empty replacement deletes
   (Semgrep's `fix: ""`), so define what deleting a statement does to its trailing semicolon and
   its line -- corpus R2 (delete every `System.debug(...);`) is the test.
2. **What happens to `...` regions.** A `...` is not reproduced in the replacement; the text it
   matched is simply not part of the replaced span, or is carried through untouched. Say which.
   For `for (...) { ... System.debug(...); ... }` the answer determines whether a rewrite can
   target the inner statement while leaving the loop alone -- which is almost certainly what a
   user wants, and means the *replaced span* is not the same as the *matched span*.
3. **How the edit is applied.** Reuse the idiom, not the module: `String::replace_range`,
   highest-offset-first so no offset remapping is needed (`apexls-server/src/fix.rs:83-137`).
   `fix.rs` itself is entirely `pub(crate)`, its only `pub` door hardcodes dead-code as its
   producer and discards every `TextRange` at the boundary, and its conflict policy is
   deletion-tuned -- silently subsuming a nested edit is right for a deletion and wrong for a
   rewrite. Confirm `query` does not open that module up, and say whether anything there should
   be lifted to shared code rather than duplicated.
4. **The trivia hazard, again.** The replaced span must be the significant span, not
   `text_range()`, or a rewrite will eat the comment before the statement and the newline after
   it. This is the same shared helper ticket 02 specifies; confirm replace uses it.
5. **Overlap handling.** The map decided an overlapping pair is refused and both locations
   reported. Define what counts as overlapping (crossing only, or nested too), what is printed,
   and what the exit code is. Note that formatting fidelity -- the thing Comby disclaims outright
   and Semgrep has open bugs for (#3070, #3577) -- is free here, because the printer is a
   byte-exact reproducer and untouched text is never re-printed. The spec should say so
   explicitly, since it is this tool's main advantage over every alternative.
6. **Whether indentation is ever adjusted.** ast-grep is the only surveyed tool with a documented
   rule ("the indentation level of a meta-variable in the fix string is preserved in the rewritten
   code"). Decide whether apexls re-indents a multi-line captured region spliced into a different
   nesting level, or splices verbatim and accepts the result. Verbatim is the lazier answer and
   may be correct; argue it either way rather than leaving it undefined.

Resolution records the replacement semantics, the apply pipeline, and the overlap policy with
its user-visible output.
