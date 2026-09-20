Type: prototype
Status: resolved

## Question

How does a pattern string become a syntax tree that can be matched against source?

This is the map's highest-risk ticket and everything else waits on it. The map's Notes carry
the load-bearing facts -- read them first, along with §12 and the appendix of
[`research/01-structural-search-syntax-landscape.md`](../research/01-structural-search-syntax-landscape.md).

The problem in one line: **neither `...` nor `$` is an Apex token.** `...` lexes as three
separate `Dot` tokens (`crates/apex-lexer/src/punct.rs:31`), `$` falls through to
`TokenKind::Unknown` (`punct.rs:141`), and since the lexer never fails, a pattern string lexes
cleanly and then parses into garbage rather than erroring usefully.

ast-grep's documented answer is to **preprocess**: substitute each hole for something the
unmodified grammar already accepts (it replaces `$` with an "expando char" so `$X` lexes as an
ordinary identifier), parse with the real parser, then reinterpret the placeholders as holes.
Its ceiling is documented and exact: **holes only work where an identifier is grammatical**.
`$LEFT $OP $RIGHT` fails outright. That ceiling lands directly on this map's motivating
example, because `for (...)` needs a hole standing in for an entire for-header, and
`for (__HOLE__)` matches neither Apex `for` form.

Prototype -- build the cheapest thing that answers this, do not build the matcher:

1. **Find a substitution scheme that parses the corpus.** At minimum get trees for
   `System.debug(...)`, `$X.size() > 0`, `[SELECT ... FROM $OBJ]`, `for (...) { ... }`, and
   `try { ... } catch (...) { }`. Note that holes appear in at least four grammatical positions
   -- argument list, expression, statement run, and for-header -- and they may not all admit
   the same substitution. Report which positions work, which do not, and what the resulting
   trees actually look like (dump them; `apexls ast` already exists).
2. **Decide how a pattern picks its parse entry point.** `parse_expression` / `parse_statement`
   / `parse_block` exist with `ExprRoot` / `StmtRoot` / `BlockRoot` kinds
   (`crates/apex-parser/src/lib.rs:62-84`). Three designs are in the survey: try every entry
   point and take what parses (gogrep), parse into the largest context and walk down single-child
   chains to an "effective node" (ast-grep), or make the user declare the kind (Coccinelle, the
   only design that resolves the ambiguity by being told rather than guessing). Recommend one,
   with the failure mode of each against real corpus patterns. Remember `parse_expression`
   leaves trailing content unconsumed, so success requires checking both `errors.is_empty()`
   *and* that the tree's text covers all of `src` (`lib.rs:56-61`).
3. **State the ceiling honestly.** Which corpus queries cannot be spelled at all under the
   recommended scheme? Corpus item 4 (a swallowing `catch`) is the known one -- `CatchClause` is
   reachable only through `statement()`'s `try` arm, so there is no bare-catch entry point.
   Semgrep's answer to exactly this is a hand-carved list, not a mechanism. Say whether the
   recommendation needs new parser entry points, and how many.
4. **Do not change the lexer to tokenize `...` or `$`.** A pattern-only token in the shared
   lexer would leak into real Apex parsing; if the prototype concludes otherwise, argue it
   explicitly rather than assuming it.

Resolution records: the substitution scheme, the entry-point rule, the list of positions where
a hole is and is not grammatical, any new parser entry points required, and the prototype's
location. The prototype is throwaway -- link it, do not merge it.

## Answer

**Substitution works, and the ceiling is smaller than the survey predicted.** 12 of 14 corpus
patterns parse cleanly into usable trees; the two that do not are both genuine, and both are
now detected by the probe rather than argued in prose. Prototype:
[`crates/apex-parser/examples/pattern_probe.rs`](../../../crates/apex-parser/examples/pattern_probe.rs)
(`cargo run -p apex-parser --example pattern_probe`). It is throwaway -- delete it once the
real matcher exists.

### The scheme: text-level, position-aware substitution

Holes become ordinary Apex identifiers before the unmodified parser ever sees them --
`...` -> `__AP_DOTS__`, `$NAME` -> `__AP_CAP_NAME__`. This is ast-grep's documented approach.
What the probe establishes is that a *uniform* substitution is not enough, and that the extra
rules needed are few, purely textual, and require no tree:

1. **Statement position takes a semicolon.** A hole whose nearest preceding non-whitespace
   character is `{`, `;` or `}` sits where a statement is expected, and a bare identifier is not
   a statement. It expands to `__AP_DOTS__;` instead. This single rule fixed 3 of the 7 initial
   failures -- every `{ ... }` pattern had been dying on `expected Semi`.
