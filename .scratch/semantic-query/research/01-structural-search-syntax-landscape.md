# Structural search-and-replace: the syntax landscape

**Question.** What syntaxes exist for semantic/structural search-and-replace over source code, and what are their ergonomic and expressive trade-offs?

**Why.** Input to the design of an `apexls query` subcommand: a pattern language that matches Apex *structurally* and eventually rewrites matches. The motivating example is `for (...) { ... System.debug(...); ... }`.

**Method.** Primary sources only — each tool's own docs site, manual, grammar reference, or shipped source. Secondary sources are used only where noted and are labelled. Where a fetch returned a paraphrase rather than a quotable sentence, that is flagged inline rather than presented as verbatim. Source URLs are cited next to each claim.

**Reading order.** Sections 1–11 are per tool. Section 12 answers the four cross-cutting design questions. Section 13 distils the axes of variation — that is the section to argue from. No recommendation is made anywhere.

---

## Contents

| § | Tool | Pattern form | Rewrites? | Semantic? |
|---|---|---|---|---|
| 1 | Comby | code + `:[hole]` | yes | no (by default) |
| 2 | Semgrep | code + `...` / `$X` | yes (`fix:`) | partial |
| 3 | ast-grep | code + `$X` / `$$$` | yes (`fix:`) | explicitly no |
| 4 | GritQL | backticked code + `=>` | yes | not claimed |
| 5 | Coccinelle / SmPL | code as a diff (`-`/`+`) | yes — its whole point | CFG-level, not type-level |
| 6 | CodeQL | relational query language | no | yes — the point |
| 7 | tree-sitter queries | S-expressions | no | no |
| 8 | JetBrains SSR | code + `$var$` + filter dialogs | yes | yes (PSI) |
| 9 | gogrep | code + `$x` / `$*_` | yes (`-s`/`-w`) | some (`-a 'type(string)'`) |
| 10 | Refaster | **real compilable Java** | yes | yes — javac does it |
| 11 | PMD XPath | XPath over AST | no | some (helper functions) |

---

## 1. Comby

*Sources: <https://comby.dev/docs/syntax-reference>, <https://comby.dev/docs/basic-usage>, <https://comby.dev/docs/advanced-usage>, <https://comby.dev/docs/configuration>, <https://comby.dev/docs/faq>, <https://github.com/comby-tools/comby>*

### 1.1 Example patterns (verbatim)

