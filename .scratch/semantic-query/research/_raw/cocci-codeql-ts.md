# Structural Search-and-Replace Tools: Research Findings

Research against PRIMARY documentation only (official docs, manuals, source). Covers Coccinelle/SmPL, CodeQL, and the tree-sitter query language.

---

## 1. Coccinelle / SmPL — most thorough coverage

Primary sources used:
- https://coccinelle.gitlabpages.inria.fr/website/docs/ (SmPL grammar manual, chapters `main_grammar001.html`–`main_grammar017.html`)
- https://coccinelle.gitlabpages.inria.fr/website/standard.iso.html (the shipped isomorphism file)
- https://docs.kernel.org/dev-tools/coccinelle.html (kernel integration doc)
- Linux kernel tree: `raw.githubusercontent.com/torvalds/linux/master/scripts/coccinelle/...`

### 1.1 Verbatim semantic patches

**(a) Function renaming** (simplest possible SmPL rule — no metavariable header needed)
Source: https://coccinelle.gitlabpages.inria.fr/website/docs/main_grammar016.html
```
@@
@@
- foo()
+ bar()
```

**(b) Removing a function argument, with a named rule and metavariable inheritance**
Source: same page
```
@ rule1 @
identifier fn;
identifier irq, dev_id;
typedef irqreturn_t;
@@
static irqreturn_t fn (int irq, void *dev_id)
{
...
}

@@
identifier rule1.fn;
expression E1, E2, E3;
@@
fn(E1, E2
- ,E3
 )
```
Note `rule1.fn` — the second rule inherits the `fn` identifier metavariable bound by the first rule (`rule1`), so the removal only applies to arguments of the *same* function matched by rule1's signature.

**(c) Introducing the `DIV_ROUND_UP` macro (disjunction `( ... | ... )` + `depends on`)**
```
@ haskernel @
@@
#include <linux/kernel.h>

@ depends on haskernel @
expression n,d;
@@
(
- (((n) + (d)) - 1) / (d)
+ DIV_ROUND_UP(n,d)
|
- (((n) + ((d) - 1)) / (d))
+ DIV_ROUND_UP(n,d)
)
```
`haskernel` is a rule with no body pattern — it just checks the file already `#include`s `linux/kernel.h`; the second rule only fires if `haskernel` matched (`depends on haskernel`).

**(d) `BUG_ON` introduction (disjunction guarding against `unlikely()`)**
```
@@
expression E,f;
@@
(
if (<+... f(...) ...+>) { BUG(); }
|
- if (E) { BUG(); }
+ BUG_ON(E);
)

@ disable unlikely @
expression E,f;
@@
(
if (<+... f(...) ...+>) { BUG(); }
|
- if (unlikely(E)) { BUG(); }
+ BUG_ON(E);
)
```

**(e) Generic NULL-dereference-in-branch detector (`*` = "report here", `when != …`, `when any`)**
```
@@
expression E, E1;
identifier f;
statement S1,S2,S3;
@@
* if (E == NULL)
{
... when != if (E == NULL) S1 else S2
when != E = E1
* E->f
... when any
return ...;
}
else S3
```
The leading `*` marks lines of interest for `report`/`org` mode display rather than performing a `-`/`+` rewrite.

