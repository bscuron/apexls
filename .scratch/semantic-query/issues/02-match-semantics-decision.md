Type: grilling
Status: open
Blocked by: 01

## Question

Given a pattern tree (ticket 01) and a source CST, what exactly constitutes a match?

The map settles the user-facing semantics; this ticket turns them into an algorithm precise
enough to implement and to write down in the spec. Read the map's Notes first, plus §13's axes
in [`research/01-structural-search-syntax-landscape.md`](../research/01-structural-search-syntax-landscape.md).

Decide, concretely:

1. **What `...` means against the CST, per position.** It is deep by default -- it crosses block
   boundaries -- but "deep" needs an operational definition. In statement position,
   `{ ... X ... }` means "a block containing X at any depth", while `{ X }` means "a block whose
   only statement is X", so the leading and trailing `...` are load-bearing and the spec must say
   so. Define the same for argument lists (`f(...)`, `f(1, ...)`), for a for-header, and for
   SOQL clause position. Note Coccinelle's `...` is CFG-path based with forall/exists quantifiers
   while Semgrep's is block-scoped; this map has chosen neither of those, so say precisely what
   it *has* chosen.
2. **How trivia is skipped.** The CST is lossless -- whitespace and comments are real nodes. A
   matcher that naively compares children will fail on any reformatted code. Define the skip
   rule, and confirm whether a comment between two statements can appear inside a `...` region
   (it must) and whether it can appear *inside* a fixed part of the pattern (e.g. between
   `System` and `.debug`).
3. **Unification mechanics.** `$X` twice must match the same code. Define "the same": identical
   token text, identical significant text ignoring trivia, or structural tree equality? These
   differ on `a.b` vs `a . b` and on `foo(1)` vs `foo( 1 )`. ast-grep compares structurally
   ("`$A == $A` matches `1 + 1 == 1 + 1`"). Confirm bindings scope per match, never per run.
4. **What a match's reported position is.** Output is vimgrep `path:line:col:text`. A node's
   `text_range()` swallows trivia at both ends, so the significant span must be re-derived from
   non-trivia descendant tokens the way `soql.rs:203-217` already does. This is a shared-layer
   fix: specify one helper on the syntax layer with `soql` switched over to it, not a second
   private copy inside `query`.
5. **Nested and overlapping matches.** The map has decided search reports every match including
   nested ones. Define the enumeration order and what "every match" means when a single pattern
   could bind its holes several ways at one site (e.g. `f(..., $X, ...)` against `f(a, b, c)`) --
   all bindings, or one per site? Note ast-grep's `$$$` is documented as *lazy*, "stopping at the
   first matching node rather than matching all nodes", and that this surprises people.
6. **Whether the walk binds.** It must not. Binding is whole-project only with no partial-bind
   hook and ~249 MB peak on NPSP (`check.rs:9-19`); `soql` binds nothing and is the precedent.
   Confirm the structural matcher stays on the parse-only path, and that the reserved `--where`
   slot is where binding would later be opted into.

Resolution records the match algorithm in enough detail that two people would implement the same
thing, plus the shared significant-span helper's shape and where it lives.
