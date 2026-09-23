Type: grilling
Status: resolved
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

## Answer

Settled by building it rather than by argument -- the semantics are pinned by the tests in
`crates/apexls/src/query.rs`, each of which names the rule it guards. Search ships; replace does
not, per the map.

**1. The algorithm.** A match is a recursive comparison of *significant children* (trivia
filtered out at every level), rooted at any descendant whose kind equals the pattern's root kind:

- Two tokens match when kind and text are equal.
- Two nodes match when kind is equal and their child sequences match.
- A pattern element that is nothing but a hole short-circuits: `...` matches anything, `$NAME`
  binds or unifies.
- A sequence without any ellipsis is exact, element for element. This is what makes `{ P }` mean
  "a block whose only statement is P" -- guarded by `a_block_pattern_without_an_ellipsis_is_exact`.
- An ellipsis consumes zero or more elements, with backtracking over where it stops.

**2. `...` is deep only inside a `Block`.** That is where "anywhere in this loop" has to mean
what a user expects; an argument list's `...` stays an ordinary sibling wildcard, since
`f(..., $X, ...)` reaching into a nested call's arguments would be nobody's intent. Implemented
as: after an ellipsis fails to match shallowly, retry the next fixed element against every
*descendant* of the remaining elements. Proven on real code -- on the NPSP corpus,
`for (...) { ... insert $X; ... }` finds 4 sites including an `insert` nested four blocks deep
inside an `if` inside the loop.

A block's own braces must be excluded from the statement sequence, or a trailing `...` followed
by `}` can never match once the `...` has consumed everything -- the pattern would fail on
exactly the nesting it exists to find. Statements are all nodes, so keeping only child *nodes*
drops both braces without special-casing either token.

**3. Trivia.** Filtered at every comparison, so a tightly written pattern matches loosely
written source, comments included -- `System.debug(...)` matches `System . debug( /* why */ 'a' )`,
and on NPSP `$X.size() > 0` matches `records.size()>0`. Guarded by
`matching_ignores_whitespace_and_comments`.

**4. Unification** compares *significant text*, so `a.b` and `a . b` are the same capture --
ast-grep's structural rather than byte-wise notion of "the same". Bindings are a plain map
threaded through one match attempt and cloned before each backtracking branch, so they scope per
match and never leak across sites. Guarded by `a_reused_capture_must_match_the_same_code`.

**5. Position** is `apex_syntax::significant_range`, now a shared helper on the syntax layer with
`soql` switched onto it (`crates/apexls/src/soql.rs` no longer carries its own copy). Guarded on
both sides: `soql`'s `anchors_past_a_leading_comment_rather_than_at_it` and `query`'s
`position_is_anchored_past_a_leading_comment`.

**6. The walk binds nothing** -- discover, parse, match, exactly as `soql` does. Whole-corpus
NPSP search runs in ~0.6s. One constraint found while building: a compiled pattern cannot hold a
red `SyntaxNode`, which is a thread-local cursor and neither `Send` nor `Sync`. `Pattern` stores
the `GreenNode` and each rayon worker rebuilds its own cursor.

**7. Nested matches** are all reported, outermost and innermost -- guarded by
`reports_every_nested_match_not_just_the_outermost`.

**8. A hole is recognised as a *token*, not by text.** Two bugs found in review, both fixed and
both now guarded, because they are the kind that returns confident wrong answers rather than
errors:

- Recognising a hole by text prefix/suffix made every *composite* pattern collapse into one
  match-anything hole: `$A + $B` substitutes to text that still starts with the capture prefix
  and ends with the capture suffix, so the whole binary expression read as one unbound capture.
  `$X = $Y;` matched all 48,452 statements in NPSP; it now matches 14,786 real assignments, and
  `Database.query($A + $B)` went from matching `Database.query(soql)` to finding 45 genuine
  string-concatenated queries -- **corpus item 6 works**. Guarded by
  `a_composite_pattern_is_not_mistaken_for_one_hole`.