**(f) Real, production semantic patch from the Linux kernel tree: `scripts/coccinelle/free/kfree.cocci`** (use-after-free detector — full file, verbatim)
Source: https://raw.githubusercontent.com/torvalds/linux/master/scripts/coccinelle/free/kfree.cocci
```
// SPDX-License-Identifier: GPL-2.0-only
/// Find a use after free.
//# Values of variables may imply that some
//# execution paths are not possible, resulting in false positives.
//# Another source of false positives are macros such as
//# SCTP_DBG_OBJCNT_DEC that do not actually evaluate their argument
///
// Confidence: Moderate
// Copyright: (C) 2010-2012 Nicolas Palix.
// Copyright: (C) 2010-2012 Julia Lawall, INRIA/LIP6.
// Copyright: (C) 2010-2012 Gilles Muller, INRIA/LiP6.
// URL: https://coccinelle.gitlabpages.inria.fr/website
// Comments:
// Options: --no-includes --include-headers

virtual org
virtual report

@free@
expression E;
position p1;
@@

(
 kfree@p1(E)
|
 kfree_sensitive@p1(E)
)

@print expression@
constant char [] c;
expression free.E,E2;
type T;
position p;
identifier f;
@@

(
 f(...,c,...,(T)E@p,...)
|
 E@p == E2
|
 E@p != E2
|
 E2 == E@p
|
 E2 != E@p
|
 !E@p
|
 E@p || ...
)

@sz@
expression free.E;
position p;
@@

 sizeof(<+...E@p...+>)

@loop exists@
expression E;
identifier l;
position ok;
@@

while (1) { ...
(
 kfree@ok(E)
|
 kfree_sensitive@ok(E)
)
  ... when != break;
      when != goto l;
      when forall
}

@r exists@
expression free.E, subE<=free.E, E2;
expression E1;
iterator iter;
statement S;
position free.p1!=loop.ok,p2!={print.p,sz.p};
@@

(
 kfree@p1(E,...)
|
 kfree_sensitive@p1(E,...)
)
...
(
 iter(...,subE,...) S // no use
|
 list_remove_head(E1,subE,...)
|
 subE = E2
|
 subE++
|
 ++subE
|
 --subE
|
 subE--
|
 &subE
|
 BUG(...)
|
 BUG_ON(...)
|
 return_VALUE(...)
|
 return_ACPI_STATUS(...)
|
 E@p2 // bad use
)

@script:python depends on org@
p1 << free.p1;
p2 << r.p2;
@@

cocci.print_main("kfree",p1)
cocci.print_secs("ref",p2)

@script:python depends on report@
p1 << free.p1;
p2 << r.p2;
@@

msg = "ERROR: reference preceded by free on line %s" % (p1[0].line)
coccilib.report.print_report(p2[0],msg)
```
This is a genuinely instructive example: it uses `iterator` metavariables, position sets with exclusion (`position free.p1!=loop.ok,p2!={print.p,sz.p};`), an `exists`-mode subordinate rule to whitelist a `while(1)` loop-with-break pattern, and two `@script:python@` reporting rules keyed off `depends on org` / `depends on report`.

I also fetched `scripts/coccinelle/null/deref_null.cocci` (full text confirmed present, real-world NULL-dereference-under-test detector using `pr1`/`pr2`/`ifm` sub-rules and the same context/org/report triple).

### 1.2 Metavariable declaration block — kinds that exist
Source: https://coccinelle.gitlabpages.inria.fr/website/docs/main_grammar002.html

`@@ expression E; identifier f; type T; statement S; @@` is the general shape. Documented kinds:
- **expression** — matches anything conforming to the C99 expression grammar; can be constrained by type (`expression@T`, or `struct foo *E`), pointer level (`*`).
- **identifier** — names: struct fields, macros, function names, variable names (not the value, the token).
- **type** — a type as it would appear in a function signature, declaration, cast, or `typedef`.
- **statement** — matches anything conforming to the C99 statement grammar.
- **declaration** — a variable declaration; groups of variables sharing one type spec (e.g. `int a,b,c=3;`).
- **idexpression** (and **local idexpression**) — a variable used as an expression, optionally restricted to `local` (block-scope) or `global` scope, with optional ctype constraint.
- **constant** — numeric or all-uppercase-identifier constants ("names given to macros in Linux usually have this form").
- **position** — attaches to a token with `@p` notation; captures file/line info, used to correlate matches across rules and to drive `report`/`org` output.
- **parameter** — matches a function *parameter* declaration specifically (distinct from a call-site *argument*).
- **field** — matches struct field identifiers only.
- **iterator** — a macro used in place of a loop-statement header (e.g. `list_for_each`) that itself generates an iteration construct — used heavily in kernel code (`iter` in the kfree.cocci example above).
- **declarer** — analogous macro-based abstraction on the declaration side.