CLI match/rewrite form (<https://comby.dev/docs/basic-usage>):

```
comby 'fmt.Println(:[args])' 'fmt.Println(fmt.Sprintf("comby says %s", :[args]))' .go
```

The README's headline example — constant-folding `if` conditions (<https://github.com/comby-tools/comby>):

```
if (:[condition])
```
→
```
if (1)
```

A `.toml` config pattern, match and rewrite together with a rule (<https://comby.dev/docs/configuration>):

```toml
[my-second-pattern]
match='''
function :[[fn]](:[1], :[2]) {
  :[body]
};'''

rewrite='''
function :[[fn]](:[2], :[1]) {
  :[body]
};'''

rule='where :[fn] != "divide"'
```

Equality constraint between two holes (<https://comby.dev/docs/advanced-usage>):

```
if (:[left_side] && :[right_side])
where :[left_side] == :[right_side]
```

Conjunction of `where` conditions — comma means AND (same page):

```
where :[left_side] == :[right_side], :[left_side] != "x == 500"
```

A nested rewrite rule, turning Python kwargs into a dict literal (same page):

```
dict(:[args])
where rewrite :[args] { ":[[k]]=:[[v]]" -> "\":[k]\": :[v]" }
```

Switch-style rule with a regex arm (same page):

```
where match :[hole] {
| ":[_~\\d+]" -> true
| ":[_]" -> false
}
```

Fresh-identifier generation (same page):

```
var a_:[id()] = 42
anon_:[id(my_label)] = func(){:[body]}
```

### 1.2 How "match anything" is spelled

Comby has **one hole mechanism**, and granularity is inferred from *where the hole sits* rather than from a different operator. The syntax reference (<https://comby.dev/docs/syntax-reference>) gives these forms:

| Form | Meaning (verbatim) |
|---|---|
| `:[var]` | "match zero or more characters in a lazy fashion" |
| `:[var~regex]` | "match an arbitrary PCRE regular expression" |
| `:[[var]]` | "match one or more alphanumeric characters and `_`" |
| `:[var:e]` | "Expression-like syntax matches contiguous non-whitespace characters" |
| `:[var.]` | "match one or more alphanumeric characters and punctuation" |
| `:[var\n]` | "match zero or more characters up to a newline, including the newline" |
| `:[ var]` | "match only whitespace characters, excluding newlines" |
| `:[_]`, `:[~regex]`, `:[_:e]` | unnamed ("just match") variants of the above |

The rule that does the real work, verbatim from the same page:

> "When used is inside delimiters, as in `{:[v1], :[v2]}` or `(:[v])`, holes match within that group or code block, including newlines. Holes outside of delimiters stop matching at a newline, or the start of a code block, whichever comes first."

So:

- **one expression** → `:[x]` outside delimiters (bounded by newline / block start);
- **a sequence of statements** → the *same* `:[x]`, placed inside `{ }`;
- **a sequence of arguments** → the same `:[x]`, placed inside `( )`;
- **arbitrary nesting depth** → free: balanced-delimiter tracking walks nested groups, so a hole spanning `{ … }` already spans everything nested inside.

There is no distinct sequence operator and no distinct deep operator. That is the entire design.

### 1.3 Captures and back-reference

Named holes are `:[var]`; the alphanumeric-only variant `:[[var]]` is the identifier form. Numbered holes (`:[1]`, `:[2]`) exist, as the `.toml` example shows.

**Back-reference is not documented.** Direct fetches of both `syntax-reference` and `basic-usage` turned up no statement that reusing a hole name in the *match* template forces the two occurrences to be equal, and no example that reuses a hole name on the match side. What the docs *do* document is the explicit rule form `where :[left_side] == :[right_side]`. Treat "reuse implies unification" as **unconfirmed for Comby** — it has an explicit equality clause instead, which is a genuinely different design point from every other tool here.

### 1.4 Constraints beyond shape

- Regex on a hole: `:[var~regex]` (PCRE), inline in the pattern.
- Rule-level equality / inequality: `where :[a] == :[b]`, `where :[a] != "literal"`, comma-chained as AND.
- `match { … -> … }` multi-way branching on a hole's captured text, including regex arms.
- Nested `rewrite :[x] { … -> … }` sub-rewrites — but "It is not currently possible to nest rewrite statements" (<https://comby.dev/docs/advanced-usage>).
- **No containment operator.** Nothing equivalent to `pattern-not-inside` / `within` / `inside` surfaced in the docs. Comby's rule language is match / rewrite / equality / regex — not a positional-containment DSL.
- **No count/repetition operator.**

### 1.5 Rewrite, and formatting

The rewrite template is the second CLI argument (or `rewrite=` in `.toml`) and uses the same hole syntax; holes are substituted with captured text.

Formatting is the interesting part, because Comby is unusually candid:

- Whitespace *matching* is deliberately loose (<https://comby.dev/docs/basic-usage>): "Whitespace in the template, like a single space, multiple contiguous spaces, or newlines are interpreted all the same: Comby will match the corresponding whitespace in the source code, but will not care about matching the exact number of spaces, or distinguish between spaces and newlines."
- Indentation is **not modelled at all** (<https://comby.dev/docs/faq>): "Comby does not currently consider whitespace indentation significant. We have plans to support it though!"
- Formatting fidelity on rewrite is **explicitly disclaimed** (FAQ): "Comby is not well-suited to stylistic changes and formatting like 'insert a line break after 80 characters.'" The FAQ's own recommended mitigation is to pipe the output through a language-specific formatter (e.g. `gofmt`).

Comment preservation is not addressed beyond the implicit guarantee that untouched spans are untouched.

### 1.6 Parser model

Comby is explicitly **not** built on the target language's real parser. From the FAQ (<https://comby.dev/docs/faq>), it "turns patterns into an executable routine (a language-aware parser) where the tree structure is implicit," and this trades away "the ability to recognize many predefined language-specific constructs." The README frames this as a *parser-lite* approach over **balanced delimiters** plus string/comment awareness: it knows where the matching `}` is without knowing what a `}` means.

The consequence is that **the fragment-parsing problem does not exist for Comby**. There is no expression/statement/type taxonomy to place a pattern into, so a bare `catch (…) { … }` is just text with balanced braces. The FAQ frames this as a feature — Comby is "more robust to matching patterns in the presence of unrecognized constructs" than a strict language parser (it names Coccinelle as the comparison).

The corresponding cost is that it cannot tell a `for` loop from a method call whose name happens to be `for`-ish, cannot normalise `a+b` vs `a + b` at the tree level (it normalises whitespace instead), and cannot answer any question about what a name refers to.

### 1.7 Known complaints (from the docs' own caveats)

- No indentation awareness (FAQ) — Python works only for indentation-insensitive edits.
- Not suited to stylistic changes; needs an external formatter afterwards (FAQ).
- No nested rewrite statements (advanced-usage).
- "Custom syntax is only partially supported in rules" (advanced-usage).
- The `match`-rule feature is "in active development and may change" (advanced-usage).
- Cannot customise the `_` wildcard's matching semantics (advanced-usage).

### 1.8 Semantics

None by default. A separate `comby-semantic.opam` package exists in the repo and there is a Comby blog post titled "Find and replace with type information" (<https://comby.dev/blog/2022/08/31/comby-with-types>) — **lead only, mechanics not verified here**.

---

## 2. Semgrep

*Sources: <https://docs.semgrep.dev/writing-rules/pattern-syntax>, <https://docs.semgrep.dev/writing-rules/rule-syntax>, <https://docs.semgrep.dev/writing-rules/autofix>, <https://docs.semgrep.dev/writing-rules/experiments/metavariable-type>, <https://docs.semgrep.dev/writing-rules/data-flow/taint-mode>, <https://docs.semgrep.dev/writing-rules/data-flow/constant-propagation>, <https://docs.semgrep.dev/kb/rules/pattern-parse-error>. Note `semgrep.dev/docs/*` 301-redirects to `docs.semgrep.dev/*`.*

### 2.1 Example patterns (verbatim)

```
insecure_function(...)
```
"finds calls regardless of its arguments."

```
func(1, ...)
```
matches both `func(1)` and `func(1, "extra", False)`.

```
$O.foo(). ... .bar()
```
— ellipsis spanning an arbitrary method-chain segment.

```
crypto.set_secret_key("...")
```
— ellipsis *inside a string literal*, matching any string content.

```
user_list = [..., 10]
```

```
foo($...ARGS, 3, $...ARGS)
```
matches `foo(1,2,3,1,2)` — a named, reused, variable-length argument slice.

```
def $FUNC(..., $ARG={}, ...):
```

```
if <... $USER.is_admin() ...>:
```

```
$X == (char *$Y)
```

All of the above from <https://docs.semgrep.dev/writing-rules/pattern-syntax>.

Full rules (<https://docs.semgrep.dev/writing-rules/rule-syntax>):

```yaml
rules:
  - id: unverified-db-query
    patterns:
      - pattern: db_query(...)
      - pattern-not: db_query(..., verify=True, ...)
    message: Found unverified db query
    severity: HIGH
    languages: [python]
```

```yaml
rules:
  - id: return-in-init
    patterns:
      - pattern: return ...
      - pattern-inside: |
          class $CLASS:
            ...
      - pattern-inside: |
          def __init__(...):
              ...
    message: return should never appear inside __init__
    languages: [python]
    severity: HIGH
```

```yaml
rules:
  - id: insecure-methods
    patterns:
      - pattern: module.$METHOD(...)
      - metavariable-regex:
          metavariable: $METHOD
          regex: (insecure)
    message: module using insecure method call
    languages: [python]
    severity: HIGH
```

```yaml
rules:
  - id: superuser-port
    languages: [python]
    message: module setting superuser port
    patterns:
      - pattern: set_port($ARG)
      - metavariable-comparison:
          comparison: $ARG < 1024 and $ARG % 2 == 0
          metavariable: $ARG
    severity: HIGH
```

Type filter (<https://docs.semgrep.dev/writing-rules/experiments/metavariable-type>):

```yaml
rules:
  - id: no-string-eqeq
    languages: [java]
    patterns:
      - pattern-not: null == $Y
      - pattern: $X == $Y
      - metavariable-type:
          metavariable: $Y
          type: String
```

Autofix (<https://docs.semgrep.dev/writing-rules/autofix>):

```yaml
rules:
  - id: use-sys-exit
    languages: [python]
    pattern: exit($X)
    fix: sys.exit($X)
    severity: MEDIUM
```

```yaml
- id: python-typing
  pattern: from typing import $X
  fix: ""
  languages: [python]
  severity: ERROR
```

Taint mode skeleton (<https://docs.semgrep.dev/writing-rules/data-flow/taint-mode>):

```yaml
pattern-sources:
  - pattern: source(...)
pattern-sanitizers:
  - pattern: sanitize(...)
pattern-sinks:
  - pattern: sink(...)
```

### 2.2 How "match anything" is spelled

Semgrep is the tool that most sharply separates the four granularities:

| Need | Spelling | Doc wording |
|---|---|---|
| one node, unnamed | `$_` | "An anonymous metavariable always takes the form `$_`." |
| one node, captured | `$X` | "Metavariables look like `$X`, `$WIDGET`, or `$USERS_2`." |
| a sequence | `...` | "The `...` ellipsis operator abstracts away a sequence of zero or more items such as arguments, statements, parameters, fields, characters." |
| a captured sequence | `$...ARGS` | e.g. `foo($...ARGS, 3, $...ARGS)` |
| arbitrary nesting depth | `<... p ...>` | "Use the deep expression operator `<... [your_pattern] ...>` to match an expression that could be deeply nested within another expression." |

The single most important caveat, and it is in the docs, not just in issue trackers:

> "The [ellipsis operator] does *not* jump from inner to outer statement blocks."

with a worked counter-example: given

```
if cond:
    foo()
baz()
bar()
```

a pattern expecting `...` to span from `foo()` inside the `if` out to `bar()` at the outer level does **not** match. `...` is scoped to one block, not to the control-flow graph. (This is the most-cited Semgrep footgun, and it is exactly the place where Coccinelle's CFG-based `...` behaves differently — see §5.3.)

### 2.3 Captures and back-reference

Naming: "They begin with a `$` and can only contain uppercase characters, `_`, or digits."

Back-reference, verbatim (<https://docs.semgrep.dev/writing-rules/pattern-syntax>):

> "For search mode rules, metavariables with the same name are treated as the same metavariable within the `patterns` operator."

So reuse *is* unification, and it is scoped to the `patterns` operator — i.e. across the sibling clauses of one rule, not merely within one pattern string. Taint mode extends this: "A metavariable defined in `pattern-sinks` and `pattern-sources` with the same name is treated as the same metavariable."

### 2.4 Constraints beyond shape

Semgrep's constraint vocabulary is the richest of the pattern-literal tools, and it is expressed entirely in the surrounding YAML rather than in the pattern:

- `metavariable-regex` — PCRE2 on the captured text.
- `metavariable-comparison` — a Python-like boolean expression over the value, with a `base:` field for octal/hex literals (`base: 8`).
- `metavariable-type` — filters on the language's **inferred type**, not on an annotation's text.
- `metavariable-pattern` — match a sub-pattern within what a metavariable captured.
- `pattern-inside` / `pattern-not-inside` — containment, positive and negative.
- `pattern-not` — negation at the same level.
- `patterns` (AND) / `pattern-either` (OR).
- `options:` — per-rule feature switches, e.g. `constant_propagation: false`.

There is **no count/repetition quantifier**. Repetition is expressed structurally (`func(1, ...)`), never numerically. This is a notable gap relative to JetBrains SSR, which has exactly the opposite emphasis.

### 2.5 Rewrite, and formatting

`fix:` is a top-level rule field using the same `$METAVAR` substitution; `fix: ""` deletes the match. Applied with `--autofix`, testable with `--autofix --dryrun`.

**The autofix documentation page makes no claim at all about whitespace, indentation, or comment preservation.** That silence is itself a finding — contrast Comby's FAQ, which disclaims formatting fidelity explicitly, and ast-grep, which documents an indentation rule explicitly.

Real-world formatting failure modes are tracked as issues, not documented as caveats *(secondary — GitHub issue titles, not doc text)*:

- `returntocorp/semgrep#3070` "Multiline auto-fix indentation is wrong" — "If a user inputs an autofix that is multiple lines, all lines after the first are indented on an absolute basis rather than relative to the first line."
- `returntocorp/semgrep#3577` — interspersed autofix of multiple expressions on the same line can garble output.
- `returntocorp/semgrep#2294` — the "Autofix mega issue."

### 2.6 Parser model and fragments

Semgrep **does** use the target language's real grammar, which is precisely why fragments are a documented source of friction. Two doc statements matter:

> "If your search pattern is a statement, Semgrep will automatically try to search for it as *both* an expression and a statement."

> "Partial statements are partially supported. For example, you can just match the header of a conditional with `if ($E)`, or just the try part of an exception statement with `try { ... }`."

That second sentence is the direct answer to the "bare `catch` block" question: Semgrep's answer is **not** a general fragment mechanism. It is a hand-carved list of supported partial statements (an `if` header, a `try` head). Everything else must be a complete expression or statement, and a "Pattern parse error" is a named, expected failure mode. The canonical example is that `if $X < 5` is invalid and must be written `if $X < 5: ...`.

A related documented parse gotcha (<https://docs.semgrep.dev/kb/rules/pattern-parse-error>), verbatim:

> "metavariable-pattern tries to match the pattern within the captured metavariable, which is going to be affected by how reserved keywords are parsed, while metavariable-regex runs a regex on the text range associated with the metavariable, ignoring how its content would be parsed and bypassing the issue."

There is also a `pattern-regex` / "generic pattern matching" escape hatch for languages with no grammar — a deliberate fall-back to Comby-ish text matching within the same tool. *(Page title fetched; body not read.)*

### 2.7 Semantics

This is where Semgrep separates from ast-grep/GritQL/Comby.

- **Constant propagation** (<https://docs.semgrep.dev/writing-rules/data-flow/constant-propagation>): "tracks whether a variable *must* carry a constant value at a given point in the program." Intrafile in Semgrep CE; interprocedural/interfile is a paid feature. It assumes called functions do not mutate a "constant" object, a documented false-positive source in languages with mutable strings.
- **Taint mode** (<https://docs.semgrep.dev/writing-rules/data-flow/taint-mode>): "Taint analysis is a dataflow analysis that tracks the flow of untrusted, or **tainted**, data throughout the body of a function or method," flowing "from sources to sinks through **propagators**, such as assignments and function calls."
- **Typed metavariables**, two spellings: inline in the pattern — Java `(java.util.logging.Logger $LOGGER).log(...)`, Go `($READER : *zip.Reader).Open($INPUT)`, C `$X == (char *$Y)` — and the standalone `metavariable-type` filter, which the docs present as the cleaner replacement.

Note the syntactic shape of the inline typed metavariable: it looks like a **cast**. That is Semgrep's answer to "how do you put a type constraint in a pattern literal without inventing new syntax" — borrow a construct the language already has.

### 2.8 Known complaints

- Ellipsis is block-scoped, not CFG-scoped (documented, §2.2).
- `metavariable-pattern` + reserved keywords → confusing parse failures; documented workaround is dropping to `metavariable-regex`.
- Autofix indentation and same-line-multiple-fix bugs (issue tracker).
- Pattern-parse-error is a recurring rough edge for anyone arriving with a Comby-style "any substring" mental model.

---

## 3. ast-grep

*Sources: <https://ast-grep.github.io/guide/pattern-syntax.html>, <https://ast-grep.github.io/guide/rule-config.html>, <https://ast-grep.github.io/guide/rule-config/atomic-rule.html>, <https://ast-grep.github.io/guide/rule-config/relational-rule.html>, <https://ast-grep.github.io/guide/rule-config/composite-rule.html>, <https://ast-grep.github.io/guide/rewrite-code.html>, <https://ast-grep.github.io/guide/rewrite/rewriter.html>, <https://ast-grep.github.io/guide/rewrite/transform.html>, <https://ast-grep.github.io/advanced/pattern-parse.html>, <https://ast-grep.github.io/advanced/core-concepts.html>, <https://ast-grep.github.io/advanced/faq.html>*

ast-grep is the closest existing thing to what `apexls query` would be: a Rust tool, tree-sitter based, pattern-literal front end, YAML rule back end. Its documentation is also the most honest about where pattern literals break down, which makes it the most useful section here.

### 3.1 Example patterns (verbatim)

Metavariable spellings:

```
$META
$META_VAR
$META_VAR1
$_
$_123
```

The full rule vocabulary in one block (<https://ast-grep.github.io/guide/rule-config.html>):

```yaml
rule:
  # atomic rule
  pattern: 'search.pattern'
  kind: 'tree_sitter_node_kind'
  regex: 'rust|regex'
  # relational rule
  inside: { pattern: 'sub.rule' }
  has: { kind: 'sub_rule' }
  follows: { regex: 'can|use|any' }
  precedes: { kind: 'multi_keys', pattern: 'in.sub' }
  # composite rule
  all: [ {pattern: 'match.all'}, {kind: 'match_all'} ]
  any: [ {pattern: 'match.any'}, {kind: 'match_any'} ]
  not: { pattern: 'not.this' }
  matches: 'utility-rule'
```

The closest published analogue to our motivating `for (...) { ... System.debug(...); ... }` (<https://ast-grep.github.io/guide/rule-config/relational-rule.html>):

```yaml
rule:
  pattern: await $PROMISE
  inside:
    any:
      - kind: for_in_statement
      - kind: for_statement
      - kind: while_statement
      - kind: do_statement
    stopBy: end
```

Note what happened: "a call inside a loop" is **not** expressible as one pattern literal. It is a pattern plus a relational rule plus an explicit `stopBy: end`. That is the central ergonomic fact about ast-grep's design.

```yaml
id: no-await-in-promise-all
language: TypeScript
rule:
  pattern: Promise.all($A)
  has:
    pattern: await $_
    stopBy: end
```

```yaml
kind: pair
has:
  field: key
  regex: 'prototype'
```

```yaml
inside:
  stopBy:
    kind: function
  pattern: function test($$$) { $$$ }
```

Composite:

```yaml
rule:
  any:
    - pattern: var a = $A
    - pattern: const a = $A
    - pattern: let a = $A
```

```yaml
rule:
  pattern: console.log($GREETING)
  not:
    pattern: console.log('Hello World')
```

Rewrite (<https://ast-grep.github.io/guide/rewrite-code.html>):

```yaml
id: change_def
language: Python
rule:
  pattern: |
    def foo($X):
      $$$S
fix: |-
  def baz($X):
    $$$S
```

```yaml
language: javascript
rule:
  kind: pair
  has:
    field: key
    regex: Remove
fix:
  template: ''
  expandEnd: { regex: ',' }
```

Rewriters — sub-fixes composed into an outer fix (<https://ast-grep.github.io/guide/rewrite/rewriter.html>):

```yaml
rewriters:
- id: dict-rewrite
  rule:
    kind: keyword_argument
    all:
    - has:
        field: name
        pattern: $KEY
    - has:
        field: value
        pattern: $VAL
  fix: "'$KEY': $VAL"
```

```yaml
rule:
  pattern: dict($$$ARGS)
transform:
  LITERAL:
    rewrite:
      rewriters: [dict-rewrite]
      source: $$$ARGS
fix: '{ $LITERAL }'
```

String transforms (<https://ast-grep.github.io/guide/rewrite/transform.html>), object and function forms:

```yaml
transform:
  NEW_VAR:
    replace:
      source: $VAR_NAME
      replace: regex
      by: replacement
  LIST:
    substring:
      source: $GEN
      startChar: 1
      endChar: -1
  KEBABED:
    convert:
      source: $OLD_FN
      toCase: kebabCase
```

```yaml
transform:
  NEW_VAR: replace($VAR, replace=regex, by=replacement)
  LIST: substring($GEN, startChar=1, endChar=-1)
  KEBABED: convert($OLD_FN, toCase=kebabCase)
```

### 3.2 How "match anything" is spelled

- **One node**: `$META` — "is a wildcard expression that can match any **single** AST node." Non-capturing `$_` exists to "micro-optimize pattern matching speed, since we don't need to create a HashMap for bookkeeping."
- **A sequence**: `$$$` — "to match zero or more AST nodes, including function arguments, parameters or statements." Named: `$$$ARGS`.
- **An unnamed node** (punctuation, operators): `$$VAR` — double dollar. Note the collision risk in the design: `$`, `$$`, `$$$` mean three different things.
- **Arbitrary nesting depth**: **no pattern-string operator exists.** Depth is a property of the relational rule: by default "relational rule will only match nodes one level further" (`stopBy: neighbor`); `stopBy: end` makes ast-grep "search surrounding nodes until it reaches the end"; `stopBy: { kind: function }` gives a custom stop condition.

`$$$` is **lazy**, which the FAQ flags as surprising: "`$$$MULTI` are lazy, stopping at the first matching node rather than matching all nodes."

### 3.3 Captures and back-reference

Naming: "Meta variables start with the `$` sign, followed by a name composed of upper case letters `A-Z`, underscore `_` or digits `1-9`." Valid: `$META`, `$META_VAR`, `$META_VAR1`, `$_`, `$_123`. Invalid: `$invalid`, `$Svalue`, `$123`, `$KEBAB-CASE`, `$`.

Back-reference, verbatim: "You can reuse same name meta variables to find previously occurred AST nodes." The worked example: `$A == $A` matches `a == a` and `1 + 1 == 1 + 1`, but not `a == b`.

There is a subtlety worth stealing or avoiding. From the FAQ: "Rule matching is ordered because previous rules' matched meta-variables can affect later rules. Only the first rule can specify what a `$META_VAR` matches." So in a rule object, *which clause binds and which clause constrains* depends on evaluation order, and the docs recommend wrapping in `all:` to pin that order. This is a real ergonomic tax created by having binding and constraining share one syntax.

### 3.4 Constraints beyond shape

- `kind:` — raw tree-sitter node kind, "used when it is hard to construct the valid syntax."
- `regex:` — "searches the node's full text, including its children, using a Rust regular expression" (so no lookahead/lookbehind).
- `field:` inside `has`/`inside` — targets a named grammar field.
- Relational: `inside`, `has`, `follows`, `precedes`, each with `stopBy: neighbor | end | <subrule>`.
- Composite: `all`, `any`, `not`, and `matches` — "a special composite rule that takes a rule-id string… The rule will match the same nodes that the utility rule matches," which is how named/reusable (and recursive) rules work.
- Combination semantics: "A node will match a rule if and only if it satisfies all fields in the rule object."
- **No type constraints, no counts.**

### 3.5 Rewrite, and formatting

The governing constraint, verbatim: **"ast-grep rule can only fix one target node at one time by replacing the target node text with a new string."** `fix` is a *string template*, not a tree transform.

But indentation is handled, and this is the one explicit statement any of these tools makes about it:

> "ast-grep's rewrite is indentation sensitive. That is, the indentation level of a meta-variable in the fix string is preserved in the rewritten code."

That is: when a `$$$`-captured multi-line block is spliced into a fix template, its lines are **re-indented relative to the metavariable's column in the template**. Not pasted verbatim, and not run through a formatter. This is the cheapest correct answer to the multi-line-rewrite problem and is worth noting as a candidate design (it is exactly the bug Semgrep has open — §2.5).

Two extension mechanisms:

- `fix` object form with `expandStart` / `expandEnd` regexes, to grow the replaced span past the matched node — e.g. swallowing a trailing comma: `fix: { template: '', expandEnd: { regex: ',' } }`. This is a frank admission that node-boundary replacement is not always the right span.
- `rewriters:` + `transform: { X: { rewrite: { rewriters: [...], source: $$$ARGS, joinBy: ' + ' } } }` — apply sub-fixes to each element of a captured sequence and splice the joined result into the outer template. "Only the matching rewriter that appears first in the `rewriters` list will be applied."

Comments: nothing documented beyond the implicit guarantee that unmatched spans are untouched.

### 3.6 Pattern parsing — the four-step algorithm

This is the most directly useful page in the whole survey (<https://ast-grep.github.io/advanced/pattern-parse.html>). The algorithm:

1. **Preprocess** — replace `$` with an "expando char" that the target grammar will happily lex as an identifier character.
2. **Parse** the preprocessed text with the target language's tree-sitter grammar.
3. **Extract the effective node** — by heuristic, or by the user's explicit `selector`.
4. **Detect wildcards** — convert the placeholder identifiers back into metavariables.

Step 1 is the trick worth copying: you do not extend the grammar to know about `$`. You make `$X` lex as an ordinary identifier, parse normally, then reinterpret. (Semgrep does the analogous thing — `$X` is already a valid identifier in most languages, which is why the metavariable sigil is `$` and not `?` or `%`.)

The governing principle, verbatim: **"First and foremost, pattern is AST based."** A pattern must be parseable code. Three documented failure classes follow:

- **Invalid** — metavariables in syntactically impossible positions. `$LEFT $OP $RIGHT` fails: the parser sees three consecutive identifiers, not an operand-operator-operand. *(This is a hard ceiling of the expando-char trick: you can only put a hole where an identifier can go.)*
- **Incomplete** — `"a": 123` in JSON does not parse without surrounding `{}`.
- **Ambiguous** — `a: 123` in JavaScript parses as either an object `pair` or a labeled statement. The default heuristic picks one (labeled statement), which may not be what you meant.

**Effective-node extraction**, verbatim heuristic: "extract the leaf node or the innermost node with more than one child." It walks down single-child chains — which carry no structural information, just wrapping — and stops at the first node with more than one child, or a leaf. So `foo(bar)` resolves to the `call_expression`, not the enclosing `expression_statement` or `program`.

**The escape hatch** — `context` + `selector`:

```yaml
pattern:
  context: '{ "a": 123 }'
  selector: pair
```

Verbatim explanation: "ast-grep works like this: First, the code in `context`, `class A { $FIELD = $INIT }`, is parsed as a class declaration. Then, it looks for the `field_definition` node, specified by `selector`, in the parsed tree."

```yaml
pattern:
  selector: field_definition
  context: class A { $FIELD = $INIT }
```

This is the general answer to fragments, and it is a *better* answer than Semgrep's hand-carved partial-statement list, because it is open-ended: any fragment can be expressed as (valid surrounding code, node kind to pull out). The cost is that the user must know the grammar's node-kind names — the exact knowledge pattern literals exist to avoid.

The FAQ makes the fragment case explicit as the top answer to "My pattern does not work": *"The most common scenario is that you only want to match a sub-expression or one specific AST node in a whole syntax tree. However, the code fragment corresponding to the sub-expression may not be valid code."*

### 3.7 Known complaints (from the FAQ itself)

- *"Pattern cannot match my use case, how?"* → **"Patterns are a quick and easy way to match code in ast-grep, but they might not handle complex code. YAML rules are much more expressive."** The tool's own docs concede the pattern literal is the shallow end.
- *"MetaVariable does not work"* → a metavariable must be a whole AST node. `use$HOOK` does not work — you cannot glue a hole to surrounding text. (Comby can; this is a direct trade of AST-correctness against convenience.)
- *"Multiple MetaVariable does not work"* → `$$$` is lazy.
- *"Why is rule matching order sensitive?"* → §3.3.
- **"ast-grep does not support multiple languages in one rule."**
- *(Secondary, GitHub Discussions)* — a `now()` pattern also matches `pendulum.now()`, because a bare call pattern is treated as "at least as general." Counter-intuitive to anyone arriving from grep.

### 3.8 Semantics

Verbatim answer to "Does ast-grep support advanced static analysis?": **"Short answer: NO."** No scope analysis, no type information, no control-flow analysis, no data-flow/taint, no constant propagation. It cannot find undefined variables, resolve types, detect unreachable code, or trace user input.

*This is the most important single data point for `apexls query`: ast-grep is the closest architectural sibling, and the capability it lacks — a resolved type and symbol layer — is exactly the thing apexls already has.*

---

## 4. GritQL

*Sources: <https://docs.grit.io/language/patterns>, <https://docs.grit.io/language/modifiers>, <https://docs.grit.io/language/conditions>, <https://docs.grit.io/language/bubble>, <https://docs.grit.io/tutorials/gritql>, <https://github.com/getgrit/gritql>. Note: the repo is now maintained under `github.com/biomejs/gritql`.*

GritQL's distinguishing move: the code pattern is a **backticked literal embedded in a real query language**, so pattern and constraint share one grammar rather than being pattern-string + surrounding YAML.

### 4.1 Example patterns (verbatim)

```grit
`console.log('Hello, world!')`
```

```grit
`console.log($message)`
```

```grit
`console.log($message)` => `// Removed console.log: $message`
```

```grit
`println($message)` => `console.log($message)`
```

```grit
`console.log($message, $...)`
```

```grit
range(start_line=1, end_line=3) => .
```

Node-constructor form — fields by name, no source-shaped literal at all:

```grit
augmented_assignment_expression(operator = $op, left = $x, right = $v)
```

Constraints in `where`:

```grit
`console.log($my_message)` => `alert($my_message)` where {
  $my_message <: `"This is a user-facing message"`
}
```

```grit
`console.log($my_message)` => `winston.info($my_message)` where {
  !$my_message <: r".+user-facing.+"
}
```

```grit
`console.log($my_message)` => `winston.info($my_message)` where {
  $my_message <: string()
}
```

```grit
or { `console.log($my_message)`, `console.error($my_message)` } => `winston.info($my_message)`
```

Nested rewrite inside a condition — note the `=>` appearing *inside* a `where`:

```grit
`console.$method($my_message)` => `winston.$method($my_message)` where {
  $method <: or { `log` => `debug`, `error` => `warn` }
}
```

The direct analogue of our motivating query — "a call inside an enclosing construct":

```grit
`console.log($my_message)` => `winston.error($my_message)` where {
  $my_message <: within `try { $_ } catch($_) { $_ }`
}
```

Interpolating a constructed identifier into a rewrite:

```grit
`const $logger = logger.$action($message)` where {
  $special_logger = js"$[action]Logger",
  $logger => $special_logger
}
```

### 4.2 How "match anything" is spelled

- **One node**: `$name`, or anonymous `$_` — matches a node "without binding to a value."
- **A sequence**: `$...` — "the spread metavariable … matches 0 or more nodes, and can be used anywhere a metavariable can be used."
- **Arbitrary depth, downwards**: the `contains` modifier — "used to modify a pattern to match any node that contains a specific pattern by **traversing downwards through the syntax tree**":

```grit
`function ($args) { $body }` where {
  $args <: contains `x`
}
```

bounded with `until` — "The `until` modifier is appended to `contains` pattern is used to stop traversal within a `contains` clause":

```grit
`console.$_($content)` where {
  $content <: contains `secret` until `sanitized($_)`
}
```

- **Arbitrary depth, upwards**: `within` — "restricts the pattern to only match if the target node appears within code matching another pattern":

```grit
`console.log($arg)` where {
  $arg <: within `if (DEBUG) { $_ }`
}
```

- **Sibling position**: `after` (and its counterpart), analogous to ast-grep's `follows`/`precedes`:

```grit
`console.warn($_)` as $warn where {
  $warn <: after `console.log($_)`
}
```

The `contains … until …` pairing is a nicer spelling of ast-grep's `stopBy` — same semantics, expressed as a natural-language-shaped clause rather than a YAML key with three possible value types.

### 4.3 Captures and back-reference

Naming: "must be alphanumeric" and "must conform to the regex `$[a-zA-Z_][a-zA-Z0-9_]*`" — lowercase-friendly, unlike Semgrep/ast-grep's uppercase-only convention.

No declaration needed: "Metavariables can be used without being declared, simply by replacing some of a code snippet with a metavariable meant to represent the substitution's part of the syntax tree."

Reuse back-references, and the docs make this explicit via the *failure* it causes — which is the most instructive thing in GritQL's docs.

### 4.4 `bubble` — the scoping problem nobody else names

From <https://docs.grit.io/language/bubble>: "The `bubble` clause introduces a new scope without the necessity of defining a separate pattern," and "variables inside the `bubble` clause are isolated from the surrounding code."

The problem it solves, in the docs' own words: *"Absent the `bubble`, only the first `console.log` call would be rewritten"* because *"When attempting to match the second `console.log`, `$message` would try to bind to `'How are you?'` and fail."*

```grit
pattern `function() { $body }` where {
  $body <: contains bubble `console.log($message)` => `console.warn($message)`
}
```

with explicit capture pass-through:

```grit
pattern `function $name() { $body }` where {
  $body <: contains bubble($name) `console.log($message)` => `console.warn($message, $name)`
}
```

**This is a general design hazard, not a GritQL quirk.** The moment a language has (a) metavariable unification and (b) a "find all X inside Y" operator, "does `$X` mean the same thing across separate inner matches?" must be answered. Semgrep answers it implicitly per-match; ast-grep answers it with rule ordering; GritQL makes it an explicit scoping construct. Any design for `apexls query` that has both features must answer it too.

### 4.5 Other constraints

- `where { … }` — "introduces one or more conditions that must be true for the pattern preceding it to execute."
- `<:` — "Grit's most common condition," the match/bind operator.
- `!` — "The `!` operator is used to negate a condition."
- `and` / `or { … }` — "The `or` operator is true if any of the conditions are true."
- `if` / `else` for conditional rewrites.
- `=` assignment inside `where`, to derive new bindings.
- Node-kind constructors — `string()`, `literal(value="42")`, `augmented_assignment_expression(operator=$op, …)` — the ast-grep `kind:` equivalent, but with named field access built in.
- Regex literals `r"…"`.
- List quantifiers:

```grit
`var $x = [$names]` => `var coolPeople = [$names]` where {
  $names <: some { `"andrew"` }
}
```

```grit
`var $x = [$names]` => `var coolPeople = [$names]` where {
  $names <: every or {`"andrew"`, `"alex"`}
}
```

- `maybe` — an optional match that still succeeds when the inner pattern does not match:

```grit
`throw new Error($err)` as $thrown => `throw new CustomError($err);` where {
  $err <: maybe string(fragment=$fun) => `{ message: $err }`
}
```

### 4.6 Rewrite

`=>` is the rewrite operator; the right side is a backticked template with metavariable substitution, or `.` to delete. Because `=>` is an ordinary operator in the query language, it composes: it appears nested inside `or { … }`, inside `where`, inside `maybe`, and inside `contains bubble`. This is the most composable rewrite design of the pattern-literal tools — much closer to Coccinelle's "the patch *is* the program" feel than to Semgrep's single top-level `fix:` field.

**Formatting/indentation/comments: not documented** on any of the pages fetched. The docs assert structural matching — "GritQL is designed to do _structural_ matching, not just string matching. Every code snippet is automatically converted into a syntax tree before it is matched against the codebase" — but say nothing about how the rewrite is re-emitted.

### 4.7 Parsing and semantics

The README's pitch is the opposite of ast-grep's: **"Start simply without learning AST details: any code snippet is a valid GritQL query."** No `pattern-parse` deep-dive page exists and no `context`/`selector` analogue was found — GritQL appears to lean on the node-constructor syntax (`augmented_assignment_expression(…)`) as the escape hatch for anything a source-shaped literal cannot express, rather than on a fragment-wrapping mechanism. *(Absence of a documented mechanism, not a confirmed absence of one.)*

No type or scope resolution is claimed anywhere in the pages fetched. GritQL sits on tree-sitter, like ast-grep.

### 4.8 Known complaints

Thin documentation of failure modes — `bubble`'s scope gotcha is the only one given its own page. No FAQ equivalent to ast-grep's exists. *(There is a `language/idioms` page not read in this pass.)*

---

## 5. Coccinelle / SmPL

*Sources: <https://coccinelle.gitlabpages.inria.fr/website/docs/> (the SmPL grammar manual, `main_grammar.pdf`, read directly), <https://coccinelle.gitlabpages.inria.fr/website/standard.iso.html>, <https://docs.kernel.org/dev-tools/coccinelle.html>, and real semantic patches from `torvalds/linux` `scripts/coccinelle/`.*

The oldest of these (2006-), the most powerful rewrite design, and the only one where the pattern language and the *patch* language are the same thing. It deserves the most space.

### 5.1 The shape: a patch is the program

The simplest possible semantic patch — rename a function everywhere (grammar manual):

```
@@
@@
- foo()
+ bar()
```

Two `@@` lines: the first delimits the rule header (empty = unnamed, no dependencies), the second closes the metavariable declaration block (empty = no metavariables). Then the body is *a diff*.

From the grammar manual, §5.1, verbatim:

> "The transformation specification essentially has the form of C code, except that lines to remove are annotated with - in the first column, and lines to add are annotated with +."

That single sentence is the whole design. Everyone else invented a separate rewrite side (`fix:`, `-rewrite`, `=>`, a Replace dialog). Coccinelle observed that programmers already have a notation for "this code becomes that code" — it is the unified diff — and made the pattern language *be* that notation. The before-pattern and the after-pattern are the same text, read twice with different line filters.

### 5.2 Metavariable declarations

The general shape is `@@ expression E; identifier f; type T; statement S; @@`. Kinds documented in the grammar manual (`main_grammar002.html` and the manual's §2):

| Kind | Matches |
|---|---|
| `expression` | anything conforming to the C99 expression grammar; constrainable by type (`struct foo *E`) or pointer level |
| `identifier` | a name — struct field, macro, function, variable (the token, not the value) |
| `type` | a type as it appears in a signature, declaration, cast, or `typedef` |
| `statement` | anything conforming to the C99 statement grammar |
| `declaration` | a variable declaration, including groups sharing one type spec (`int a,b,c=3;`) |
| `idexpression` / `local idexpression` | a variable used as an expression, optionally restricted to block scope or global scope |
| `constant` | numeric or all-uppercase-identifier constants ("names given to macros in Linux usually have this form") |
| `position` | attaches to a token via `@p`; captures file/line, correlates matches across rules |
| `parameter` | a function *parameter* declaration (distinct from a call-site argument) |
| `field` | struct field identifiers only |
| `iterator` | a macro used in place of a loop header, e.g. `list_for_each` |
| `declarer` | the declaration-side analogue of `iterator` |

Modifiers on a declaration: `!=` comparison constraints, regexp constraints, `script:` constraints (an arbitrary OCaml/Python boolean), and a `list` modifier to match a run of consecutive elements as one metavariable.

**This is the richest metavariable-kind vocabulary of any tool surveyed**, and it is the direct answer to the "is this hole an expression, a statement, or a type?" ambiguity — Coccinelle makes you *declare* it. Nobody else does. Semgrep and ast-grep infer it from position and then fail confusingly when the inference is wrong (§2.6, §3.6).

Note `iterator` and `declarer` especially: they exist because C has macros that *look like* calls but *behave like* loop headers. That is precisely the "a domain construct the grammar does not model" problem, solved by adding a metavariable kind rather than by special-casing names.

### 5.3 `...` and its `when` constraints

This is where Coccinelle's `...` differs fundamentally from Semgrep's. Verbatim from the grammar manual (§4):

> "Ellipses ('...') can be used to indicate to Coccinelle that anything can be present in a control-flow graph path between matches of two statements."

**A control-flow graph path** — not a block, not a token range. From §5.1:

> "A transformation specification can also use dots, '...', describing an arbitrary sequence of function arguments or instructions within a control-flow path. Implicitly, '...' matches the shortest path between something that matches the pattern before the dots (or the beginning of the function, if there is nothing before the dots) and something that matches the pattern after the dots (or the end of the function, if there is nothing after the dots)."

Three contexts, three meanings:

- **In a statement sequence** — any sequence of statements along a CFG path, shortest-path by default.
- **In an argument list** — `f(...)` is "any arguments, zero or more," purely syntactic.
- **In an expression** — any elided subexpression.

The shortest-path rule is implemented, verbatim: "by requiring that the pattern (if any) appearing immediately before the dots and the pattern (if any) appearing immediately after the dots are not matched by the code matched by the dots."

**`when` constraints:**

| Form | Meaning |
|---|---|
| `... when != X` | the elided region must not contain a match of `X` anywhere on the path |
| `... when any` | verbatim: "when any removes the aforementioned constraint that '...' matches the shortest path" |
| `... when exists` | this ellipsis succeeds if *any* CFG path satisfies the pattern |
| `... when forall` | *all* CFG paths must satisfy it |
| `... when strict` | (present in the grammar, `when COMMA_LIST(any_strict)`) |

Defaults, verbatim from the manual (§4.3):

> "By default when a semantic patch has '-' and '+', or when it has no annotations at all and only script code, ellipses ('...') use the forall semantics. And when the semantic patch uses the context annotation ('*'), the ellipses ('...') uses the exists semantics. Using the keyword 'forall' or 'exists' in the rule header affects all ellipses ('...') uses in the rule. You can also annotate each ellipses ('...') with 'when exists' or 'when forall' individually."

The default flip is deliberate and worth pausing on: **when you are transforming, `...` is universally quantified (safe — the rewrite must be correct on every path); when you are merely reporting, it is existentially quantified (useful — one bad path is a finding).** No other tool here varies its quantifier by whether you are searching or rewriting.

**The nest operators**, verbatim from §4.2:

> "There are two possible modifiers to the control flow for ellipses, one (`<... ...>`) indicates that matching the pattern in between the ellipses is to be matched 0 or more times, i.e., it is optional, and another (`<+... ...+>`) indicates that the pattern in between the ellipses must be matched at least once, on some control-flow path. In the latter, the `+` is intended to be reminiscent of the `+` used in regular expressions."

So `<... P ...>` = "P, zero or more times, anywhere within" and `<+... P ...+>` = "P at least once, anywhere within". These are the deep/descendant operators, and notice they come in a 0-or-more and a 1-or-more flavour — a distinction Semgrep's `<... ...>` and GritQL's `contains` do not make.

Used for arbitrarily-nested expression search:

```
 sizeof(<+...E@p...+>)
```

(E occurs at least once, however deeply nested, inside the `sizeof` argument), and:

```
if (<+... f(...) ...+>) { BUG(); }
```

(a call to `f` occurs somewhere in the condition, at any depth).

### 5.4 Disjunction and conjunction

From §5.1, verbatim:

> "a transformation can specify a disjunction of patterns, of the form `( pat1 | … | patn )` where each `(`, `|` or `)` is in column 0 or preceded by `\`. Similarly, a transformation can specify a conjunction of patterns, of the form `( pat1 & … & patn )`… All of the patterns must be matched at the same place in the control-flow graph."

Column-0 significance is a hack, but it buys something real: `|` inside a pattern can still mean C's bitwise-or, because the *alternation* `|` is distinguished by position, not by escaping. A worked example (grammar manual):

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

Note also `@ haskernel @` — a rule with an empty body that only checks a file `#include`s something, and `depends on haskernel` gating the second rule on it. Rules as predicates over files.

### 5.5 Isomorphisms — the one genuinely unique idea

*Source: <https://coccinelle.gitlabpages.inria.fr/website/standard.iso.html>*

`standard.iso` is a separate SmPL-syntax file, loaded automatically for every semantic patch, containing named equivalence rules written with `<=>` (bidirectional) and `=>` (unidirectional):

```
X == NULL <=> NULL == X <=> !X     (is_null)
X != NULL <=> NULL != X            (isnt_null1)
!X <=> X == NULL                   (not_ptr2)
!X <=> 0 == X  /  !X <=> X == 0    (not_int1 / not_int2)
```

Effect: a pattern written `x == NULL` transparently matches `NULL == x` and `!x` as well, without the author enumerating them.

This is the **only mechanism in the entire survey that normalises semantically-equivalent surface syntax at the pattern layer**, and it is *user-extensible* — the isomorphism file is data, not compiled-in behaviour. Individual patches can disable specific isomorphisms by name: `@ rulename disable is_null @`. A real example, disabling the `unlikely` isomorphism so that the `unlikely(E)` form can be matched separately:

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

The manual notes isomorphism expansion is **not** run to a fixed point: "As we don't do a fixpoint, changing the order may impact the result." There are also isomorphisms built into the implementation rather than living in the file (manual §1, around the `disable` keyword), e.g. the one letting a SmPL declarer with a semicolon match one without, and `optional_storage` (a pattern without `static` matches code with it), which can likewise be disabled.

For Apex, the analogous normalisations are obvious and numerous: `x == null` / `null == x`, `String.isBlank(s)` vs `s == null || s == ''`, `List<X> l = new List<X>()` vs `new List<X>{}`, case-insensitive identifiers. An isomorphism file is the design that lets those live as *data* rather than as parser special-cases.

### 5.6 `*` mode — search without rewrite

A leading `*` marks lines of interest for reporting rather than performing a rewrite. Full example from the manual:

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

One notation, three modes: `*` (report), `-`/`+` (rewrite), and no annotation (pure match, usually feeding a script rule). The pattern text is identical across all three.

### 5.7 Named rules, inheritance, positions, and scripting

A full production semantic patch from the kernel — `scripts/coccinelle/free/kfree.cocci`, a use-after-free detector (<https://raw.githubusercontent.com/torvalds/linux/master/scripts/coccinelle/free/kfree.cocci>), abridged to the structurally interesting rules:

```
@free@
expression E;
position p1;
@@

(
 kfree@p1(E)
|
 kfree_sensitive@p1(E)
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
 &subE
|
 BUG(...)
|
 E@p2 // bad use
)

@script:python depends on report@
p1 << free.p1;
p2 << r.p2;
@@

msg = "ERROR: reference preceded by free on line %s" % (p1[0].line)
coccilib.report.print_report(p2[0],msg)
```

Everything load-bearing is visible here:

- **Rule naming and metavariable inheritance** — `@free@` binds `E` and `p1`; later rules write `expression free.E;` and `position free.p1` to inherit those exact bindings. Cross-rule unification, explicitly namespaced.
- **Position variables** — `kfree@p1(E)` attaches position `p1` to the `kfree` token. Positions are first-class values.
- **Position *set arithmetic*** — `position free.p1!=loop.ok,p2!={print.p,sz.p};` means "p1 is a kfree position that is *not* the whitelisted in-loop one, and p2 is *not* any of the positions matched by the `print` or `sz` rules." Whitelisting by exclusion of previously-matched program points. No other tool in this survey has anything like this; in Semgrep the nearest equivalent is stacking `pattern-not` clauses, which cannot refer to another rule's matches.
- **`subE<=free.E`** — a sub-expression constraint: `subE` must be a sub-expression of `E`.
- **Rule modes** — `@loop exists@`, `@r exists@` switch those rules to existential path quantification.
- **Script rules** — `@script:python depends on report@`, inheriting `p1 << free.p1;`. A script rule runs once every non-defaulted inherited metavariable is bound. Defaults are available (`id << r.id = "default";`, or `= []` for lists). OCaml scripts can bind both the string and the parsed-AST form: `(id,id) << rulename.id;`. `initialize:`/`finalize:` blocks run once before/after all files.
- **Dependencies** — `depends on X` (X matched in the current metavariable environment), `depends on ever X` (X matched at all), `depends on never X`.

Another real patch worth noting for its shape, also from the kernel tree — the second-rule-inherits-first-rule idiom for "remove an argument, but only from *this* function":

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

Note the diff annotation applied *mid-expression*, to a single argument — `-` on the `,E3` line inside a call. The `-`/`+` notation is line-based but the patch is applied at AST granularity, which is why the un-annotated `fn(E1, E2` and `)` lines act as context.

### 5.8 Formatting, whitespace, and comments

Coccinelle's output is a diff over the original file: unmatched code is emitted unchanged, because the unparser re-emits the original token stream everywhere except where a `-`/`+` substitution occurred. Comments, blank lines, and indentation around a match survive verbatim. This is the primary operational reason the Linux kernel uses Coccinelle for tree-wide changes — the diffs are minimal and reviewable.

**Gap.** I could not retrieve primary-source documentation for `--smpl-spacing` specifically; the canonical CLI reference is <https://coccinelle.gitlabpages.inria.fr/website/docs/options.pdf>, which was not read in this pass. The commonly-understood behaviour (`--smpl-spacing` makes added code follow the spacing written in the semantic patch rather than being re-indented heuristically) is **unverified here**. If the exact re-indentation policy matters to the design, read that PDF or run `spatch --help`.

### 5.9 Known complaints

- **C only.** The grammar is built around C99 ("conforms to the C99 [expression/statement/type] grammar" throughout the manual). No C++-specific constructs, no other languages. The design generalises; the implementation does not.
- **Macros.** Coccinelle deliberately does not expand preprocessor directives — it matches the pre-preprocessed token stream — so an undeclared macro can desynchronise the parser. The fix is `--macro-file-builtins <header.h>` (the kernel ships `standard.h` for exactly this) or `--macro-file`. *(Mechanism confirmed; the specific "insufficient definition" phrasing is from search results, secondary.)*
- **Performance.** The kernel docs document `J=<n>` for parallelism and, since 1.0.2, dynamic load balancing via OCaml `parmap` with `--chunksize 1`; workflow guidance is to scope runs with `M=<dir>` or `COCCI=<file>` rather than running full-tree `coccicheck` routinely.
- From the kernel doc, verbatim: "As with any static code analyzer, Coccinelle produces false positives. Thus, reports must be carefully checked, and patches reviewed."

---

## 6. CodeQL

*Sources: <https://codeql.github.com/docs/codeql-overview/about-codeql/>, <https://codeql.github.com/docs/ql-language-reference/queries/>, <https://codeql.github.com/docs/ql-language-reference/types/>, <https://codeql.github.com/docs/codeql-language-guides/basic-query-for-cpp-code/>, <https://codeql.github.com/docs/codeql-language-guides/expressions-types-and-statements-in-cpp/>, <https://codeql.github.com/docs/writing-codeql-queries/about-data-flow-analysis/>, <https://codeql.github.com/docs/ql-language-reference/recursion/>*

### 6.1 Why there is no pattern literal

From "About CodeQL": CodeQL extracts "a single relational representation of each source file," storing "a full, hierarchical representation of the code, including a representation of the abstract syntax tree, the data flow graph, and the control flow graph." Each language has "its own unique database schema that defines the relations used to create a database," and "the CodeQL libraries define classes to provide a layer of abstraction over the database tables. This provides an object-oriented view of the data which makes it easier to write queries."

The design premise is that the artefact being queried is a **database**, not a token stream to be re-matched. Once that is true, a pattern literal has nothing to bind to — there is no "code shaped like this" operation, only joins over relations. You never write source-shaped syntax with holes in it, ever.

### 6.2 Examples (verbatim)

Query structure:

```
from int x, int y
where x = 3 and y in [0 .. 2]
select x, y, x * y as product, "product: " + product
```

A class — the object-oriented layer over the relational tables:

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

A real C++ query — redundant empty `if`, in the docs' own iterative-refinement style:

```
from IfStmt ifstmt, BlockStmt block
where ifstmt.getThen() = block and
  block.getNumStmt() = 0 and
  not ifstmt.hasElse()
select ifstmt, "This 'if' statement is redundant."
```

### 6.3 "A call inside a for loop" — the direct comparison

This is the closest published analogue of our motivating query (<https://codeql.github.com/docs/codeql-language-guides/expressions-types-and-statements-in-cpp/>):

```
import cpp

from AssignExpr e, ForStmt f
// the assignment is in the for loop body
where e.getEnclosingStmt().getParentStmt*() = f.getStmt()
  and e.getRValue().getValue().toInt() = 0
  and e.getLValue().getType().getUnderlyingType() instanceof IntegralType
select e, "Assigning the value 0 to an integer, inside a for loop body."
```

The `*` is transitive closure — from <https://codeql.github.com/docs/ql-language-reference/recursion/>, `+` is "the predicate applied one or more times" and `*` is "the predicate applied zero or more times." So `getParentStmt*()` is exactly "at any nesting depth," and `getAChild*()` is its downward counterpart.

Put beside Coccinelle's `for (...) { <+... f(...) ...+> }` or Semgrep's `for (...) { ... $F(...) ... }`, the trade is stark: **what a pattern literal gets for free from the shape of the text, CodeQL requires you to name explicitly** — the containment relation, the type test, the value test, each as a separate predicate call, each requiring you to know the library's class and predicate names.

### 6.4 Rewrites

None. The documented model is database creation → query execution → result interpretation. There is no transform primitive in the language at all. (GitHub's Copilot Autofix layers LLM-generated suggestions on top of CodeQL *alerts*; that is a separate mechanism, not part of QL, and its output "still requires review and testing.")

### 6.5 What the verbosity buys

From <https://codeql.github.com/docs/writing-codeql-queries/about-data-flow-analysis/>: data flow analysis computes "the possible values that a variable can hold at various points in a program, determining how those values propagate through the program and where they are used." Normal data flow analyses "the information flow in which data values are preserved at each step"; taint tracking extends this to cases where "data values are not necessarily preserved, but the potentially insecure object is still propagated." Analysis runs at two levels — local (single function, fast and precise) and global (cross-function, more comprehensive, more expensive).

That is the capability no syntactic tool can offer at any price: "does untrusted input reach this sink through any assignment chain" is not a question about shape.

### 6.6 Ergonomics

The docs do not editorialise about difficulty, and I found no primary-source admission of a learning curve. The examples make the trade self-evident: a 3-line Coccinelle hunk versus a query that declares two typed variables and spells out containment and type tests as named predicate calls. CodeQL buys semantic precision at the cost of requiring the class/predicate vocabulary up front — the same "you must know the node names" tax as tree-sitter queries, one abstraction layer higher.

---

## 7. tree-sitter query language

*Sources: <https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html>, `/2-operators.html`, `/3-predicates-and-directives.html`*

The relevant contrast: a query language that is *not* source-shaped, sitting on the same substrate (tree-sitter) that ast-grep and GritQL build their source-shaped languages on top of.

### 7.1 Examples (verbatim)

```
(assignment_expression
  left: (member_expression object: (call_expression))
)
```

```
(assignment_expression
  left: (identifier) @the-function-name
  right: (function))
```

Quantifiers:

```
(comment)+
(decorator)* @the-decorator
(string)? @the-string-arg
```

Grouped sibling sequence:

```
(
  (number)
  ("," (number))*
)
```

Alternation:

```
(call_expression
  function: [
    (identifier) @function
    (member_expression property: (property_identifier) @method)
  ])
```

Anchors:

```
(array . (identifier) @the-element)
(block (_) @last-expression .)
(identifier) @prev-id . (identifier) @next-id
```

### 7.2 The vocabulary

- **Field names** — `left:`, `object:` prefix a child pattern to require a specific grammar field.
- **Negated fields** — `!type_parameters` requires the node to *lack* that field.
- **Anonymous nodes** — literal tokens as quoted strings: `operator: "!="`.
- **Wildcards** — `(_)` matches any *named* node; bare `_` matches any node, named or anonymous. Note this is the inverse convention from the pattern-literal tools: here the parenthesised form is the *narrower* one.
- **ERROR / MISSING** — `(ERROR)`, `(MISSING identifier)` match parser error-recovery nodes. A capability none of the pattern-literal tools expose, and directly relevant to a language server that already has error-tolerant trees.
- **Supertypes** — grammar union types can be matched directly (`(expression)`), or narrowed as `expression/binary_expression`.
- **Anchors** — leading `.` = first named child; trailing `.` = last; between two siblings = immediately adjacent. Anchors compose with quantifiers, and a quantified node that matches zero times imposes no adjacency constraint.

### 7.3 Predicates, and who evaluates them

Verbatim, and this is the architecturally important sentence:

> "Predicates and directives are not handled directly by the Tree-sitter C library. They are just exposed in a structured form so that higher-level code can perform the filtering."

The core query engine *parses* `#eq?` / `#match?` / `#any-of?` but does not *evaluate* them. Every binding (Rust, WASM, editor integration) implements the filtering itself over raw match results. This is a deliberate minimalism: the engine does structural matching only, and every semantic predicate is the caller's problem — which is precisely the seam where a tool with a type checker would inject type predicates.

Predicate families:

- `#eq?` / `#not-eq?` / `#any-eq?` / `#any-not-eq?` — "The first argument to this predicate must be a capture, but the second can be either a capture" (compare two captures' text) "or a string" (compare against a literal).
- `#match?` — "The first argument must be a capture, and the second must be a string containing a regular expression."
- `#any-of?` — "will match if the capture's text is equal to any of the strings"; exact string matching, not regex.
- The `any-` prefix: "you can prefix either of these with `any-` to match if any of the nodes match the predicate. This is only useful when dealing with quantified captures" — by default a quantified capture requires *all* its nodes to satisfy the predicate.
- Directives (side-effecting, not filtering): `#set!`, `#select-adjacent!`, `#strip!`.

### 7.4 Back-reference

**Captures do not unify.** Two captures sharing a name simply both report. To require equal text you write it explicitly: `(#eq? @a @b)` — and, per §7.3, *the caller* evaluates that. This is the opposite convention from every pattern-literal tool here, where name reuse implies unification.

### 7.5 No rewrite side

Confirmed across all three query pages: no replacement or transform primitive exists. The output is a set of (pattern, captures) matches. `#set!` and `#strip!` only annotate or locally massage text a caller already extracted (syntax highlighting uses `#set!` to attach highlight-group names). Any tree-sitter-based codemod is entirely caller-built, splicing text by the returned byte ranges.

### 7.6 Contrast with pattern literals

- **Verbosity** — capturing a function name takes an S-expression with explicit field names, where Coccinelle takes `- foo()` / `+ bar()`.
- **Grammar knowledge is mandatory** — you must already know `assignment_expression`, `member_expression`, `property_identifier`. Pattern-literal tools exist precisely to avoid this; CodeQL hides it behind library class names.
- **No fragment problem** — because the query is a separate grammar matched against an existing tree, there is never a question of whether the pattern parses as valid standalone code. This is the direct pay-off for the verbosity: **the fragment-parsing problem and the must-know-node-names problem are the same trade, seen from two ends.**

---

## 8. JetBrains Structural Search and Replace

*Sources: <https://www.jetbrains.com/help/idea/structural-search-and-replace.html>, <https://www.jetbrains.com/help/idea/search-templates.html>, <https://www.jetbrains.com/help/idea/structural-search-and-replace-examples.html>, <https://www.jetbrains.com/help/idea/tutorial-work-with-structural-search-and-replace.html>, <https://plugins.jetbrains.com/docs/intellij/psi.html>. Note: `structural-search-filters.html` and `creating-editing-search-templates.html` 404 on the current site — that content has been folded into the two pages above.*

### 8.1 Examples (verbatim, from the examples page)

- `$Instance$.$MethodCall$($Parameter$)` — "matches method call expressions. If the number of occurrences is zero, it means that a method call can be omitted."
- `@Deprecated $Instance$.$MethodCall$($Parameter$)`
- `synchronized ($parameter$){ $statement$; }` — "search for all the synchronizable methods with an arbitrary number of parameters, but with only one line of code in the body"
- `$Statement$;` — "find sequences of statements that contain up to the specified number of elements"
- `if ($Expr$) { $ThenStatements$; } else { $ElseStatements$; }`
- `class $Clazz$ extends $AnotherClass$ {}`
- `class $a$ { public void $show$(); }` — "look for the different implementations of the same interface method"
- `class $Class$ { @Modifier("packageLocal") @Modifier("Instance" ) $ReturnType$ $MethodName$($ParameterType$ $Parameter$); } }`
- `new java.lang.RuntimeException($x$)` — the canonical Count-modifier example
- `LOG.debug($params$);`
- `<$tag$ $attribute$=$value$ />` — the same template language over XML/HTML

Replace pairs:

- Search `$Statements$;` → Replace `try { $Statements$; } catch(Exception ex) { }` — "replace a statement with a try/catch/finally construct"
- Search `<$tag$ $attribute$="$value$">` → Replace `$to_lower_case$`

### 8.2 The distinctive design: repetition is a constraint, not syntax

There is **no sequence operator at all**. No `...`, no `$$$`, no `$*_`. Instead, every variable carries a **Count modifier**:

> "The Count modifier specifies a number of occurrences." … "To set the unlimited maximum count, provide an empty value in the modifier field."

So `$Parameter$` with count `[1,1]` is exactly one argument; with count `[0,∞]` it is varargs; `$Statement$;` with an unbounded count is an arbitrary run of statements. The UI renders the range next to the variable as `[0,∞]`.

**One filter unifies "optional", "exactly N", "at most N", and "arbitrary sequence."** That is strictly more expressive than `...` — Semgrep and ast-grep have no way to say "between two and four arguments" at all — and it is the only tool here that can express a count bound.

The ergonomic cost is severe and is the tool's best-known complaint: you cannot tell from reading `$Instance$.$MethodCall$($Parameter$)` whether `$Parameter$` means one argument or any number. **The pattern text is not self-contained.** The semantics live in a dialog.

### 8.3 Filters

The current live docs enumerate exactly five:

1. **Count** — as above.
2. **Text** — "The Text modifier checks the variable against regular expressions or plain text." Includes matching by fully qualified name and a hierarchy option.
3. **Type** — "The Type modifier adds a type of the value or expression that is expected for the specified variable." (e.g. restricting `$expression$` to `int` to catch boxing.)
4. **Reference** — "The Reference modifier lets you reference some other search template in the variable" — i.e. a saved template as a sub-constraint. (Compare ast-grep's `matches:` utility rules.)
5. **Script** — "The Script modifier adds Groovy script constraints to the search template." Used for "constructors with the specified number of parameters," "members with the specified visibility modifiers." All template variables are exposed to the script as PSI nodes: "All variables used in a template can be accessed from script constraints… this variable is in fact a node in the PSI tree."

**Flagged as unconfirmed:** the filter names "contained in constructor," "read/write access," and "formal argument type" could **not** be confirmed on the current live docs. They may be older UI labels or script-level predicates. The examples page separately mentions a "Contained in Constraints" *field* (a scoping field restricting matches to within a containing element), which is related but distinct from the five modifiers. Verify in the IDE before citing.

### 8.4 Back-reference

No dedicated "same as" filter is enumerated. Reusing `$var$` at multiple template positions binds them together through PSI variable binding, and the **Script** modifier is the documented route to asserting cross-variable equality explicitly (a Groovy predicate over `.text`/`.name` of two variables). *This is a genuine "escape to a general-purpose language" answer, and it is what having a scripting hook buys you.*

### 8.5 Replace side and formatting

Verbatim options from the replace dialog docs:

> "Shorten fully-qualified names - replaces fully qualified class names with short names and imports."
> "Reformat - automatically formats the replaced code."
> "Use static import - uses static import in replacement when possible."

This is a third distinct formatting strategy, and the only one available to a tool with full semantic context: **not** "preserve original bytes" (Coccinelle), **not** "re-indent relative to the template" (ast-grep), but "hand the result to the IDE's own formatter and import manager." *"Shorten fully-qualified names"* is only possible because the tool can resolve names and edit the import list — a rewrite that is semantic, not textual.

Comment preservation is not documented on the pages fetched. Do not assert either way.

### 8.6 Parser

Yes, the real one. From the IntelliJ platform docs / `platform/structuralsearch` source: "The pattern in structural search is valid code that is parsed by the parser, producing the PSI tree. The tree is then matched on source code trees to find fragments that are suitable for the specified constraints." And the framing sentence from the SSR help page: "A conventional search process does not take into account the syntax and semantics of the source code."

Fragments are matched at whatever grammar entry point the template fragment parses as — no `context`/`selector` mechanism is documented, which suggests IntelliJ's parsers are simply more tolerant of fragments than a standalone grammar is (they are built for incremental editing of incomplete code, which is exactly the property a language-server parser has).

### 8.7 Known complaints

- **GUI only.** Everything runs through the Structural Search / Structural Replace dialogs. No documented CLI or batch invocation — so no CI use, no scripting, no diffable rule files.
- **Sharing is export/import** of a template config: "You can share a search template with your peers by exporting or importing it." Not a plain-text pattern you commit to a repo.
- **Not self-contained.** The `$x$` sigil is verbose, and the load-bearing semantics (counts, types) are invisible in the pattern text (§8.2).

---

## 9. gogrep

*Source: <https://raw.githubusercontent.com/mvdan/gogrep/master/README.md> (fetched in full). **Status: archived** — "Note that this project is no longer being developed." See <https://github.com/mvdan/gogrep/issues/64>. The actively-maintained fork is <https://github.com/quasilyte/gogrep>, used inside `go-critic`.*

Small, and worth reading precisely because the entire language fits in a README.

```
gogrep -x 'if $x != nil { return $x, $*_ }'
```

The command model — a pipeline expressed as repeated flags on one invocation:

```
A command is of the form "-A pattern", where -A is one of:

       -x  find all nodes matching a pattern
       -g  discard nodes not matching a pattern
       -v  discard nodes matching a pattern
       -a  filter nodes by certain attributes
       -s  substitute with a given syntax tree
       -w  write source back to disk or stdout
```

The fragment answer, stated as a list of accepted pattern kinds:

```
A pattern is a piece of Go code which may include wildcards. It can be:

       a statement (many if split by semicolons)
       an expression (many if split by commas)
       a type expression
       a top-level declaration (var, func, const)
       an entire file
```

Back-reference, stated outright:

```
Wildcards consist of $ and a name. All wildcards with the same name
within an expression must match the same node, excluding "_". Example:

       $x.$_ = $x // assignment of self to a field in self
```

Sequences, and the neat trick of using the same `*` for "optional":

```
If * is before the name, it will match any number of nodes. Example:

       fmt.Fprintf(os.Stdout, $*_) // all Fprintfs on stdout

* can also be used to match optional nodes, like:

	for $*_ { $*_ }    // will match all for loops
	if $*_; $b { $*_ } // will match all ifs with condition $b
```

`for $*_ { $*_ }` matching *all* for loops — including the three-clause, range, and bare forms — by putting a zero-or-more wildcard in the header slot, is a genuinely elegant piece of design worth noting.

Type constraints via a separate filter command:

```
       gogrep -x '$x + $y'                   // will match both numerical and string "+" operations
       gogrep -x '$x + $y' -a 'type(string)' // matches only string concatenations
```

Patterns are parsed with Go's own `go/parser`; the multi-entry-point trial is implied by the list of accepted pattern kinds above. *(The README states the kinds; it does not state the trial order. The exact order was not confirmed.)*

---

## 10. Refaster (Error Prone)

*Source: <https://errorprone.info/docs/refaster>*

The most radical answer to "what should a pattern look like": **it should be real, compilable code.**

```java
public class StringIsEmpty {
  @BeforeTemplate
  boolean equalsEmptyString(String string) {
    return string.equals("");
  }

  @BeforeTemplate
  boolean lengthEquals0(String string) {
    return string.length() == 0;
  }

  @AfterTemplate
  boolean optimizedMethod(String string) {
    return string.isEmpty();
  }
}
```

Doc description: "Refaster templates are any class with multiple methods with the same return type and list of arguments with the same name." One method is `@AfterTemplate`; every other is a `@BeforeTemplate`. Anything matching any before-template — however the expression is chained, e.g. `someChained().methodCall().returningAString().length() == 0` — is rewritten to the after form.

**There is no placeholder syntax at all.** The template method's **parameters are the metavariables**: `String string` means "any expression of type `String`." This collapses three problems into zero:

1. *Fragment parsing* — impossible by construction; the pattern is a class, javac parses it.
2. *Expression/statement/type ambiguity* — resolved by where the hole appears in real Java: a parameter is an expression hole, its declared type is the type constraint.
3. *Type constraints* — free. javac type-checks the template, so `String string` is a real type constraint enforced by the real type checker, not by a filter DSL. Generic parameters give generic-typed matching for free.

Multiple `@BeforeTemplate`s per class give the Coccinelle-isomorphism effect by enumeration: several syntactic variants collapse to one canonical after-form. Less general than an isomorphism file (no bidirectionality, no reuse across templates), but it needs no new mechanism.

`@Placeholder` is the escape hatch to statement/block-shaped holes: it marks an abstract method representing "some function in terms of the specified input." Constraint, quoted: "The code matched by the placeholder method **cannot** refer to variables in the `@BeforeTemplate` that are not explicitly passed in." Related: `@MayOptionallyUse` (the after-template may optionally use an argument) and `allowsIdentity = true` (permits identity/no-op placeholder matches). Some support exists for adjusting between block and expression lambdas.

**Limits.** The mechanism is fundamentally expression-level — "a Java method body is a pattern" — with `@Placeholder` as the extension toward blocks. And the whole approach only works for languages where (a) you have a compiler, and (b) a well-typed fragment is expressible as a method. There is no way to express "not inside a loop," no counts, no relational constraints. The price of "the pattern is real code" is that you can only say what the language itself can say.

---

## 11. PMD XPath rules

*Sources: <https://pmd.github.io/pmd/pmd_userdocs_extending_writing_xpath_rules.html>, and PMD's shipped Apex ruleset at <https://github.com/pmd/pmd/blob/main/pmd-apex/src/main/resources/category/apex/bestpractices.xml>*

Directly relevant: **PMD already ships an Apex AST and a query language over it.** Real, shipped Apex rules, verbatim:

```xml
<rule name="ApexUnitTestMethodShouldHaveIsTestAnnotation"
    since="6.13.0"
    language="apex"
    message="Apex test methods should have @isTest annotation."
    class="net.sourceforge.pmd.lang.rule.xpath.XPathRule"
    externalInfoUrl="${pmd.website.baseurl}/pmd_rules_apex_bestpractices.html#apexunittestmethodshouldhaveistestannotation">
    <description>
        Apex test methods should have `@isTest` annotation instead of the `testMethod` keyword,
        as `testMethod` is deprecated.
    </description>
    <priority>3</priority>
    <properties>
        <property name="xpath">
            <value>
                <![CDATA[
                //Method[ModifierNode[@DeprecatedTestMethod = true()]]
                ]]>
            </value>
        </property>
    </properties>
</rule>
```

```xml
<rule name="AvoidFutureAnnotation" ... language="apex" ...>
    <properties>
        <property name="xpath">
            <value>
                <![CDATA[
                //Method/ModifierNode/Annotation[lower-case(@Name) = 'future']
                ]]>
            </value>
        </property>
    </properties>
</rule>
```

**Apex node names visible in shipped rules:** `Method`, `ModifierNode`, `Annotation`, `UserClass`, `SoqlExpression`, `MethodCallExpression`. Attributes: `@Name`, `@DeprecatedTestMethod`, `@FullMethodName`. Note the XPath axis names drop the `AST` prefix the Java classes carry (`ASTMethod` → `Method`).

Also seen *(secondary — from a blog, not re-verified verbatim)*, an Apex rule of exactly our motivating shape — a SOQL/`Database.query` call inside a non-test method of a non-`*Accessor` class:

```
//UserClass[not(ends-with(@Image, 'Accessor'))]/Method/ModifierNode[@Test=false()]/..//(SoqlExpression | MethodCallExpression[lower-case(@FullMethodName)='database.query'])
```

That `/..//` is doing the work our `for (...) { ... System.debug(...) ... }` needs — "go up to the method, then descend to any depth." It is expressible, and it is unreadable. Worth keeping as the cautionary example of what "query over the AST with a general path language" costs a rule author.

General mechanics: PMD 7 uses XPath 3.1, with language-specific extension functions:

```
//*[pmd-java:nodeIs("Expression")]
//MethodDeclaration[pmd-java:hasAnnotation("java.lang.Override")]
//MethodCall[pmd-java:matchesSig("_#equals(java.lang.Object)")]
//b[pmd:fileName() = 'Foo.xml']
//b[pmd:endLine(.) == pmd:startLine(.)]
```

`matchesSig("_#equals(java.lang.Object)")` is the notable one: a **semantic** predicate (resolved method signature) exposed as an XPath function — the same architectural move as tree-sitter pushing predicates to the caller, filled in with real type information. That is the seam where a type-aware tool adds semantics to a syntactic query language without changing the query language.

**No rewrite side.** The documentation covers detection queries exclusively; there is no replace/fix mechanism analogous to SSR's replace or Refaster's `@AfterTemplate`.

*(The XPath 3.1 version claim was not independently re-fetched in this pass.)*

---

## 11b. rslint

`github.com/rslint/rslint` — "A (WIP) Extremely fast JavaScript and TypeScript linter and Rust crate." Explicitly early/WIP; maintenance status uncertain (not confirmed archived, not confirmed active). No user-facing structural-search pattern DSL was found — its rules appear to be a fixed built-in set. Nothing distinctive to report.

---

## 12. Cross-cutting design questions

### 12.1 How do you make a *fragment* parse with a real language parser?

Five distinct answers exist, in increasing order of generality:

1. **Don't use a real parser.** *(Comby.)* Track balanced delimiters plus string/comment boundaries. Every fragment is legal because nothing is being placed into a grammar category. Cost: no structural knowledge at all.

2. **Make the hole lex as an identifier, then reinterpret.** *(ast-grep step 1, and implicitly Semgrep.)* ast-grep preprocesses the pattern by "replacing `$` with the expando_char" so `$X` lexes as an ordinary identifier, parses the result with the unmodified grammar, then converts the placeholder identifiers back into metavariables. This is *the* core trick and it costs nothing: no grammar fork, no parser modification. **Its ceiling is exact:** you can only put a hole where an identifier is grammatical. Hence ast-grep's documented failure `$LEFT $OP $RIGHT` — "parsers see three consecutive identifiers, not an operator." A hole over an operator, a modifier list, or a type position needs something more.

3. **Try several grammar entry points and take the first that parses.** *(Semgrep, gogrep.)* Semgrep: "If your search pattern is a statement, Semgrep will automatically try to search for it as *both* an expression and a statement." gogrep's README lists its five entry points: a statement, an expression, a type expression, a top-level declaration, an entire file. Cheap, and covers the common cases. The residual problem is ambiguity — when two entry points both succeed, the order decides, silently.

4. **Hand-carve the partial forms you support.** *(Semgrep.)* "Partial statements are partially supported. For example, you can just match the header of a conditional with `if ($E)`, or just the try part of an exception statement with `try { ... }`." A fixed list, not a mechanism. It covers the cases users hit most and leaves everything else as a parse error. The bare-`catch` question lands here: Semgrep supports `try { ... }` because someone added it.

5. **Let the user supply the surrounding context and name the node to extract.** *(ast-grep's `context` + `selector`.)* Verbatim: "First, the code in `context`, `class A { $FIELD = $INIT }`, is parsed as a class declaration. Then, it looks for the `field_definition` node, specified by `selector`, in the parsed tree."

```yaml
pattern:
  context: '{ "a": 123 }'
  selector: pair
```

This is the only *general* answer. Any fragment whatsoever can be expressed as (some valid enclosing code, the node kind to pull out of it). The price is that the user must know grammar node-kind names — which is exactly the knowledge a source-shaped pattern language exists to hide. So the escape hatch from "patterns look like code" leads straight back to "you must know the AST," and every tool that offers it is admitting the pattern literal has a ceiling. ast-grep says so outright: "Patterns are a quick and easy way to match code in ast-grep, but they might not handle complex code. YAML rules are much more expressive."

And the two designs that sidestep the question entirely: **tree-sitter queries** (the query is a separate grammar, so there is no fragment to parse) and **Refaster** (the pattern is a whole compilable class, so there is no fragment either). Both pay in the same currency — you give up the source-shaped literal.

### 12.2 How is "expression vs statement vs type" ambiguity resolved?

Four strategies, and they are genuinely different, not variations:

- **Declare it.** *(Coccinelle.)* `@@ expression E; identifier f; type T; statement S; @@` — every hole's grammatical category is stated up front, along with `constant`, `idexpression`, `local idexpression`, `parameter`, `field`, `declarer`, `iterator`, `position`, `declaration`. No inference, no ambiguity, no silent wrong guess. The cost is a declaration block on every rule; the benefit is that `identifier f` and `expression E` are *different things* the engine can exploit, and that new categories (`iterator`, `declarer`) can be added for constructs the base grammar does not distinguish.

- **Infer from position, with a documented heuristic.** *(ast-grep.)* "Extract the leaf node or the innermost node with more than one child" — walk down single-child chains (which carry no structural information) and stop at the first branching node. The heuristic is right most of the time and, when it is wrong, wrong silently: `a: 123` in JavaScript resolves to a labeled statement when the user meant an object `pair`. `selector:` is the manual override.

- **Try both and union the results.** *(Semgrep.)* "Semgrep will automatically try to search for it as *both* an expression and a statement." Fewer surprises than picking one; more false matches.

- **Make it unrepresentable.** *(Refaster, tree-sitter.)* Refaster: a parameter's declared type *is* its category, checked by javac. tree-sitter: you wrote `(binary_expression)`, so there is nothing to infer.

Note that (1) is the only strategy that scales to categories the parser has but the surface syntax does not disambiguate — which is a live concern for Apex, where an identifier could be a local, a field, a static, a type name, or an SObject field, and where `Foo.Bar` is genuinely ambiguous without resolution.

### 12.3 Which tools are semantic rather than syntactic, and what does it do to the syntax?

| Tool | Semantic capability | How it shows up in the syntax |
|---|---|---|
| CodeQL | full: types, CFG, local + global dataflow, taint | the syntax **is** the semantics — there is no syntactic layer; you write `instanceof IntegralType`, `getParentStmt*()` |
| Refaster | full type checking, via javac | **invisible** — a parameter's declared type is the constraint; no new syntax at all |
| JetBrains SSR | PSI: types, hierarchies, references | a **Type filter** (with "within type hierarchy"), a **Reference filter**, plus `Shorten fully-qualified names` on rewrite. Out-of-band from the pattern text |
| Semgrep | constant propagation, taint mode, type inference | three spellings: a **cast-shaped** inline annotation `(Logger $X).log(...)`; a **separate YAML clause** `metavariable-type`; a **separate rule mode** (`pattern-sources`/`-sinks`) |
| PMD | resolved signatures | an **XPath extension function**: `pmd-java:matchesSig("_#equals(java.lang.Object)")` |
| gogrep | some type info | a **separate filter command**: `-a 'type(string)'` |
| Coccinelle | CFG paths, not types | `...` is CFG-aware, `when forall`/`exists` quantify over paths; types appear only as metavariable constraints (`struct foo *E`) |
| Comby, ast-grep, GritQL, tree-sitter | none | — |

The pattern across every tool that has both: **semantics never go inside the pattern literal.** They go beside it — a separate filter, a separate clause, a separate function, a separate mode. The three ways anyone has managed to put a type *into* the pattern text are (a) borrow a construct the language already has that looks like a type annotation (Semgrep's cast form `(Logger $X)`, Go's `($R : *zip.Reader)`), (b) declare it in a header (Coccinelle's `struct foo *E;`), or (c) make the pattern real code so the language's own type syntax applies (Refaster).

This is the single most transferable observation in the survey for a tool whose distinguishing asset is a resolved type and symbol layer. The syntax question is not "how do I write types in the pattern" but "which of those three shapes does the host language make natural." For Apex, (a) is directly available — Apex has casts, and `((Account) $X).Name` is already valid-looking Apex.

Secondary observation: tree-sitter and PMD show the clean architectural seam. tree-sitter's engine "does not handle predicates directly… they are just exposed in a structured form so that higher-level code can perform the filtering." PMD fills that same slot with `matchesSig(...)`. A structural matcher plus a caller-evaluated predicate hook is how a syntactic engine and a semantic layer compose without either one leaking into the other.

### 12.4 Rewrite: how is the original formatting of untouched sub-matches kept?

Five approaches, in order of increasing ambition:

1. **Replace only the matched byte range; never touch anything else.** The universal baseline. Everyone does this, and it is why comments and blank lines outside a match always survive.

2. **Re-emit the original token stream except at `-`/`+` sites.** *(Coccinelle.)* The unparser preserves the original bytes everywhere unmodified, which is why kernel-wide semantic patches produce minimal reviewable diffs. Strongest fidelity of any tool here, and the reason it is trusted on a codebase nobody will re-format. *(Exact re-indentation policy for `+` lines — `--smpl-spacing` — not verified; see §5.8.)*

3. **Re-indent captured multi-line text relative to its column in the fix template.** *(ast-grep, the only explicit statement anyone makes.)* "ast-grep's rewrite is indentation sensitive. That is, the indentation level of a meta-variable in the fix string is preserved in the rewritten code." Cheap, no formatter needed, and it is precisely the bug Semgrep has open (#3070: later lines "indented on an absolute basis rather than relative to the first line").

4. **Say nothing and let it be wrong.** *(Semgrep, GritQL.)* Semgrep's autofix page makes no formatting claim; the known indentation and same-line-multiple-fix failures live in the issue tracker. GritQL documents nothing on the subject.

5. **Hand the result to a real formatter.** Two flavours: *external* — Comby's FAQ explicitly disclaims stylistic fidelity and recommends piping through `gofmt`; or *internal and semantic* — JetBrains SSR's "Reformat - automatically formats the replaced code," plus "Shorten fully-qualified names - replaces fully qualified class names with short names and imports" and "Use static import," which are rewrites only a name-resolving tool can perform.

Two further mechanisms worth noting because they are about *what span* to replace, not how to format it:

- ast-grep's `expandStart`/`expandEnd` regexes grow the replaced range past the matched node — the trailing-comma case, `fix: { template: '', expandEnd: { regex: ',' } }`. An admission that AST node boundaries are sometimes the wrong edit boundary.
- ast-grep's `rewriters:` + `transform:` apply sub-fixes to each element of a captured sequence and splice the joined result (`joinBy`) into the outer template — the composable answer to "rewrite each argument, then rebuild the call."

And the structural point: **`fix` being a string template rather than a tree transform is a deliberate, near-universal choice.** ast-grep states the constraint outright — "ast-grep rule can only fix one target node at one time by replacing the target node text with a new string." Only Coccinelle (a diff over a token stream) and JetBrains (PSI edits plus a formatter) do anything else. String-template rewrite is what makes formatting preservation nearly free for untouched spans, and it is why multi-line splicing is where every one of these tools has bugs.

---

## 13. The design space

The axes below are what actually varies. Each is stated as a trade, not a recommendation.

### Axis 1 — Is the pattern source-shaped, or is it a query over the tree?

The foundational fork, and it determines most of the rest.

**Source-shaped** (Comby, Semgrep, ast-grep, GritQL, Coccinelle, SSR, gogrep, Refaster): zero grammar knowledge to start; the pattern is readable by anyone who reads the language; it is copy-pasteable from real code. In exchange you inherit the fragment-parsing problem (§12.1), the category-ambiguity problem (§12.2), a ceiling on what a hole can stand for (holes only go where identifiers go), and no way to name a construct the surface syntax does not distinguish.

**Tree-query** (CodeQL, tree-sitter, PMD XPath): no fragment problem, no ambiguity, uniform expressiveness — anything in the tree is addressable, at any depth, with any predicate. In exchange the author must know the node vocabulary before writing a single query, and the PMD Apex example in §11 shows what a moderately complex query looks like in practice.

Every source-shaped tool that got serious ended up growing a tree-query escape hatch: ast-grep's `kind:`/`context:`/`selector:`, GritQL's `augmented_assignment_expression(operator=$op, …)`, JetBrains' Script filter. **The realistic design is not one or the other; it is a source-shaped front end with a principled escape hatch, and the question is how cleanly the two halves meet.**

### Axis 2 — One wildcard or several?

- **One, context-sensitive** (Comby): `:[x]` means different things inside and outside delimiters. Nothing to learn; ambiguous to read; no way to distinguish "one argument" from "all the arguments."
- **Graded** (Semgrep `$X` / `...` / `$...ARGS` / `<... ...>`; ast-grep `$X` / `$$$` / `$$VAR`; GritQL `$x` / `$...`; gogrep `$x` / `$*x`): each granularity gets its own spelling. Precise and self-documenting, at the cost of a sigil zoo — ast-grep's `$`, `$$`, `$$$` mean three unrelated things.
- **Uniform hole plus a count constraint** (JetBrains): one `$var$` spelling; repetition is a `[min,max]` range on the variable. Strictly the most expressive — it is the only design here that can say "two to four arguments" — but the pattern text stops being self-contained, which is SSR's best-known complaint.

A fourth position worth naming: **gogrep folds "optional" into "sequence"** — `for $*_ { $*_ }` matches every form of Go `for` loop because a zero-or-more wildcard in the header slot also matches an empty header. One operator, two jobs, no extra syntax.

### Axis 3 — How is "at any depth" spelled, and is it bounded?

| Design | Spelling | Bounded? |
|---|---|---|
| free, via delimiters | Comby — any hole spanning `{ }` | n/a |
| an operator in the pattern | Semgrep `<... p ...>`; Coccinelle `<... p ...>` (0+) and `<+... p ...+>` (1+) | Coccinelle's two forms distinguish 0+ from 1+ |
| a separate relational clause | ast-grep `inside:`/`has:` with `stopBy: neighbor \| end \| <subrule>` | yes, and the subrule form gives a custom stop condition |
| a modifier in the query language | GritQL `contains p until q`, `within p` | yes, via `until` |
| transitive closure on a relation | CodeQL `getParentStmt*()`, PMD `//` | no |
| none | tree-sitter (structure is explicit), Refaster | n/a |

Two things to take from this row. First, **bounded descent matters in practice** — both ast-grep and GritQL shipped it, independently, with different spellings, because "find X inside Y but not past the next function boundary" is a real query. Second, the motivating `for (...) { ... System.debug(...); ... }` sits exactly here: in Semgrep and Coccinelle it is one pattern literal; in ast-grep it is a pattern plus a relational rule plus `stopBy: end`; in CodeQL it is a `getParentStmt*()` join. **Whether this query is one line or three is entirely determined by this axis.**

### Axis 4 — Does name reuse unify, and what is the scope?

- **Yes, within a pattern** — ast-grep ("You can reuse same name meta variables to find previously occurred AST nodes"; `$A == $A`), gogrep ("All wildcards with the same name within an expression must match the same node, excluding `_`"), GritQL, SSR (via PSI binding).
- **Yes, across sibling clauses** — Semgrep ("metavariables with the same name are treated as the same metavariable within the `patterns` operator"), and across rule *modes* in taint mode.
- **Yes, across *rules*, explicitly namespaced** — Coccinelle: `expression free.E;` inherits `E` from rule `free`. Plus set arithmetic over positions: `position free.p1!=loop.ok,p2!={print.p,sz.p};`. Nothing else here comes close.
- **No — unification is an explicit predicate** — tree-sitter (`#eq? @a @b`, evaluated by the caller) and Comby (`where :[a] == :[b]`; implicit unification is **not documented**).

Every design in the first three rows must then answer: *does `$X` mean the same thing across separate inner matches of a `contains`-style operator?* GritQL's `bubble` exists solely for this, and the docs state the failure plainly: "Absent the `bubble`, only the first `console.log` call would be rewritten… `$message` would try to bind to `'How are you?'` and fail." ast-grep answers it with rule evaluation order — "Only the first rule can specify what a `$META_VAR` matches" — which the FAQ concedes is a source of confusion. **This is a hazard that only appears once you have both unification and a deep-search operator, and it must be designed for deliberately rather than discovered.**

### Axis 5 — Where do non-shape constraints live?

- **In the pattern** — Comby `:[x~regex]`; Semgrep's cast-shaped typed metavariables; Coccinelle's metavariable declarations (`constant char [] c;`, `expression free.E, subE<=free.E`).
- **Beside the pattern, in structured data** — Semgrep's `metavariable-*` clauses; ast-grep's `constraints`/relational/composite YAML; PMD's XPath predicates.
- **In the query language itself** — GritQL's `where { $x <: string(), !$y <: r"…" }`, CodeQL's `where`.
- **In a general-purpose scripting escape** — Coccinelle `@script:python@` / `@script:ocaml@`, JetBrains' Groovy Script filter, tree-sitter's caller-evaluated predicates.

Two consistent findings. **Semantics never go inside the pattern literal** (§12.3) — every tool with type awareness puts it beside. And **every mature tool has a scripting escape**, because the constraint language always runs out; the ones without it (Comby, ast-grep) have the most "I can't express this" complaints in their own FAQs.

### Axis 6 — Rewrite: separate template, or the same text read twice?

- **A separate after-template**: Semgrep `fix:`, ast-grep `fix:`, Comby's second argument, SSR's Replace box, Refaster's `@AfterTemplate`. Simple; you write the shape twice; the two can drift.
- **An operator inside the pattern language**: GritQL `=>`. Composes — it appears nested inside `or { }`, inside `where`, inside `maybe`, inside `contains bubble`.
- **The pattern *is* the patch**: Coccinelle's `-`/`+`. Written once, read twice. Unmodified lines serve as context in both readings, so there is no drift possible, and a reader who knows `diff` already knows the notation. This is the strongest idea in the survey and the least copied.
- **No rewrite at all**: CodeQL, tree-sitter, PMD.

Note Coccinelle's third mode: `*` marks lines of interest for reporting. One notation covers search (`*`), rewrite (`-`/`+`), and pure match (no annotation) with **identical pattern text** — which means a user who writes a search can turn it into a rewrite without rewriting it.

### Axis 7 — Formatting preservation

Covered in §12.4. Summarised as a spectrum: preserve original bytes (Coccinelle) → re-indent relative to the template (ast-grep) → undefined (Semgrep, GritQL) → hand to a formatter (Comby via external `gofmt`, SSR via the IDE's own). The untouched-span guarantee is free for everyone because rewrites are byte-range replacements; **multi-line splicing is where every tool in this survey has bugs or silence**, and ast-grep's explicit relative-indentation rule is the only documented answer that requires no formatter.

### Axis 8 — Is there a normalisation layer?

Only **Coccinelle**, via user-extensible isomorphisms: `X == NULL <=> NULL == X <=> !X`, loaded from `standard.iso`, disableable per rule by name (`@ disable unlikely @`, `@ r disable is_null @`), not run to a fixed point.

Everything else makes the author enumerate variants by hand: Semgrep's `pattern-either`, ast-grep's `any:`, GritQL's `or { }`, Refaster's multiple `@BeforeTemplate`s. That works, but it is per-pattern — every author re-enumerates the same equivalences forever, and a pattern written by someone who forgot one variant silently under-matches.

This is the axis with the widest gap between what one tool does and what everyone else does, and the one most directly reachable for a language with well-known idiom pairs.

### Axis 9 — What does the tool know?

From nothing to everything: Comby (delimiters) → tree-sitter / ast-grep / GritQL (syntax tree) → Coccinelle (syntax + control-flow graph) → Semgrep (syntax + constant propagation + intrafile dataflow + partial types) → PMD/SSR (syntax + resolved types and signatures) → Refaster (a full compiler) → CodeQL (full relational AST + CFG + global dataflow).

Two observations for anyone sitting on an existing semantic layer:

- The *syntax* of the semantic constraint is nearly always bolted on beside the pattern (§12.3), so adding semantics does not require redesigning the pattern language — but it does require deciding early *where* the seam is, because retrofitting is what produced Semgrep's three different spellings for "this metavariable has type T."
- tree-sitter's split — a structural engine that recognises predicates but delegates their evaluation — plus PMD's `matchesSig(...)` show the clean version of that seam: a syntactic matcher with a typed predicate hook. Everything semantic lives on one side of one interface.

### Axis 10 — Distribution

Underrated, and it is the axis on which the most capable tool here loses outright. JetBrains SSR is semantically the richest pattern-literal tool surveyed (real PSI, type hierarchies, Groovy predicates, import-aware rewrites) and is **GUI-only, with no documented CLI**; patterns are shared by IDE export/import rather than as text in a repo. Everything else here is a CLI over plain-text patterns that live in version control and run in CI. Coccinelle's semantic patches are *files in the Linux tree* (`scripts/coccinelle/**/*.cocci`) that CI runs — which is the reason its ideas have had thirty years of real use.

---

## Appendix — the same query, eleven ways

"A `System.debug` call somewhere inside a `for` loop." Transliterated into each notation to make the axes concrete. **These are illustrative transliterations, not verbatim doc examples** (the verbatim source for each notation is in that tool's section above).

```
# Comby
for (:[init]) { :[body] }        where :[body] ~ "System\.debug"   # approximate: no containment operator

# Semgrep
for (...) { ... System.debug(...); ... }

# ast-grep (YAML — cannot be one pattern)
rule:
  pattern: System.debug($$$)
  inside: { kind: for_statement, stopBy: end }

# GritQL
`System.debug($...)` where { $... <: within `for ($_) { $_ }` }

# Coccinelle
@@ @@
  for (...) { <+... System.debug(...); ...+> }

# CodeQL
from MethodAccess m, ForStmt f
where m.getMethod().hasName("debug")
  and m.getEnclosingStmt().getParentStmt*() = f.getStmt()
select m

# tree-sitter
(for_statement body: (_ (expression_statement
  (method_invocation name: (identifier) @n)) ) ) (#eq? @n "debug")

# JetBrains SSR
for ($Init$; $Cond$; $Update$) { $Statements$; }      # + Count [0,∞] on $Statements$,
                                                      # + a Script filter for the debug call

# gogrep (Go shape)
gogrep -x 'for $*_ { $*_ }' -g 'System.debug($*_)'

# Refaster — not expressible: no containment, no statement-level "anywhere inside"

# PMD XPath (Apex, real node names)
//ForLoopStatement//MethodCallExpression[lower-case(@FullMethodName)='system.debug']
```

The spread is the finding. Two tools express it in one source-shaped line (Semgrep, Coccinelle). Two need a second clause (ast-grep, GritQL). Three need tree vocabulary (CodeQL, tree-sitter, PMD). One needs a dialog and a Groovy script (SSR). One cannot express it at all (Refaster). One can only approximate it (Comby).

---

## Confidence notes

Verified verbatim against primary sources: all Comby hole-syntax forms and FAQ caveats; Semgrep's metavariable naming, unification sentence, ellipsis definition, block-scoping caveat, and partial-statement sentence; all ast-grep YAML, the four-step pattern-parse algorithm, the effective-node heuristic, the `context`/`selector` explanation, the indentation-sensitivity sentence, and the "Short answer: NO" FAQ entry; all GritQL examples and the `bubble` explanation; the SmPL grammar manual's `...`/`when`/nest/disjunction passages (read from `main_grammar.pdf` directly) and `kfree.cocci` from the kernel tree; CodeQL's database-model and dataflow quotes and the C++ `for`-loop queries; tree-sitter's predicate-delegation sentence and all query examples; JetBrains' five filters and three replace options; gogrep's full README; Refaster's example and `@Placeholder` constraint; PMD's shipped Apex rule XML.

Flagged as unverified or secondary, and marked inline where they appear:

- **Comby** — whether reusing a hole name in the *match* template implies unification. Not documented either way on `syntax-reference` or `basic-usage`; the documented mechanism is `where :[a] == :[b]`. Worth checking the parser source if it matters.
- **Comby** — `comby-semantic` / type-aware mode. Known only from a package filename and a blog post title.
- **Coccinelle** — `--smpl-spacing` semantics and the exact `+`-line re-indentation policy. The canonical reference is `docs/options.pdf`, not read in this pass. Also, the "macros with insufficient definitions" phrasing is from search results, not the manual.
- **Semgrep** — the formatting-failure GitHub issues (#3070, #3577, #2294) are issue titles, not doc caveats. The `pattern-regex`/generic-matching fallback page was not read.
- **JetBrains** — the filter names "contained in constructor," "read/write access," and "formal argument type" could not be confirmed on the current docs, which list exactly five modifiers (Count, Text, Type, Reference, Script). Do not cite those three without checking the IDE.
- **PMD** — the XPath 3.1 version claim and the `//UserClass[not(ends-with(@Image,'Accessor'))]…` Apex/SOQL rule are secondary (blog), not re-verified.
- **GritQL** — the absence of a `context`/`selector` analogue is an absence of documentation, not a confirmed absence of mechanism. The `language/idioms` page was not read.
- **gogrep** — the README lists the five pattern kinds but does not state the trial-parse order.
- **§13 appendix** — transliterations, not verbatim doc examples.

Raw per-tool research notes, with fuller quote context, are in `.scratch/semantic-query/research/_raw/`.