- Deep descent threw away the pattern's tail, so `{ ... P }` matched a block with P buried in the
  middle and further statements after it, breaking the map's "the trailing `...` is load-bearing"
  rule. Guarded by `deep_descent_requires_a_trailing_ellipsis`.

### Corpus status, measured against NPSP

| Item | Status |
|---|---|
| 1 SOQL in a loop | works -- 0 hits on NPSP, which is the *correct* answer: 2,450 for-each loops, none containing SOQL. Verified against a controlled fixture instead |
| 2 DML in a loop | works (4 hits, one nested four blocks deep) |
| 3 `System.debug` | works (46 hits) |
| 4 swallowing `catch` | works -- `catch (...) { }` finds 48 empty catches, `catch (...) { ... System.debug(...); ... }` finds 6 log-and-continue ones |
| 6 `Database.query` concat | works (45 hits) |
| R1 `size() > 0` | findable (246 hits); the rewrite itself is not built, per the map |
| R2 delete `System.debug` | findable; rewrite not built |

### Ceilings this surfaced, recorded rather than patched

**9. A block segment may be a bare expression** (added after the ceiling below was first
recorded, because it blocked the flagship query). A user writing
`for (...) { ... [SELECT ... FROM $O] ... }` means "a loop containing this query" and writes the
query as it appears in code, with no terminator -- which is not a statement, so the block would
not parse. Two small changes make it work, and together they *are* the "let each segment choose
its own entry point" fix the map asked for:

- The omitted `;` is supplied before a statement-position ellipsis or a closing `}`. This parser
  accepts `[SELECT Id FROM Account];` as an `ExprStmt`, which is what makes the repair possible.
- The resulting `ExprStmt` wrapper is unwrapped when searching descendants, so the query is found
  where it really sits -- inside a declaration, an argument, a `return` -- not only as a
  statement of its own. Harmless for a segment that genuinely is a statement, which then matches
  by either route at the same site.

Deciding statement position also moved from "what is the previous non-space character" to "what
is the nearest unclosed bracket". The old rule read the trailing hole in
`{ ... [SELECT ...] ... }` as expression position, because the character before it is `]`.
Guarded by `finds_a_bare_expression_anywhere_inside_a_loop` and
`an_unterminated_segment_is_repaired_but_nonsense_is_still_rejected`.
- **Two fixed elements with no ellipsis between them** ask to be *consecutive*, which is
  meaningless once they may sit at different depths, so that shape stays shallow. Likewise a
  pattern missing its leading or trailing `...` has anchored that end to the block's first or
  last statement, which a match buried at depth is not. Both stay shallow rather than guessing.
- **An assignment pattern does not match a declaration.** `$X = [SELECT ...]` misses
  `List<Contact> cs = [SELECT ...]`. This is the isomorphism gap the map already tracks.
- ~~`for (...)` matches for-each loops only~~ **-- fixed.** See item 14.
**10. `parse_catch_clause`/`CatchRoot` is implemented**, the one grammar change ticket 01
specified. Checked against a real org first, at the user's insistence, and the answer was the
uncomfortable one: **a bare `catch` is not valid Apex** -- `sf apex run` rejects
`catch (Exception e) { }` with "Unexpected token 'catch'" while accepting the same clause
attached to a `try`. Added anyway, because no fragment entry point parses standalone-valid Apex
(`a + b` is not a program, nor is a bare `{ stmt; }`); they exist so tooling can name a
sub-construct, and `catchClause` is a real production. Anchoring on the clause rather than the
whole `try` is what lets a match report its own position and isolate one clause of a
multi-`catch`. The consequence is stated in the entry point's own doc comment: a pattern can now
be written that the Apex compiler would refuse.

**11. `... P ... Q ...` matches in document order.** Deep matching now flattens the block's
whole subtree into one document-ordered list and walks the pattern's fixed elements across it
with a forward-only cursor, so the shape means "P somewhere, then Q somewhere after it" however
deeply either is nested. Matching each fixed element independently was what made this
unanswerable before: a descendant match left no defined place to resume the search for Q.

