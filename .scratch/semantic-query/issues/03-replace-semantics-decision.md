Type: grilling
Status: resolved
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

## Answer

Built, not just specified. `apexls query PATTERN --replace TEMPLATE`.

**1. The replacement is a string template**, not a tree transform -- the near-universal choice
(ast-grep, Semgrep, Comby) and the right one here, because a rewrite is a byte-range splice into
text that is never re-printed. The formatting of everything untouched survives by construction,
which is the problem Comby disclaims outright ("not well-suited to stylistic changes") and
Semgrep has open indentation bugs for (#3070, #3577).

**2. Only named captures may appear in it.** A bare `...` is rejected at compile time: on the
match side it stands for code nobody named, so on the output side it has nothing to refer to.
Positional correspondence between the nth `...` of each side is how Coccinelle does it and is
easy to get wrong. Requiring a name makes the template total -- every hole in it has exactly one
binding -- and `$_` is rejected too, since it deliberately binds nothing. A capture the pattern
never binds is also a compile-time error, before any file is touched.

**3. The replaced span is the match's significant range**, never `text_range()`, so a preceding
comment is not eaten. Replace therefore operates on *the node matched*:
`for (...) { ... System.debug(...); ... }` replaces the whole loop, and deleting just the call
means querying `System.debug(...);` directly. That is the honest model; replacing a sub-part of
a match has no principled definition.

**4. An empty template deletes, and takes the whole line when nothing else is on it.** Splicing
only the significant range would leave a blank indented line at every site, which makes corpus
R2 useless in practice. Deliberately narrow: the line must be whitespace on both sides of the
match, so a deletion never takes code with it.

**5. Nested matches collapse to the outermost.** This revises the map, which said to refuse an
overlapping pair and report both. Search reports nested matches on purpose -- a call inside two
nested loops really is two hits -- so under a blanket refusal every nesting pattern would be
unrewritable and the refusal list would be noise. Rewriting the outermost loses nothing, since
the inner text is part of what the outer rewrite replaces. Genuinely *crossing* overlaps, which
are the ambiguous ones, are still refused and both locations named.

**6. Edits apply highest-offset-first** via `String::replace_range`, so earlier offsets stay
valid without remapping -- the idiom `apexls-server`'s own fix pipeline uses, reused rather than
its module, which is `pub(crate)` and deletion-tuned.

**7. In place, no dry run**, per the map: a repository is under version control, so `git diff`
is the preview and `git checkout` the undo. The rehearsal is the same command without
`--replace`, which is just a search.

### The safety net, which is ours alone

**Every rewritten file is re-parsed, and not written if it gained parse errors.** One parse per
changed file turns a mistyped template from silent corruption across a codebase into a clean
refusal that names the file and the error. No tool in the survey does this, because none of them
has a parser to hand. A refusal also sets a failing exit code, so a script cannot read "some
files were skipped" as success.

### Deviation from the map, recorded

`--replace` is a flag, not the second positional argument the map specified. With a variadic
`paths` argument a bare second positional is ambiguous -- `apexls query 'pat' src/` cannot be
told from `apexls query 'pat' 'replacement'`. The map's reasoning for a separate argument was
that pattern and replacement stay two independent strings that lift into a rule file unchanged,
and a flag preserves that intent exactly.

### Known gap

Captures match exactly one construct, so a *variable-length* run cannot be carried across:
`Database.query($Q)` -> `Database.queryWithBinds($Q, ...)` has nowhere to put the original
arguments. That wants a named sequence capture, which Semgrep spells `$...ARGS`. The binding
corpus does not need it (R1 and R2 are covered), and it is the obvious first extension.

Verified end to end on a scratch file: `$X.size() > 0` -> `!$X.isEmpty()` rewrote correctly,
`System.debug(...);` -> empty deleted both calls and their lines, and all three refusal paths
(`...` in the template, an unbound capture, a template that would not parse) left the file
byte-identical.