Modifiers: comparison constraints (`!=`), regexp constraints, `script:` constraints (arbitrary OCaml/Python boolean check), and a `list` modifier to match a run of consecutive elements as one metavariable.

### 1.3 `...` semantics and `when` constraints
Source: https://coccinelle.gitlabpages.inria.fr/website/docs/main_grammar004.html (verbatim quotes below)

> "Ellipses ('...') can be used to indicate to Coccinelle that anything can be present in a control-flow graph path between matches of two statements."

- **In a function body / statement sequence**: `...` means "any sequence of statements along some/all control-flow paths between the two anchor points," subject to the rule's `exists`/`forall` mode (see below).
- **In an argument list**: `f(...)` means "any arguments, zero or more" — purely syntactic, not control-flow.
- **In an expression**: similarly, "any subexpression" is elided.

**`when` constraints** (exact semantics):
- `... when != x` — the elided region must **not** contain a match of `x` anywhere along the path. Used in kfree.cocci: `... when != break; when != goto l;` — the `while(1)` body must not break or goto out.
- `... when exists` — overrides the rule's default match mode for this specific ellipsis: succeed if *any* control-flow path satisfies the pattern (rather than requiring all paths to).
- `... when forall` — the complementary override: *all* control-flow paths must satisfy the pattern. Used at the end of the `loop` rule: `when forall` (all paths through the `while(1)` loop must exhibit the free-then-no-escape shape).
- `... when any` — relaxes an implicit "no matches of already-bound metavariables" default, allowing arbitrary code (including further matches of already-seen metavariables) inside the ellipsis. Seen in example (e): `... when any` right before `return ...;`.

**Rule-level mode** (`@r@` vs `@r exists@` vs default): by default a rule must match on *all* possible control-flow paths (this is effectively `forall` mode); annotating the rule name with `exists` (e.g. `@loop exists@`, `@r exists@` in kfree.cocci) switches the rule to succeed if *any* path matches — used for loops/branches where only one arm needs to exhibit the buggy pattern.

**`<... ...>` and `<+... ...+>`** (verbatim from the doc):
> "one (`<... ...>`) indicates that matching the pattern in between the ellipses is to be matched 0 or more times, i.e., it is optional, and another (`<+... ...+>`) indicates that the pattern in between the ellipses must be matched at least once [on some control-flow path]."

`<... ...>` = "optional nested match anywhere within" (0+ occurrences); `<+... ...+>` = "must occur at least once somewhere within" — both used above: `sizeof(<+...E@p...+>)` (E must appear somewhere, at least once, inside the `sizeof(...)` argument — handles arbitrarily nested expressions), and `if (<+... f(...) ...+>) { BUG(); }` (a call to `f` must occur somewhere in the `if` condition, however deeply nested).

### 1.4 Isomorphisms (`standard.iso`)
Source: https://coccinelle.gitlabpages.inria.fr/website/standard.iso.html

This is the mechanism whereby a single written pattern like `x == NULL` transparently also matches semantically-equivalent surface forms. Verbatim rule shapes from the file:
```
X == NULL <=> NULL == X <=> !X     (rule name: is_null)
X != NULL <=> NULL != X            (rule name: isnt_null1)
!X <=> X == NULL                   (not_ptr2, integer/pointer variants split)
!X <=> 0 == X   /  !X <=> X == 0   (not_int1 / not_int2, for integer-typed operands)
```
Mechanism: `standard.iso` is a separate SmPL-syntax file (default path `/usr/local/share/coccinelle/standard.iso`, or wherever `--iso-file`/`--include` points) containing named isomorphism rules using `<=>` (bidirectional) and `=>` (unidirectional) equivalence operators between concrete syntactic forms. It is loaded automatically for every semantic patch unless disabled. Individual semantic patches can turn off specific isomorphisms with `@ rulename disable is_null @` (the `disable` keyword followed by isomorphism rule name(s), as seen in example (d) above: `@ disable unlikely @`). The doc notes rule *order* matters ("As we don't do a fixpoint, changing the order may impact the result") since isomorphism expansion is not iterated to a fixed point.

