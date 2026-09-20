# Specify `apexls query`: structural search and replace over Apex

## Destination

Specify `apexls query`: a structural search-and-replace subcommand over Apex source. The spec
fixes the pattern language (`...` for any code, deep by default; `$NAME` for captures,
unifying on reuse), the CLI shape (`apexls query PATTERN [REPLACEMENT]`, vimgrep output,
replace writes in place), and the matching semantics against apexls's lossless CST. Search is
built from this spec; replace is specced now and built after. Binder-backed constraints get a
reserved `--where` slot that v1 leaves empty.

This map is a spec/decision only -- tickets settle design questions; implementation happens
after, not as part of resolving a ticket.

## Notes

- **Domain**: apexls Rust workspace. The relevant crates are `apex-lexer`, `apex-parser`
  (fragment entry points, error recovery), `apex-syntax` (the lossless rowan CST and its typed
  AST layer), `apex-printer` (27-line byte-exact reproducer), `crates/apexls` (the CLI, which
  depends on both `apex-binder` and `apexls-server`), and `apexls-server`'s `fix.rs` (the
  existing edit pipeline).
- Per wayfinder default, consult the grilling and domain-modeling skills for decision tickets.
  Dispatch research subagents for codebase facts rather than asking the user anything checkable
  in the repo.
- **The research is already done. Read it before opening a ticket.**
  - [`research/01-structural-search-syntax-landscape.md`](research/01-structural-search-syntax-landscape.md)
    -- 1918 lines, 12 tools from primary docs with verbatim patterns (Comby, Semgrep, ast-grep,
    GritQL, Coccinelle/SmPL, CodeQL, tree-sitter, JetBrains SSR, gogrep, Refaster, PMD XPath
    incl. real shipped Apex rules, rslint). §12 answers the cross-cutting questions, §13 gives
    ten axes of variation, and the appendix transliterates "System.debug inside a for loop"
    into all eleven notations side by side.
  - [`research/02-local-substrate.md`](research/02-local-substrate.md) -- what apexls actually
    provides, with `path:line` citations throughout.
  - [`research/_raw/`](research/_raw/) -- fuller per-tool quote context.

### Settled during charting (do not relitigate; these are the spec's premises)

- **Destination is a spec**, not an implementation. The syntax choice is inseparable from what
  the engine can match, so the two are specced together and built afterward.
- **Structural core, with a reserved constraint slot.** v1 resolves nothing semantically. The
  spec names *where* constraints attach (`--where`) even though v1 supports none, because
  reserving a slot is free and retrofitting one into a language people have written patterns in
  is a breaking change.
- **CLI one-liner first**, designed so a rule file is a later superset. `apexls query PATTERN
  [REPLACEMENT] [--where '...']` -- replacement as a second positional argument, not an inline
  `=>`, so the pattern string means the same thing in search and replace and both lift into a
  rule file unchanged.
- **`...` is the only "match anything" operator**, disambiguated by position (any for-header,
  any statement run, any argument list). `$NAME` is added only when a capture is needed.
- **`...` is deep by default**: it crosses block boundaries. `for (...) { ... [SELECT ...] ... }`
  matches a SOQL query nested inside an `if` inside the loop, because that is still the
  governor-limit bug. Semgrep chose the opposite and documents its users' surprise:
  "The ellipsis operator does not jump from inner to outer statement blocks."