2. **Two keyword-introduced slots want a multi-token construct.** Recognised from the keyword
   heading the paren group the hole sits directly inside:
   - `for (...)` -> `__AP_DOTS___T __AP_DOTS___V : __AP_DOTS___C` (the for-each header shape)
   - `catch (...)` -> `__AP_DOTS___T __AP_DOTS___N` (a catch parameter is `Type name`)
3. **Everywhere else** -- argument lists, expressions, SOQL clauses -- a bare identifier is
   correct and sufficient.

Per position, measured rather than assumed:

| Hole position | Expansion | Result |
|---|---|---|
| Argument list (`f(...)`) | bare identifier | parses |
| Expression / operand | bare identifier | parses |
| SOQL clause (`[SELECT ... FROM x]`) | bare identifier | parses |
| Statement run (`{ ... }`) | identifier + `;` | parses |
| For-header (`for (...)`) | `T V : C` shape | parses (for-each only, see below) |
| Catch parameter (`catch (...)`) | `T N` shape | parses |
| Operator position (`$L $OP $R`) | -- | **no expansion works** |

This is exactly the ceiling ast-grep documents ("holes only work where an identifier is
grammatical"), and the finding is that for Apex it is crossed by a **small enumerable set of
slots keyed on the preceding keyword**, not by an open-ended problem. The one position with no
fix is operator position: an operator is not an identifier and no substitution makes it one, so
the spec must reject `$OP`-in-operator-position at compile time with a real error message.

### Entry-point rule: try all three, take the first that consumes the whole pattern

`parse_expression` / `parse_statement` / `parse_block` in that order, accepting a result only
when `errors.is_empty()` **and** the root has exactly one non-trivia child, which is a node.

**The obvious spelling of that second condition does not work, and finding that out is one of
this ticket's results.** Comparing the root's `text_range().len()` against the input length --
which is what the ticket's own framing implies, quoting `lib.rs:56-61` -- is dead code.
`parse_with` (`crates/apex-parser/src/lib.rs:180-183`) completes the root marker over the entire
input and the tree is lossless, so every token lands under the root whether the grammar consumed
it or not. The root range *always* equals the input length; across 14 patterns x 3 entry points
the check never once fired. What discriminates is the root's child list: unconsumed trailing
tokens appear as extra children beside the real node.

The difference is not academic. Under the range check, `System.debug(...);` was accepted as an
**expression** -- root children `[MethodCallExpr, Semi]`, the `;` silently dropped -- so
first-match-wins would have compiled a statement pattern into an expression tree. Under the
child-list check it is correctly rejected as an expression and accepted as a statement. The same
fix turns `$LEFT $OP $RIGHT` from a silent success (a lone `NameExpr` with two orphaned
identifiers trailing it) into a reported failure, which is why the ceiling is now measured by
the instrument instead of asserted in prose.

This is gogrep's multi-entry-point design and it needs no user annotation, so Coccinelle's
declare-the-kind alternative is not required. Genuine ambiguity still occurs -- `{ ... }` parses
as both a statement and a block -- but first-match-wins on a fixed order is well-defined, and
the resulting root kind (`ExprRoot` / `StmtRoot` / `BlockRoot`) tells the matcher what it got.

### One new parser entry point required

`catch (...) { }` is the one *structural* pattern no entry point can parse (the other failure,
`$LEFT $OP $RIGHT`, is the operator-position ceiling above and needs no parser change),
confirming the map's note:
`CatchClause` is reachable only through `statement()`'s `try` arm. Corpus item 4 (a `catch` that
swallows) needs it. The spec requires **one** new entry point -- `parse_catch_clause`, with a
`CatchRoot` kind, mirroring the three that exist -- and no other grammar change. Note the
workaround is real but poor: `try { ... } catch (...) { }` *does* parse, so item 4 is
expressible today only by also matching the `try`, which changes what the query means.

### Carried forward to ticket 02

- **`for (...)` compiles to one shape but means two.** The expansion above produces a
  `ForEachStmt`; the C-style `for (init; cond; update)` form is a different tree. A pattern
  written `for (...)` must compile to **both** and match either, so pattern compilation is
  one-to-many, not one-to-one. This shape (one written pattern, several trees) is also what
  Coccinelle's isomorphisms would need, which is worth knowing before the fog item is specified.
- **The statement-position lookback is a heuristic over raw text.** It is right for every corpus
  pattern, but `}` also ends a block *expression* context and `)` precedes a statement in
  `if (x) ...`, which the current rule would miss. Ticket 02 should either confirm the heuristic
  against a wider pattern set or replace it with a retry: substitute without the semicolon, and
  on `expected Semi` retry with it.
- **The probe never changed the lexer**, per the ticket's constraint. No pattern-only token was
  added, so nothing leaks into real Apex parsing.