### 1.5 Whitespace / indentation / comment handling
- The kernel doc and grammar manual describe Coccinelle's output as diff-based: unmatched code is emitted byte-for-byte unchanged; only the `-`/`+` lines are literally rewritten text, everything else (comments, blank lines, indentation) around a match is preserved verbatim because Coccinelle's "unparser" re-emits the original token stream except where a `-`/`+` substitution was made.
- I was unable to retrieve a specific primary-source page documenting `--smpl-spacing` in detail (web search on the exact flag name returned no matching official-doc hits); the `options.pdf` manual (https://coccinelle.gitlabpages.inria.fr/website/docs/options.pdf) is the canonical location for spatch CLI flags including spacing-related ones, but I could not fetch its content directly through WebFetch (PDF). Flag it as something to verify by rendering `spatch --help` or the options PDF directly rather than relying on my summary here.
- Practical behavior widely documented in the kernel workflow docs: because unmatched regions are untouched, semantic patches are safe to run repo-wide and produce minimal, reviewable diffs (this is the primary operational reason the kernel uses Coccinelle instead of ad hoc scripts).

### 1.6 Named rules, inheritance, position variables, scripting
Sources: main_grammar002.html, main_grammar003.html
- `@r@ ... @@ pattern` declares rule name `r`; a later rule can write `identifier r.fn;` or `expression free.E;` to **inherit** a metavariable bound by rule `r` — this is how kfree.cocci's `@r exists@` rule reuses `free.E` (the expression freed) and `free.p1` (its position) from the `@free@` rule, and excludes the position already accepted by the `@loop@` rule (`position free.p1!=loop.ok,...`).
- `depends on r` / `depends on ever r` / `depends on never r` gate a rule on whether another named rule matched (at least once / never, respectively).
- **Position variables** (`@p` suffix on a metavariable declared `position p;`): attach to a matched token to record its source location; used both to correlate the same program point across multiple rules and to drive `--generate-org`/`--generate-report` style output via `cocci.print_main(...)` / `coccilib.report.print_report(...)`.
- **`@script:python@` / `@script:ocaml@` rules**: a scripting rule inherits metavariables from earlier rules with `id << rulename.id;` syntax (optionally `id << rulename.id = "default";` for a default value when unbound, or `= []` for a list default), executes only once every non-defaulted inherited metavariable is bound, and can call into `coccilib`/`cocci` helper APIs (`cocci.print_main`, `coccilib.report.print_report`) — exactly as used in kfree.cocci's two `@script:python depends on org@` / `depends on report@` rules. OCaml scripts additionally support pulling both the string and parsed-AST form of a metavariable: `(id,id) << rulename.id;` (either half replaceable with `_`). There are also special `initialize:` / `finalize:` script blocks that run once before/after all file processing and can't see SmPL metavariables, only virtual ones and script-set globals.

### 1.7 Known complaints (from docs + surrounding ecosystem)
- **C-only**: Coccinelle's grammar and parser are built around the C99 grammar (per main_grammar006–010's explicit "conforms to the C99 [expression/statement/type] grammar" language) — it has no support for C++-specific constructs, and no support at all for other languages (Java, Python, JS, etc.).
- **Macro parsing failures**: search results confirm Coccinelle "sometimes doesn't recognize or parse complex macro variables due to insufficient definition"; the fix is to give it an explicit prototype via `--macro-file-builtins <headerfile.h>` (the kernel ships `standard.h` for exactly this, used automatically), or `--macro-file` to supply extra macro definitions. Coccinelle explicitly **does not expand preprocessor directives** during matching (this is by design — it matches the pre-preprocessed token stream — but it means undeclared macros can desynchronize the parser).
- **Performance**: the kernel doc documents `J=<n>` for `spatch`/`coccicheck` parallelism, and notes 1.0.2+ supports dynamic load balancing via OCaml `parmap` with `--chunksize 1`; running full-tree semantic patches remains slow enough that kernel workflow docs recommend scoping with `M=<dir>` or `COCCI=<file>` rather than running the whole `coccicheck` target routinely.
- General caveat quoted directly from the kernel doc: "As with any static code analyzer, Coccinelle produces false positives. Thus, reports must be carefully checked, and patches reviewed."