- **`$X` used twice unifies** -- both occurrences must match the same code. Bindings scope *per
  match*, not per run; do not make them global (GritQL's `bubble` page is the cautionary tale).
- **Output is vimgrep-style**, matching the `soql` subcommand's existing idiom: `path:line:col:text`.
- **The subcommand is `apexls query`.** Note this overloads a word `soql.rs` already owns
  (`struct Query` = a SOQL query site); ticket 04 settles which concept keeps the name.
- **Replace writes in place.** No `--write` flag, no diff-by-default, no clean-tree check --
  version control is the undo.
- **Nested matches**: search reports every match including nested ones; replace refuses an
  overlapping pair and names both locations rather than silently picking one.
- **No escape hatch in v1.** The pattern literal is the whole language; the ceiling is documented
  honestly. The Q9 corpus is the instrument that will reveal whether `kind`/`regex` constraints
  are actually needed -- see Not yet specified.

### The corpus (the spec's acceptance test)

The stated goal is that anything a user might want to search for in Apex should be searchable.
That is a direction, not a v1 bar -- the survey's clearest finding is that universality and
writability trade off directly (CodeQL and PMD XPath can ask anything and nobody wants to write
them). These are the queries the spec is tested against; the binding subset is **1, 2, 3, 4, 6,
R1, R2**, which between them exercise containment, sibling sequences, nesting depth, and a
capture reused on the replace side:

1. SOQL inside a loop *(governor limits)*
2. DML inside a loop
3. `System.debug(...)` in non-test code
4. A `catch` block that swallows -- empty, or only a `System.debug`
5. `@AuraEnabled` method on a class not declared `with sharing`
6. `Database.query(...)` built from string concatenation *(SOQL injection)*
7. Hardcoded Salesforce Ids as string literals
8. `Test.startTest()` with no matching `Test.stopTest()`
9. SOQL with no `WHERE` and no `LIMIT`
10. `@future` methods
11. Trigger bodies with logic inline instead of delegating to a handler class

Replace: **(R1)** `x.size() == 0` -> `x.isEmpty()`, `x.size() > 0` -> `!x.isEmpty()`;
**(R2)** delete every `System.debug(...);`; **(R3)** redundant null-comparison forms.

### Load-bearing substrate facts (confirmed this session, with citations)

- **The fragment problem is already solved, partially.** `parse_expression` / `parse_statement`
  / `parse_block` exist with dedicated `ExprRoot` / `StmtRoot` / `BlockRoot` kinds
  (`crates/apex-parser/src/lib.rs:62-84`). This is gogrep's multi-entry-point design, already
  built. Two catches: `parse_expression` leaves trailing content unconsumed, so a caller must
  check both `errors.is_empty()` *and* that the tree's text covers all of `src`
  (`lib.rs:56-61`); and **a bare `catch` clause has no entry point** -- `CatchClause` is
  reachable only through `statement()`'s `try` arm, which is corpus item 4.
- **The parser never panics and always produces a lossless tree**
  (`crates/apex-parser/src/errors.rs:3-6`). Round-trip holds even on error trees
  (`crates/apex-parser/tests/roundtrip.rs:12-21`). `ParseError` carries `offset: u32`, not a
  range. Any walker needs `apex_parser::RECOMMENDED_MIN_STACK_SIZE` (64 MiB) -- deep trees
  overflow on *drop*.
- **Neither `...` nor `$` is an Apex token.** `...` lexes as three separate `Dot` tokens
  (`crates/apex-lexer/src/punct.rs:31`) and `$` falls through to `TokenKind::Unknown`
  (`punct.rs:141`). The lexer never fails, so a pattern string lexes cleanly and then parses
  into garbage. Preprocessing before parsing is mandatory -- this is ticket 01.
- **Rewrite fidelity is free here.** `apex-printer` is 27 lines,
  `render(node) = node.text().to_string()` -- a byte-exact reproducer, not a formatter, and
  no formatter exists in the workspace. There is no machinery for printing a *modified* tree;
  rowan green-tree mutation is used nowhere. All editing is text-range splicing over the source
  string. Every competing tool works around not having this (Comby disclaims formatting
  fidelity outright; Semgrep has open indentation bugs #3070/#3577).
- **A node's `text_range()` swallows trivia at both ends.** `soql.rs:203-217` re-derives a
  significant span from non-trivia descendant tokens instead; the same hazard is documented at
  `crates/apex-syntax/src/ast/mod.rs:106-125` (`Name::ident_range`). A matcher hits this the
  moment it reports a position or splices a range. **This belongs as one helper on the syntax
  layer with `soql` switched over to it, not a second private copy inside `query`.**
- **`soql` is the traversal precedent**: rayon-parallel per file (`soql.rs:131`), binds nothing,
  globally sorts after collection, prints `path:line:col:text` with whitespace collapsed via
  `split_whitespace().join(" ")`. It now dispatches `.trigger` to `parse_trigger_unit`
  (`soql.rs:163-178`); `ast.rs:39` still does not. `project.rs` has no project loading -- just
  `find_project_root`, `matches_any`, `canonicalize_filters`, `ArgError`.
- **`fix.rs` is not reusable.** `CandidateFix { range, trigger_range, new_text, description }`
  and the whole pipeline are `pub(crate)` (`crates/apexls-server/src/fix.rs:19-33`); the only
  `pub` door, `resolve_fixes_for_file` (`apexls-server/src/lib.rs:2100`), hardcodes dead-code
  as its sole producer, requires a `&BoundProgram`, and discards every `TextRange` at the
  boundary. Its conflict policy is also deletion-tuned -- silently subsuming a nested edit is
  right for a deletion and wrong for a rewrite. Reuse the *idiom* (`String::replace_range`,
  highest-offset-first, so no offset remapping) rather than the module.
- **The binder resolves but has no types.** `Resolution`
  (`crates/apex-binder/src/reference_table.rs:137-146`) answers "what does this resolve to" and
  "is this a stdlib member", queried by `SyntaxPtr` which any walk can build on the spot. But
  `Ty` is `pub(crate)` and per `crates/apex-binder/src/ty.rs:6-8` "never appears in `Resolution`
  or gets stored on `BoundProgram`". The only public type info is per-declaration raw text
  (`Symbol::type_name`/`type_args`), one level deep. So a future `--where` can say
  "resolves to stdlib `System.debug`"; it cannot say Semgrep's `metavariable-type`. Binding is
  whole-project only, no partial-bind hook (`check.rs:9-19`), ~249 MB peak on NPSP -- a purely
  structural search must not bind at all, exactly as `soql` does not.
- **No matching code exists anywhere.** No visitor trait, no matcher, no query DSL, and no
  `SyntaxKind::from_str` (only `TryFromPrimitive` over `u16`), so a pattern language that names
  kinds needs one written. `SyntaxKind` is one flat enum: 257 token + 118 node kinds
  (`crates/apex-syntax/src/syntax_kind.rs:43-200`). The universal traversal idiom
  `root.descendants().filter_map(T::cast)` appears 40+ times with no shared helper.

## Decisions so far

<!-- one line per closed ticket, then zoom the link for the detail -->

- [Prototype: pattern string to syntax tree](issues/01-pattern-to-tree-prototype.md): holes
  substitute to ordinary Apex identifiers before the unmodified parser sees them (`...` ->
  `__AP_DOTS__`, `$NAME` -> `__AP_CAP_NAME__`), with two purely textual position rules --
  statement-position holes take a trailing `;`, and the `for (`/`catch (` slots expand to a
  multi-token shape. Entry point is chosen by trying `parse_expression`/`parse_statement`/
  `parse_block` in order and accepting the first whose parse has no errors *and* leaves the root
  with exactly one non-trivia child -- a text-range coverage check looks equivalent but is dead
  code, since the root marker always spans the whole input. 12 of 14 corpus patterns parse; the
  spec requires exactly one new parser entry point (`parse_catch_clause`/`CatchRoot`) and no
  other grammar change, plus a compile-time error for holes in operator position, the one slot
  no substitution can reach.

- [Decision: match semantics](issues/02-match-semantics-decision.md): recursive comparison of
  significant children; exact sequences unless an ellipsis is present; `...` deep only inside a
  `Block` (retry the next fixed element against descendants), with the block's own braces
  excluded from the statement sequence; unification on significant text, scoped per match;
  positions from the new shared `apex_syntax::significant_range`, with `soql` switched onto it.
  **Search is built and shipped** as `apexls query` -- the whole NPSP corpus in ~0.6s, binding
  nothing. A compiled `Pattern` holds a `GreenNode`, not a red `SyntaxNode`, which is a
  thread-local cursor and cannot cross rayon workers. Deep matching runs over the block's
  subtree flattened into document order with a forward-only cursor, so `{ ... P ... Q ... }`
  means "P somewhere, then Q somewhere after it" and the reverse order is a different query; it
  is offered only for a pattern unanchored at both ends, so `{ ... P }` keeps its promise that
  P is last. A block segment may be written
  as a bare expression (`{ ... [SELECT ...] ... }`): the omitted `;` is supplied and the
  statement wrapper unwrapped when searching, so the flagship query works. Matching is
  case-insensitive, as Apex is, with string literal contents excepted. **The whole binding
  corpus subset is expressible**, item 4 included via the new `parse_catch_clause`/`CatchRoot`
  entry point -- added knowingly, since a bare `catch` is not valid Apex (confirmed against a
  real org) but no fragment entry point parses standalone-valid Apex anyway.

## Not yet specified

- **Replace.** The destination's remaining half: search is built, replace is specced and not.
  This is now the largest gap against the original ask.

- ~~The escape hatch~~ **-- resolved, and the answer was "both".** Running the corpus against
  built search was the instrument the map said it would be: item 9 needed `kind:` (a SOQL `WHERE`
  hole cannot be written, since a value must be a literal or bind rather than an identifier) and
  item 7 needed `regex:` (a hardcoded Id is a string of a given length and alphabet, which no
  tree shape distinguishes). Both shipped, plus `--not`/`--containing` for questions about
  absence. `SyntaxKind::from_name` is generated by the same macro as the enum, so it cannot
  drift. The predicted cost is real and documented: `kind:` hands the user grammar node names,
  which is what pattern literals exist to avoid, so it is a fallback rather than the main road.
- **Binder-backed `--where` predicates.** The slot is reserved by this map and left empty. What
  can eventually go in it is bounded by the substrate note above: resolution and stdlib-membership
  yes, expression types no. Revisit once search exists and the corpus has shown which queries are
  false-positive-prone for lack of resolution (corpus item 3 is the obvious candidate -- a
  user-defined class named `System` would match structurally).
- **Isomorphisms.** Coccinelle's `standard.iso` is the one idea in the survey nobody copied: a
  loaded file of semantic equivalences (`X == NULL <=> NULL == X <=> !X`) so one written pattern
  matches every equivalent spelling, disableable per-rule by name. For corpus R1 (`x.size() == 0`
  / `0 == x.size()` / `x.isEmpty()`) that is the difference between one pattern and three OR'd
  together. Strong candidate once the core exists; cannot be specified before match semantics are.
- **Rule files as a superset of the one-liner.** The CLI shape was chosen to make this a clean
  later addition (pattern and replacement become two keys). What the file format is, what metadata
  it carries, and how multiple rules compose is not specified and should not be until the
  one-liner has real use.
- **Whether the matcher gets an LSP surface.** `apexls-server` could expose query-driven
  navigation or a workspace-wide structural find. Not part of this destination.
- **Whether one-to-many compilation generalises into isomorphisms.** `for (...)` compiling to
  both loop forms is now built, so `Pattern` already holds several trees and matches if any does
  -- structurally the machinery Coccinelle's isomorphisms need. Whether the two become one
  mechanism, or the loop case stays a hardcoded special case alongside a separate
  equivalence-file feature, is still open.

## Out of scope

- **Queries as user-extensible lint rules** -- checked-in rules that `apexls check` reports and
  `apexls fix` applies. A real and attractive product, ruled out of *this* effort deliberately:
  it would force every question on this map through the zero-false-positive detection bar that
  CONTEXT.md sets for diagnostics, and couple a brand-new language to two shipped subcommands.
  A user-authored query is ad-hoc and best-effort by nature, which is the opposite of what that
  bar demands. Revisiting this means redrawing the destination, i.e. a fresh effort.