On NPSP, `{ ... Test.startTest(); ... Test.stopTest(); ... }` finds 1,613 blocks and the same
two reversed finds **0** -- order is genuinely enforced, and `stopTest` never precedes
`startTest`, which is the best available check that the cursor is real. Guarded by
`two_fixed_elements_around_an_ellipsis_match_in_document_order`.

Note this does **not** deliver corpus item 8 (`Test.startTest()` with *no* matching
`Test.stopTest()`): that is a negative query and there is no negation in the language. What it
delivers is the positive ordering shape item 8 is built from, which is also acquire/release and
open/close.

**12. A `{`-enclosed `...` has two readings, and the parser picks.** Whether such a hole means
"a run of statements" or "an expression sitting inside one" cannot be decided from the text:
`{ ... [SELECT ...] ... }` needs the first and `{ ... String $v = ...; ... }` needs the second.
Two successive lookback rules each fixed one case and broke the other -- previous-significant-
character got the initializer right and the flagship wrong; nearest-enclosing-bracket got the
flagship right and the initializer wrong, regressing `{ ... String $v = ...; ... }` to a compile
error.

Resolved by not guessing. The bracket rule remains the first attempt; if nothing parses, holes
are flipped to the expression reading and retried, fewest flips first, until the parser accepts
one. It is the same "try readings until one parses" move the entry-point loop already makes, and
a pattern that compiles on the first attempt pays nothing. `{ ... String $v = ...; ... }` now
finds 2,203 blocks on NPSP, and `...;` and `...` are interchangeable spellings. Guarded by
`both_readings_of_a_brace_enclosed_hole_compile` and
`a_hole_in_an_initializer_matches_any_initialiser`.

**13. `for (...)` matches both loop forms**, which is ticket 01's one-to-many finding
implemented. `Pattern` now holds several green trees and a candidate matches if any of them
does; `for (...)` compiles to both `ForEachStmt` and `ForStmt` shapes, and a written-out
`for (...; ...; ...)` pins the C-style form. Any header part may be omitted -- a hole standing
alone in a sequence is an ellipsis, and an ellipsis may consume nothing, so `for (;;)` is
covered for free.

This was a *silent under-report*, the worst failure mode for a search tool: on NPSP
`for (...) { ... }` went from 2,451 to 2,988, so **537 C-style loops were invisible to every
loop query**, and `for (...) { ... insert $X; ... }` went from 4 to 5 -- the new hit is
`RD_RecurringDonations.cls:424`, `for ( ;j<installments;j++ )` with a DML insert inside it, a
real governor-limit bug the tool had been skipping. Guarded by `for_matches_both_loop_forms` and
`an_explicit_c_style_header_matches_only_that_form`.

**14. A string literal is opaque, and `'...'` means any string.** Substituting inside quotes was
a third silent wrong answer: `System.debug('...')` became a search for the literal text
`'__AP_DOTS__'`, so it returned 0 while reading as "any string argument". Strings are now left
alone by the substituter, so `'a...b'` is a three-dot string and `'$x'` is a dollar sign.

A literal that is entirely `'...'` is the exception and means *any string literal*, as Semgrep
spells it. It is its own hole kind rather than an ordinary ellipsis, because an ellipsis matches
anything: `System.debug('...')` would then also match `System.debug(x)`, losing exactly the
distinction the quotes were drawn to make. On NPSP it finds the 2 debug calls whose argument is a
bare literal, out of 54 debug calls and 35 single-argument ones. Guarded by
`a_whole_string_literal_hole_matches_any_string` and
`holes_inside_a_string_literal_are_just_text`.