Source: https://docs.kernel.org/dev-tools/coccinelle.html

---

## 2. CodeQL

Primary sources: https://codeql.github.com/docs/codeql-overview/about-codeql/, https://codeql.github.com/docs/codeql-language-guides/basic-query-for-cpp-code/, https://codeql.github.com/docs/codeql-language-guides/expressions-types-and-statements-in-cpp/, https://codeql.github.com/docs/ql-language-reference/queries/, https://codeql.github.com/docs/ql-language-reference/types/, https://codeql.github.com/docs/writing-codeql-queries/about-data-flow-analysis/, https://codeql.github.com/docs/ql-language-reference/recursion/

### 2.1 Why no pattern-literal syntax — the relational/database model
Verbatim from "About CodeQL" (https://codeql.github.com/docs/codeql-overview/about-codeql/):
> CodeQL extracts "a single relational representation of each source file" stored in "a full, hierarchical representation of the code, including a representation of the abstract syntax tree, the data flow graph, and the control flow graph."
> Each language has "its own unique database schema that defines the relations used to create a database."
> "the CodeQL libraries define classes to provide a layer of abstraction over the database tables. This provides an object-oriented view of the data which makes it easier to write queries."
> CodeQL uses "a specially-designed object-oriented query language called QL" to analyze this data.

There is no "write example code, get a pattern" mode at all — you never write source-shaped syntax with holes in it; you write relational-algebra-style predicates over AST/CFG/dataflow tables, because the underlying representation *is* a database, not a token stream to be re-matched.

### 2.2 Verbatim `from ... where ... select` examples

**Basic query structure**, from https://codeql.github.com/docs/ql-language-reference/queries/:
```
from int x, int y
where x = 3 and y in [0 .. 2]
select x, y, x * y as product, "product: " + product
```
And a query predicate (reusable, `query`-annotated) form:
```
query int getProduct(int x, int y) {
  x = 3 and
  y in [0 .. 2] and
  result = x * y
}
```

**Class definition** (the object-oriented layer over relational tables), from https://codeql.github.com/docs/ql-language-reference/types/:
```
class OneTwoThree extends int {
  OneTwoThree() { // characteristic predicate
    this = 1 or this = 2 or this = 3
  }

  string getAString() { // member predicate
    result = "One, two or three: " + this.toString()
  }

  predicate isEven() { // member predicate
    this = 2
  }
}
```

**Real C++ query** — redundant empty `if`, from https://codeql.github.com/docs/codeql-language-guides/basic-query-for-cpp-code/ (shown as an iterative refinement, which is itself instructive about the query-writing workflow):
```
from IfStmt ifstmt, BlockStmt block
where ifstmt.getThen() = block and
  block.getNumStmt() = 0
select ifstmt, "This 'if' statement is redundant."
```
refined to exclude false positives where an `else` exists:
```
from IfStmt ifstmt, BlockStmt block
where ifstmt.getThen() = block and
  block.getNumStmt() = 0 and
  not ifstmt.hasElse()
select ifstmt, "This 'if' statement is redundant."
```

### 2.3 AST-shaped querying — "for loop containing X" equivalent
Source: https://codeql.github.com/docs/codeql-language-guides/expressions-types-and-statements-in-cpp/ (verbatim, real doc examples — closest primary-source analogue to "a for loop containing a `System.debug` call"; these detect an assignment-of-zero inside a `for` loop's init vs. body respectively, using exactly the AST containment idiom you'd use for a call-inside-a-loop check):

Assignment in the `for` loop **initialization**:
```
import cpp

from AssignExpr e, ForStmt f
// the assignment is in the 'for' loop initialization statement
where e.getEnclosingStmt() = f.getInitialization()
  and e.getRValue().getValue().toInt() = 0
  and e.getLValue().getType().getUnspecifiedType() instanceof IntegralType
select e, "Assigning the value 0 to an integer, inside a for loop initialization."
```

Assignment anywhere in the `for` loop **body** (this is the general "X anywhere nested inside a for loop" pattern, using the `*` transitive-closure operator on `getParentStmt` to mean "at any nesting depth"):
```
import cpp

from AssignExpr e, ForStmt f
// the assignment is in the for loop body
where e.getEnclosingStmt().getParentStmt*() = f.getStmt()
  and e.getRValue().getValue().toInt() = 0
  and e.getLValue().getType().getUnderlyingType() instanceof IntegralType
select e, "Assigning the value 0 to an integer, inside a for loop body."
```
To adapt this to "a `ForStmt` containing a `System.debug` call" in Apex/Java terms, the idiom is identical: swap `AssignExpr e` for `MethodAccess m` (with `m.getMethod().hasName("debug")` or similar), and swap the containment test for `m.getEnclosingStmt().getParentStmt*() = f.getStmt()` (or the library's `getAChild*()` transitive-closure predicate over the loop's AST subtree — confirmed as a real predicate via https://codeql.github.com/docs/ql-language-reference/recursion/, which documents `+` = "the predicate applied one or more times" and `*` = "the predicate applied zero or more times," giving `p.getAParent+()`/`p.getAParent*()` as the canonical ancestor/descendant-closure idiom the standard libraries expose throughout, e.g. `getAChild*()`).

### 2.4 Does it rewrite at all?
No — confirmed from https://codeql.github.com/docs/codeql-overview/about-codeql/: the tool's three-step model is *database creation → query execution → result interpretation*, with "no mention of automatic code remediation" anywhere in that page. CodeQL itself only ever *selects* (reports) — it has no `-`/`+` or transform primitive in the query language at all. (GitHub's separate "Copilot Autofix" feature layers an LLM suggestion on top of CodeQL alert output, but that is explicitly a different, non-CodeQL-language mechanism, and even its suggestions "still require review and testing" per GitHub's own materials — CodeQL proper never edits source.)

### 2.5 What it buys: dataflow / taint / type resolution
Verbatim from https://codeql.github.com/docs/writing-codeql-queries/about-data-flow-analysis/:
> Data flow analysis computes "the possible values that a variable can hold at various points in a program, determining how those values propagate through the program and where they are used."
> Normal data flow analyzes "the information flow in which data values are preserved at each step."
> Taint tracking extends this to model cases where "data values are not necessarily preserved, but the potentially insecure object is still propagated" (e.g. `y = x + 1` still taints `y` from `x`).
> Analysis operates at two levels — "local" (single function, fast/precise) and "global" (cross-function/cross-object, more comprehensive, more expensive).

This is the capability a syntactic tool like Coccinelle or tree-sitter fundamentally cannot offer: CodeQL's relational database includes precomputed dataflow-graph and (for typed languages) resolved-type relations, so a query can ask "does untrusted input reach this sink through any assignment chain" — a semantic question, not a syntactic pattern.

### 2.6 Ergonomic complaint: learning curve / verbosity vs. pattern literals
The docs themselves don't editorialize about difficulty (I found no primary-source line admitting this), but the *structure itself* makes the tradeoff self-evident from the examples above: where Coccinelle expresses "assignment of 0 inside a for-loop body" as a 3-token diff hunk with `...` doing the traversal implicitly, the equivalent CodeQL query requires declaring two typed variables, writing an explicit `getEnclosingStmt().getParentStmt*() = f.getStmt()` containment predicate, and a separate type-compatibility clause (`instanceof IntegralType`) — i.e. every piece of "obviously nested inside a loop" structure that SmPL/tree-sitter give you for free from parentheses/`...` must be spelled out as named predicate calls over the class library. This is the standard, well-documented tradeoff: CodeQL buys semantic precision (dataflow, types) at the cost of needing to know the relevant class/predicate names and writing full relational-algebra style formulas instead of a source-shaped literal.

---

## 3. tree-sitter query language

Primary sources: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html, .../2-operators.html, .../3-predicates-and-directives.html

### 3.1 Verbatim S-expression query examples

Basic node/child/capture form:
```
(assignment_expression
  left: (member_expression object: (call_expression))
)
```
Capturing a node with `@`:
```
(assignment_expression
  left: (identifier) @the-function-name
  right: (function))
```
**Field names** — `field:` prefix narrows a child pattern to a specific grammar field, e.g. `left: (member_expression object: (call_expression))` above requires the match be specifically in the `left` field position, and specifically have an `object` field that is itself a `call_expression`.

**Negated fields** — `!field` requires the node to *lack* that field entirely, e.g. `!type_parameters` matches only nodes with no `type_parameters` field present.

**Anonymous nodes** — literal tokens matched as quoted strings rather than a parenthesized node type: `operator: "!="`, `right: (null)`.

**Wildcards** — `(_)` matches any *named* node (any node type, but must be a "named" grammar node, not an anonymous punctuation token); bare `_` matches literally any node, named or anonymous.

**ERROR / MISSING nodes** — `(ERROR)` captures parser-error nodes; `(MISSING)` and `(MISSING identifier)` detect nodes the parser synthesized/inserted during error recovery.

**Supertypes** — grammar-level union types can be matched directly, e.g. `(expression)`, or narrowed with `expression/binary_expression` syntax to mean "a binary_expression matched via the expression supertype slot."

### 3.2 Quantifiers, alternations, anchors, grouping
Source: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/2-operators.html
```
(comment)+                    ; one or more
(decorator)* @the-decorator   ; zero or more
(string)? @the-string-arg     ; optional (zero or one)
```
Grouping sibling sequences with plain parentheses:
```
(
  (number)
  ("," (number))*
)
```
Alternations with square brackets:
```
(call_expression
  function: [
    (identifier) @function
    (member_expression property: (property_identifier) @method)
  ])
```
Anchors with `.`:
- Leading `.` — must be the first named child: `(array . (identifier) @the-element)`
- Trailing `.` — must be the last named child: `(block (_) @last-expression .)`
- Between two sibling patterns — requires immediate adjacency: `(identifier) @prev-id . (identifier) @next-id`
- Anchors compose with quantifiers; a zero-match quantified node imposes no adjacency constraint when it matches nothing.

### 3.3 Predicates — NOT handled by the query engine itself
Source: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/3-predicates-and-directives.html — this is an explicit, important, and verbatim-quotable point:
> "Predicates and directives are not handled directly by the Tree-sitter C library. They are just exposed in a structured form so that higher-level code can perform the filtering."

That is, the core Tree-sitter query engine parses and *recognizes* `#eq?`/`#match?`/etc. syntactically but does not evaluate them — evaluation is left to the language binding/caller (Rust crate, WASM binding, editor integration, etc.), each of which implements the filtering itself against the raw match results the core engine returns.

Documented predicate families:
- `#eq?` / `#not-eq?` / `#any-eq?` / `#any-not-eq?` — "The first argument to this predicate must be a capture, but the second can be either a capture" (to compare two captures' text against each other) "or a string" (to compare one capture's text against a literal).
- `#match?` / presumably `#not-match?` — regex match: "The first argument must be a capture, and the second must be a string containing a regular expression."
- `#any-of?` — "will match if the capture's text is equal to any of the strings" — and performs **exact string** matching, not regex.
- The `any-` prefix generally: "you can prefix either of these with `any-` to match if any of the nodes match the predicate. This is only useful when dealing with quantified captures" (i.e. a capture bound to multiple nodes via `+`/`*`) — by default a quantified capture requires *all* captured nodes to satisfy the predicate; `any-` relaxes that to *at least one*.
- Directives (side-effecting, non-filtering): `#set!` (attach metadata to a match), `#select-adjacent!` (filter to adjacent nodes), `#strip!` (regex-based text stripping).

### 3.4 Capture naming and back-reference confirmation
Confirmed by the `#eq?` doc text above: two captures sharing a name do **not** automatically unify/back-reference each other in the query engine — the query engine just returns every node bound to a given capture name in the match. To require two captures to have *equal text* (the "same identifier used twice" pattern), you write it explicitly as a predicate: `(#eq? @a @b)`, comparing the two capture's text content as a post-match filter (again, evaluated by the caller, not the core engine, per 3.3).

### 3.5 No rewrite side at all
Confirmed: nothing in the syntax (1-syntax.html), operators (2-operators.html), or predicates/directives (3-predicates-and-directives.html) pages defines any replacement/transform primitive. The query language's only output is a set of (pattern, captures) matches. Directives like `#set!`/`#strip!` only annotate or locally massage the *matched text a caller already extracted* for its own display/highlighting purposes (e.g. syntax highlighting themes use `#set!` to attach a highlight-group name) — they do not rewrite the source tree or produce a patch. Any actual code rewriting using tree-sitter (e.g. codemods) is entirely the responsibility of code built on top of the query API, using the returned node byte-ranges to splice text — tree-sitter itself provides zero rewrite machinery.

### 3.6 Contrast with pattern-literal approaches
- **Verbosity**: even a simple "capture a function's name" query is an S-expression with explicit field names (`left:`, `right:`), not a source-shaped literal with a hole — compare Coccinelle's `- foo()` / `+ bar()` needing zero knowledge of the parse-tree node type names at all.
- **Must know node type names**: every tree-sitter query requires the author to already know the target grammar's exact node/field vocabulary (`assignment_expression`, `member_expression`, `property_identifier`, etc.) — this is a hard prerequisite, whereas SmPL/Coccinelle patterns are written as ordinary C source syntax that "looks like" the target code, requiring no grammar-internals knowledge, and CodeQL similarly hides node-type detail behind named library classes (`ForStmt`, `AssignExpr`) rather than raw grammar rule names.
- **No fragment-parsing problem**: because tree-sitter queries are S-expressions matched directly against an already-produced concrete syntax tree (not re-parsed as a standalone code fragment), there's no risk of a pattern like `if (E) { ... }` being ambiguous or failing to parse as valid top-level C the way a naive "pattern is also source code" tool might struggle with a bare expression or partial statement — the query language is a wholly separate grammar from the target language grammar, purpose-built for tree matching, so it never needs to guess what surrounding context would make a fragment syntactically legal.

---

## Summary comparison

| | Coccinelle/SmPL | CodeQL | tree-sitter query |
|---|---|---|---|
| Pattern form | Target-language source with `-`/`+`/metavariables | Relational query language (QL) over AST/CFG/dataflow DB | S-expression over concrete syntax tree |
| Needs grammar node names? | No (patterns look like real code) | No (hidden behind class library) | Yes (must know node/field names) |
| Rewrite capability | Yes — native `-`/`+` diff generation, this is its core purpose | No — analysis/reporting only; rewrites (if any) come from separate LLM-based Autofix layered on top | No — matching only, rewriting is entirely caller-built |
| Dataflow/taint/type resolution | No | Yes — first-class, this is its main value-add | No |
| Predicate evaluation | Built into the engine (`when`, script rules) | Built into the engine (it's the whole language) | NOT built into the core engine — pushed to the caller |
| Language scope | C only (kernel-focused) | Many languages, per-language schema/library | Any tree-sitter-parsed language, uniform query syntax |

All source URLs are cited inline above next to each claim.

## Known gap

I could not retrieve primary-source detail on the exact `--smpl-spacing` flag semantics (web search on the exact flag name returned no matching official-doc hits, and the canonical CLI options manual, https://coccinelle.gitlabpages.inria.fr/website/docs/options.pdf, is a PDF I couldn't fetch through WebFetch). Worth verifying directly via `spatch --help` or that options PDF if precise flag behavior matters for the design doc.