**15. Declarations are queryable**, via a fifth entry point `parse_class_member`/`MemberRoot`
over the existing `class_body_decl` grammar -- which also covers fields, properties,
constructors, initializer blocks and nested types. Before this, nothing about a *declaration*
could be asked at all; only the code inside one. On NPSP: `private $T $f;` 657,
`public void $m(...) { ... }` 650, `@AuraEnabled public static $T $m(...) { ... }` 118,
`@future public static void $m(...) { ... }` 14 -- **corpus items 5 and 10 now reachable**.

Two things fell out of it:

- `f(...)` and `void m(...)` are written identically and mean different things, an argument list
  versus a parameter list, and a formal parameter is a `Type name` pair so a bare identifier does
  not parse there. Rather than another lookback rule, this became a second axis of the existing
  reading retry: default to arguments, flip to parameters, let the parser decide.
- `(...)` first meant *exactly one* parameter, because the two-token expansion was matched
  structurally rather than as a hole. `hole_of` now treats an element whose *every* token is a
  sentinel as one ellipsis, so it means any number including none. Requiring every token to be a
  sentinel is what keeps this from re-opening the composite-pattern bug: `$A + $B` and `... + ...`
  both contain a `+`.

Still out of reach: a whole class body (`class $C { ... }`), since a class holds members rather
than statements and a member-run hole does not exist. Guarded by
`finds_declarations_not_just_code_inside_them` and
`a_paren_hole_is_arguments_or_parameters_as_the_pattern_requires`.

**16. Two escape hatches and two filters**, which completes the binding corpus. The map left the
escape hatch unspecified on purpose -- "run the corpus against built search, and the queries that
cannot be written will say whether it is `kind`, `regex`, both, or neither." Run, the answer was
**both**, plus something the map had not anticipated: a way to ask about *absence*.

- `kind:Name` matches any node of a syntax kind, via a `SyntaxKind::from_name` generated by the
  same macro as the enum so it cannot drift. Needed because a SOQL `WHERE` hole cannot be
  written at all: a SOQL value must be a literal or a bind, never an identifier, so
  `WHERE $F = $V` does not parse and naming the clause is the only route.
- `regex:RE` matches any node whose significant text matches, anchored at both ends so a
  fragment cannot match a whole file. Needed because a hardcoded Salesforce Id is a string of a
  particular length and alphabet, which no tree shape distinguishes.
- `--not PATTERN` and `--containing PATTERN` filter a match by what it holds. "Contains"
  includes the match *itself*, not only its descendants, which is what lets a negation narrow a
  pattern rather than only exclude what is nested inside it.

The predicted cost is real and is documented rather than hidden: `kind:` hands the user grammar
node names, which is the thing pattern literals exist to avoid. Both are a fallback, not the
main road.

Corpus results on NPSP, completing the eleven:

- **7** hardcoded Ids: `regex:'[a-zA-Z0-9]{18}'` finds 124 eighteen-character string literals
  (the regex is the user's to tighten -- `'someOtherName12345'` is also eighteen characters).
- **8** unbalanced test blocks: `{ ... Test.startTest(); ... }` minus `Test.stopTest();` finds
  **37** blocks that start a test-context and never stop it.
- **9** unbounded queries: `kind:SoqlExpr` minus `kind:SoqlWhereClause` minus `kind:SoqlLimit`
  finds **646** of 2,132 queries with neither clause. Note `[SELECT ... FROM $O]` is *exact* in
  its clause list, so it means "a query with no trailing clauses at all" (520) -- which is why
  the kind form is the right way to say "any query".
- **11** inline trigger logic: `kind:TriggerBlock --containing kind:IfStmt` finds **0** of NPSP's
  26 trigger bodies, which is the correct answer -- NPSP delegates through its TDTM handler
  framework. Verified against a controlled pair of triggers, one inline and one delegating, where
  it flags exactly the inline one.

Guarded by `kind_names_a_syntax_kind_directly`, `regex_matches_node_text_anchored` and
`not_and_containing_filter_by_what_a_match_holds`.

**17. Holes became real tokens, and the special cases went away.** The user's verdict on the
accumulated per-position fillers was "I feel like we are missing something. Things shouldn't be
this complicated" -- and that was right. Substituting a hole with an identifier only works where
an identifier is grammatical, so every position needed its own filler (`;` for a statement,
`T V : C` for a `for` header, `T N` for a parameter, a bind for a SOQL value) plus a
combinatorial retry to choose between them. "Is this text grammatical here" has one answer per
grammar position, so the list could never be finished.

Replaced by a `PatternHole`/`PatternCapture` token pair that only `Input::new_pattern` ever
produces, and a `pattern_mode` parser that accepts one wherever the grammar *requires* a
construct. Real Apex parsing is untouched -- those tokens cannot arise from real source, and
apex-parser's own suite is unchanged.

The discipline that made it work: **a hole satisfies a requirement, it never decides a choice.**
Admitted at a choice point it misreads everything -- letting `expect()` take a hole let one
stand in for a *semicolon*, so `{ ... $X = y; }` swallowed the statement after the ellipsis, and
admitting one as a binary operator did the same a different way. Both were backed out. Operator
position is therefore still unreachable, and that is now a deliberate, documented limit rather
than an accident.

Two narrower ambiguities were decided rather than guessed: `[` after a bare `...` opens the next
construct instead of indexing into the ellipsis (`{ ... [SELECT ...] }`), while `[` after a
capture is still an index (`$X[0]`) -- which is why the two hole kinds are distinct; and a hole
after a SOQL object name is the trailing `...`, not a table alias.

Net effect: **905 lines deleted against 449 added**, `query.rs` alone about 450 lines lighter,
and the rule finally uniform -- `...` means "anything here", omitting it means exact. On NPSP
`[SELECT ... FROM $o ...]` returns 2,132, identical to `kind:SoqlExpr`, while
`[SELECT ... FROM $o]` returns 520; `[SELECT Id FROM $o WHERE Id = $v]`, impossible before,
finds 14. Every previously-measured corpus number is unchanged.

**18. Identifier globs, and modifiers as a subset.** Two additions driven by a real query,
`static void addChild*() { ... }`, which returned nothing for two independent reasons.

A glob (`addChild*`, `*_success`, `get?`) matches one identifier shell-style, case-insensitively.
It is folded from an adjacent run of name and wildcard tokens, and it must contain a *name* part:
without that rule a lone `*` folds into a glob by itself and every multiplication in a pattern
stops parsing -- `a * b` and `x.size() * 2` were both rejected until it was added. The cost is
that `a*b` in a pattern is a glob while `a * b` is arithmetic, which is worth stating out loud.
A glob captures nothing, so a replacement refuses it and says to use `$NAME` instead.

Modifiers on a declaration now match as a **subset**: every modifier the pattern names must be
present, and ones it is silent about are ignored. Matching the list exactly meant
`static void addChild*()` could not find `@isTest static void addChildQueries_success()`, since
the pattern had no way to say "and whatever else this is annotated with". Writing a modifier now
narrows the search: `void addChild*()` finds all 9, `static void addChild*()` still finds 9, and
`@future static void addChild*()` finds 0.

**19. Subset matching covers every modifier site**, not just members: a type declaration
(`@isTest class $C`, `with sharing class $C`), a local variable's `final`, a catch parameter, a
method parameter, a property accessor. Two details made the general form necessary -- a catch
clause's modifiers sit after `catch (` rather than leading the node, so the split is a partition
and not a prefix; and a local declaration bumps `final`/`transient` as bare tokens instead of
wrapping them in `Modifier` nodes, so those count as modifiers too. On NPSP:
`@isTest class $C { ... }` 283, `with sharing class $C { ... }` 363, `final $T $v = ...;` 386,
and `catch (final $T $e) { ... }` 0 -- the last independently corroborated by `apex-parser`'s
own note that the form "never appears anywhere in the NPSP corpus".

**20. Matching is case-insensitive, because Apex is.** Found by the user against the real
corpus: `Database.query(...)` returned 260 hits and `database.query(...)` returned 64, two
halves of one set. Both now return 327. String literal contents stay case-sensitive, since case
there is a difference in value rather than in spelling; capture unification is case-insensitive,
so `acc` and `Acc` are one capture. Guarded by `matching_is_case_insensitive_like_apex_itself`,
`string_literal_contents_stay_case_sensitive` and `a_reused_capture_unifies_across_case`.

**21. Conditions replace inline globs and `--containing`.** `apexls query PATTERN [--and COND]...
[--not COND]...`: a match is kept when every `--and` holds and no `--not` does. A COND is either
a pattern the match must contain (what `--containing` and `--not` took before), or `$X ~ GLOB` /
`$X ~ /REGEX/` on a capture's text. Globs are shell-style and anchored, with `{a,b}` alternatives
and `$V` for another capture's text, and case-insensitive; regexes are unanchored and
case-sensitive, Perl-style, unless flagged `/re/i` (also `x`, `s`, `m`). The two forms cannot be confused, since `~` is only a prefix operator in
Apex. The user chose this shape over inline constraint syntax and over boolean operators inside
a condition: the pattern says the structure, `~` the text, the flags yes or no.

Inline globs are gone. They were told from multiplication by whitespace alone (`a*b` versus
`a * b`), `get?` never compiled because `?` reads as a ternary, and a glob bound nothing, so a
matched name could not reach a replacement. Every NPSP count carried over exactly --
`static void $M() { ... } --and '$M ~ addChild*'` is 9, `$_ $M(...) { ... } --and '$M ~ add*'`
173, `$M(...) --and '$M ~ addChild*'` 4, as the inline forms were -- and the old filters too:
751, 107 and 11,303. Guarded by `a_glob_condition_tests_a_captures_text`,
`a_regex_condition_is_unanchored_and_takes_flags`, `a_glob_can_compare_two_captures`,
`a_condition_names_a_bound_capture` and `conditions_combine`.

**22. The return type is optional, as modifiers are.** A member pattern with no return type,
`$F(String $_) { ... }`, parses as a constructor and now also matches methods of any return
type, the name compared by hole or text. A written return type still pins: `void $F(...)` finds
only void methods. On NPSP `$F(...) { ... }` finds 12,095 -- every method with a body plus every
constructor, exactly. Found alongside it: the catch-clause fragment entry point bumped its first
token without checking it was `catch`, so every `NAME(...) { ... }` pattern without a modifier
compiled as a catch clause and matched nothing. Guarded by `a_return_type_is_optional`.

**23. A regex is case-sensitive unless flagged.** `/re/` began case-insensitive like everything
else, and `/^(get|set)[A-Z]/` then found 2,137 on NPSP, `getter` included, because `[A-Z]`
matched lowercase. A regex now means exactly what it says, with Perl's trailing flags: `/re/i`,
and `x`, `s`, `m`. That also brings `~` into line with `regex:` and `comment:`, which were
case-sensitive all along. Globs stay case-insensitive: they are the Apex-aware form, and Apex
names are case-insensitive. Guarded by `a_regex_condition_is_unanchored_and_takes_flags` and
`a_regex_condition_rejects_bad_flags`.

**24. Typed `when` arms, catch-clause runs, and quiet broken pipes.** `when $T $v { ... }`
takes captures as a typed arm's type and variable. In a `try`, a `...` right before a `catch`, a
`finally` or the pattern's end stands for any catch clauses, so `try { ... } ... finally { ... }`
is "a try with a finally, whatever it catches" (5 on NPSP), and `try { ... } ...` finds all 785.
After the last clause, a `...` still belongs to the enclosing block. Every subcommand now prints
through one `outln!` that exits 0 on a broken pipe instead of panicking. Known limitation, seen
here: when `...` lets a pattern match more than one way, the matcher keeps the *first* binding,
and conditions test only that one -- in `switch on $o { ... when $T $v { ... } ... }`, `$T` is
the first typed arm, so `--and '$T ~ Contact'` misses a `Contact` arm that is not first.

**25. `^` marks where a match is reported.** `void $_(...) { ... ^[SELECT ... FROM $o ...] ... }`
reports each method at its query's `[`, so `vim -q <(apexls query ...)` lands on the query
rather than the method. The printed text stays the whole match. `^` was chosen over `@` (which
clashes with annotations, so it could not sit before one) and `$` (the capture sigil, and a legal
Apex identifier): XOR is binary-only, so wherever a construct *starts* -- a statement, a prefix
expression, a member, a declared name -- `^` cannot be an operator, and between two operands it
still is (`$a ^ ^$b` focuses XOR's right operand). A `^` straight after a bare `...` is the
marker, since an ellipsis on the left is a statement run. One `^` per pattern; a `^` in an
`--and`/`--not` pattern is ignored. Like conditions, it follows the first binding: a method with
two queries is one hit, at its first. On NPSP the query above gives 810 hits with or without the
`^`, and no measurable time. Guarded by `a_caret_moves_the_reported_position` and
`a_caret_between_operands_is_xor`.

**26. With a `^`, each place it lands is a hit** -- superseding item 25's "one hit per match".
The user's call: `^` names what is being looked for, so a method holding two queries is two hits
for `void $_(...) { ... ^[SELECT ...] ... }`. Found by re-matching the already-matched node with
the positions reported so far ruled out, which backtracks to the next; each position keeps its
own captures, so conditions are checked per position, which fixes item 24's first-binding
limitation for any pattern that has a `^`. A position is reported once, since `{ ... ^X ... }`
also matches the blocks nested around X. The seen positions live in a per-thread set rather than
the bindings, which are cloned for every candidate, and a seen candidate is refused before it is
compared; together that took the NPSP run from 801 ms to about 315 ms, against 210 ms without the
`^`, for 1,703 hits instead of 810. Patterns without a `^` are untouched. Guarded by
`a_caret_reports_every_place_it_lands`.

**27. `$X : TYPE` tests a capture's inferred type.** The same glob as `~`, over the type's text as
the binder spells it (`String`, `List<Account>`), case-insensitive; inside it `$V` is V's *type*,
so `$a : $b` compares types where `$a ~ $b` compares spelling. An untyped capture fails every `:`
test, so `--not` keeps it; `$v : *` means "typed at all". Only a `:` condition binds the project
(~0.35 s on NPSP); everything else stays parse-only, confirmed by timing. The binder keeps no
types during a normal bind: `BoundProgram::expr_types_in(callable)` re-binds one method or
constructor body with a recorder switched on, so the server and `check` pay one never-taken branch
per expression -- `check` on NPSP timed 488 vs 490 ms, with identical output. Types are rendered to
text as they are recorded, so no symbol id needs remapping. Inline SOQL is typed on the recording
path only (`List<Object__c>`, `Integer` for a bare `COUNT()`, `List<AggregateResult>` for
aggregates), never returned into Pass 2, where it would start flagging `Account a = [SELECT ...]`.
Coverage measured on NPSP before building the condition: 88% of 437,259 expressions in 12,348
bodies typed, 100% of the 35 single-argument `System.debug($v)` calls; the largest remaining gap is
SObject field types. Known limits: only method and constructor bodies, and a captured type
reference (`$T` in `List<$T>`) is compared by its text, not resolved. Guarded by
`a_type_condition_tests_the_inferred_type`, `a_type_condition_compares_two_captures` and
`a_type_condition_needs_a_bound_capture`.

**28. Types reach every body the binder walks.** `$X : TYPE` now works in field initializers,
property accessors and trigger bodies, not only methods and constructors. The separate re-bind
function is gone: Pass 2's three body entry points take a `record_types` flag, and
`BoundProgram::expr_types_in(unit)` re-binds any declaration through the same dispatcher Pass 2
uses, so the two cannot drift. A cheap `type_unit_at(file, offset)` finds the innermost owning
declaration from the file's symbols, which lets a query cache per unit without binding to look.
On NPSP, 759 of 795 `private static` field initializers are typed (none were reachable before),
and all 26 trigger bodies' receivers. `check` is unchanged in time and output. Still not typed:
static and instance initializer blocks (`static { ... }`), which the binder does not bind at all.
Guarded by `a_type_condition_reaches_initializers_accessors_and_triggers`.

**29. `${...}` groups, and `$N => TEMPLATE` rewrites.** The user's design: `${X}` marks a
numbered capture group (`$1`, `$2`, ... from the left), `^` stays report-position only, numbers can
no longer name holes, and a rewrite can target groups -- `-r '$1 => fix($1)' -r '$2 => other'`,
one per group, beside the unchanged whole-match `-r TEMPLATE` (the two cannot be mixed). `=>` was
chosen over the user's first suggestion of `s/$1/.../` because Apex writes `/` constantly; only the
first `=>` separates and at most one space either side is dropped, so templates keep their
whitespace and map literals. A group's text is the capture `$N` (usable in templates, `~` and `:`),
and it records its exact span for the rewrite. Without a `^`, group `$1` tells readings apart, so
a group is rewritten everywhere it can land: on NPSP, `void $_(...) { ... ${[SELECT ... FROM $o
...]} ... } -r '$1 => Data.query($o)'` rewrote all 13 queries in `ALLO_Allocations_TEST.cls` with
the methods kept and zero parse errors. Search with a group still reports each match once (810,
unchanged). A group around the whole pattern is refused as meaningless. Guarded by
`a_group_rewrite_touches_only_the_group`, `a_group_in_a_deep_pattern_rewrites_every_place_it_lands`,
`a_group_template_keeps_its_whitespace`, `group_rewrites_are_checked` and
`a_group_works_in_conditions_and_search`.

**30. A child relationship is a `List` of the child.** `opportunity.Payments__r` used to have no
type, so anything called on it (`.isEmpty()`, `.size()`) dead-ended. The child's own lookup field
declares `<relationshipName>`, which nothing parsed: `apex-metadata` now keeps it, `SchemaIndex`
indexes `(parent, relationship) -> child object`, and a field-access miss consults that before
giving up. On NPSP this removes one real `check` error (`Households.cls:145`, `.isEmpty()` on
`accountRecord.Opportunities`), the only line that changed. The gain is narrow there because only
77 NPSP fields name a relationship and most uses are *standard* relationships (`account.Contacts`):
the scraped `standard_objects.json` carries no relationship names at all -- checked in the raw
JSON, per the standing rule about not inferring stdlib data from a struct's shape. Guarded by `a_child_relationship_is_a_list_of_the_child_object`.

Two items listed as open turned out to be done already: SOSL clauses take a hole at every boundary
(item 21's counterpart, verified: `[FIND $q ... RETURNING ... ...]` and `[FIND $q IN $g FIELDS ...]`
both compile and match), and SObject *field* types have worked since the binder's `Ty` layer --
502 of 675 NPSP declarations initialised from a field access are typed.

**31. Standard child relationships are bundled from a describe.** The doc scrape cannot supply
them, and pluralizing the child's name is wrong too often to use: of `Account`'s 85 child
relationships, measured against a real org, 23 are not the child's plural (`ChildAccounts`,
`Shares`, `ProvidedAssets`) and six child objects reach `Account` through more than one
relationship. So `tools/standard-child-relationships` asks the org itself and bundles the answer
as `crates/apex-stdlib/data/standard_child_relationships.json` -- 4,678 relationships on 580
parents, 221 KB, with anything containing `__` dropped on both sides so the file is identical
whatever org generates it. `SchemaIndex` seeds its relationship index with these and lets a
project's own metadata win on a clash. On NPSP this removes 28 further `check` errors (`.size()`
and `.isEmpty()` on child collections) and adds no new diagnostic. Guarded by
`a_standard_child_relationship_is_a_list_of_the_child_object`.