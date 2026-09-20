# Structural Search-and-Replace Tools: Comby vs Semgrep (research notes for design doc)

All claims sourced from primary docs: comby.dev/docs/*, github.com/comby-tools/comby, and docs.semgrep.dev/* (semgrep.dev/docs/* 301-redirects there — used as canonical URL below). Where the fetch tool's summarization was imprecise, I've flagged it as paraphrase rather than presenting it as a direct quote.

---

## COMBY

### 1. Verbatim example patterns (match / rewrite)

```
comby 'fmt.Println(:[args])' 'fmt.Println(fmt.Sprintf("comby says %s", :[args]))' .go
```
— match/rewrite CLI form. Source: https://comby.dev/docs/basic-usage

```
Array.prototype.slice.call(:[arguments]);
```
→
```
Array.from(:[arguments])
```
— `.toml` config, `[my-first-pattern]`. Source: https://comby.dev/docs/configuration

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
— swaps argument order unless function is named `divide`. Source: https://comby.dev/docs/configuration

```
if (:[condition])
```
→
```
if (1)
```
— README's headline example (constant-folding `if` conditions in C-like syntax). Source: https://github.com/comby-tools/comby

```
if (:[left_side] && :[right_side])
where :[left_side] == :[right_side]
```
— equality constraint via `where`. Source: https://comby.dev/docs/advanced-usage

```
where :[left_side] == :[right_side], :[left_side] != "x == 500"
```
— comma-joined = logical AND of multiple `where` conditions. Same source.

```
dict(:[args])
where rewrite :[args] { ":[[k]]=:[[v]]" -> "\":[k]\": :[v]" }
```
— nested rewrite rule that transforms python kwargs into a dict literal. Same source.

```
where match :[left_side] {
| "x == 500" -> true
| "x == 600" -> false
}
```
— pattern-match/switch rule form. Same source.

```
where match :[hole] {
| ":[_~\\d+]" -> true
| ":[_]" -> false
}
```
— regex submatch inside a `match` rule (note backslash must be doubled in the quoted pattern). Same source.

```
var a_:[id()] = 42
anon_:[id(my_label)] = func(){:[body]}
```
— fresh/unique identifier generation via `:[id()]`. Same source.

### 2. "Match anything" spelling

Comby has **one hole mechanism**, not separate operators per granularity — granularity is inferred from where the hole sits:
- **Single expression / bounded token run**: `:[var]` — "matches in a lazy fashion" and, outside delimiters, "stop[s] matching at a newline, or the start of a code block, whichever comes first." (comby.dev/docs/syntax-reference)
- **Sequence of statements/arguments**: the *same* `:[var]` syntax, but placed inside a delimiter pair, e.g. `{:[v1], :[v2]}` or `(:[v])` — because it's bounded by balanced delimiters it's allowed to span newlines/multiple items. There's no distinct "statement-sequence operator" the way Semgrep has `...`; it's overloaded onto the same hole.
- **Unnamed / "don't care"**: `:[_]`.
- **Arbitrary nesting depth**: no separate deep-operator; matching balanced delimiters (parens/braces/brackets) is comby's default behavior for *any* hole that spans a delimiter pair — it just walks nested balanced groups. (comby.dev/docs/advanced-usage, README)

### 3. Captures / metavariables (holes)

- Named: `:[var]`. Alphanumeric-only variant: `:[[var]]` (matches "one or more alphanumeric characters and `_`" — used e.g. for identifiers like function names). Source: https://comby.dev/docs/syntax-reference
- **Back-reference / reuse**: the docs do **not** state explicitly, in the syntax-reference page, that reusing a hole name in the *match* template requires identical text (my fetch of that page turned up nothing on this). What the docs *do* show is the `where :[left_side] == :[right_side]` **rule**-level equality check (advanced-usage) — i.e., Comby's documented mechanism for "these two captures must be equal" is an explicit `where X == Y` clause, not implicit unification from reusing a hole name twice in the match template. I could not find a doc sentence confirming implicit unification the way Semgrep documents it — treat this as an open question for the design doc, not a confirmed capability.
- Reuse in the **rewrite** template is core to the tool: whatever a hole captured in the match template can be re-emitted (and reordered, as in `:[1]`/`:[2]` swap example above).

### 4. Constraints beyond shape

- Regex on a hole's content: `:[var~regex]` (PCRE). Source: syntax-reference.
- Rule-level equality/inequality: `where :[a] == :[b]`, `where :[a] != "literal"`, comma-chained AND (advanced-usage, verbatim above).
- `match { case -> result }` rule construct for multi-way branching on a hole's captured text, including regex arms (`":[_~\\d+]" -> true`). Same source.
- Nested/sequential `rewrite :[args] { ... -> ... }` sub-rewrites (verbatim above) — but **"It is not currently possible to nest rewrite statements"** (comby.dev/docs/advanced-usage) — i.e. you can chain sequential rewrite rules but not nest one rewrite rule inside another.
- No "not inside X" / scoping operator was surfaced by the docs I fetched (nothing like Semgrep's `pattern-not-inside`); Comby's rule language is match/rewrite/equality/regex-submatch, not a positional-containment DSL.

### 5. Rewrite syntax + formatting/whitespace/comment preservation

- Rewrite template: the second string argument to `comby`, or the `rewrite=` key in `.toml` configs; same hole syntax as match, holes are substituted with the captured text.
- **Whitespace matching (not rewrite output) is explicitly documented as loose**: "Whitespace in the template, like a single space, multiple contiguous spaces, or newlines are interpreted all the same: Comby will match the corresponding whitespace in the source code, but will not care about matching the exact number of spaces, or distinguish between spaces and newlines." (https://comby.dev/docs/basic-usage)
- **Indentation is explicitly NOT modeled**: "Comby does not currently consider whitespace indentation significant. We have plans to support it though!" (FAQ, https://comby.dev/docs/faq)
- **Formatting/stylistic preservation on rewrite is explicitly disclaimed**: "Comby is not well-suited to stylistic changes and formatting like 'insert a line break after 80 characters.'" The FAQ's own recommended mitigation is to pipe Comby's output through a language-specific formatter (e.g. `gofmt`) afterward — Comby does not attempt this itself. (FAQ)
- No explicit statement was found about comment preservation specifically, beyond the parser description below (comments are treated as an opaque/skippable syntactic unit during matching, not specially preserved/reformatted on rewrite).

### 6. Parser model

Comby is explicitly **not** built on each target language's real parser. FAQ, verbatim: it "turns patterns into an executable routine (a language-aware parser) where the tree structure is implicit," and this design trades away "the ability to recognize many predefined language-specific constructs." (https://comby.dev/docs/faq) The README/overview frames this as a **"parser-lite"** approach based on **balanced delimiters** — it tracks nesting of parens/brackets/braces and respects string/comment boundaries, so a hole spanning `{ ... }` knows where the matching `}` is, but there is no full grammar, no AST, and (per FAQ) no built-in disambiguation of language-specific syntactic categories the way a real parser would. This is also why it needs no separate "expression vs statement" entry-point logic the way Semgrep does — it isn't parsing into any such taxonomy to begin with.
- Comparison to Coccinelle (also cited in FAQ): Comby is "more robust to matching patterns in the presence of unrecognized constructs" than a strict language parser would be — i.e. the lack of a full parser is framed as a *feature* for handling malformed/partial/unusual code, not just a limitation.

### 7. Known ergonomic complaints / limitations (from docs' own caveats)

- No indentation-awareness (Python noted as a case that "still works" only for non-indentation-dependent code) — FAQ.
- Not suited to pure formatting/style changes; needs an external formatter pass afterward — FAQ.
- No nested rewrite statements — advanced-usage.
- "Custom syntax is only partially supported in rules" — advanced-usage.
- The `match`-rule / pattern-matching feature is called out as "in active development and may change" — advanced-usage.
- Cannot customize the `_` wildcard's matching semantics or bind custom syntax to alternative match semantics — advanced-usage.
- No published benchmark vs. alternatives; the docs frame tool choice as "probably comes down to your expressive needs rather than speed" — FAQ.
- I was not able to pull specific GitHub issue threads (the issues list didn't render individual entries through the fetch); the one substantive external note found is Comby's own 2022 blog post title "Find and replace with type information," implying pre-that-feature false positives from purely syntactic (non-type-aware) matching — https://comby.dev/blog/2022/08/31/comby-with-types (post body not fetched; flag as lead, not verified quote).

### Does Comby resolve types / semantics?

Not by default — matching is syntactic/balanced-delimiter based per the FAQ description above. Comby ships a **separate** `comby-semantic` opam package (seen only as a filename, `comby-semantic.opam`, in the GitHub repo listing) suggesting an add-on semantic/type-aware mode exists, consistent with the "Find and replace with type information" blog post title, but I did not fetch enough to describe its mechanics — flag as follow-up if the design doc needs Comby's type-aware story in detail.

---

## SEMGREP

### 1. Verbatim example patterns

```
insecure_function(...)
```
— ellipsis matches any/all arguments. Source: https://docs.semgrep.dev/writing-rules/pattern-syntax

```
func(1, ...)
```
matches both `func(1)` and `func(1, "extra", False)` — leading-fixed-arg + trailing ellipsis. Same source.

```
$O.foo(). ... .bar()
```
— ellipsis spanning an arbitrary method-chain segment. Same source.

```
crypto.set_secret_key("...")
```
— ellipsis inside a string literal to match any string content. Same source.

```
user_list = [..., 10]
```
— ellipsis inside a list/container pattern. Same source.

```
foo($...ARGS, 3, $...ARGS)
```
— named ellipsis metavariable (captures a variable-length argument sublist, reused). Same source.

```
def $FUNC(..., $ARG={}, ...):
```
— classic "mutable default argument" Python anti-pattern. Same source.

```
$X == (char *$Y)
```
— typed metavariable constraint inline in a C pattern. Same source.

```
if <... $USER.is_admin() ...>:
```
— deep expression operator: matches an admin check arbitrarily nested inside a larger boolean expression. Same source.

```yaml
rules:
  - id: md5-usage
    languages: [python]
    message: Found md5 usage
    pattern: hashlib.md5(...)
    severity: HIGH
```
Source: https://docs.semgrep.dev/writing-rules/rule-syntax

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
Same source.

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
Same source.

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
Same source.

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
— `metavariable-comparison` evaluates a Python-like boolean expression over the captured value. Source: https://docs.semgrep.dev/writing-rules/rule-syntax

```yaml
rules:
  - id: excessive-permissions
    languages: [python]
    message: module setting excessive permissions
    patterns:
      - pattern: set_permissions($ARG)
      - metavariable-comparison:
          comparison: $ARG > 0o600
          metavariable: $ARG
          base: 8
    severity: HIGH
```
— `base:` field for non-decimal numeric literals. Same source.

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
— `metavariable-type` filters by the *inferred type* of what the metavariable bound to (not string-matching a type annotation). Source: https://docs.semgrep.dev/writing-rules/experiments/metavariable-type

```yaml
rules:
  - id: use-sys-exit
    languages: [python]
    pattern: exit($X)
    fix: sys.exit($X)
    severity: MEDIUM
```
— autofix. Source: https://docs.semgrep.dev/writing-rules/autofix

```yaml
- id: python-typing
  pattern: from typing import $X
  fix: ""
  languages: [python]
  severity: ERROR
```
— autofix that deletes the match. Same source.

```yaml
pattern-sources:
  - pattern: source(...)
pattern-sanitizers:
  - pattern: sanitize(...)
pattern-sinks:
  - pattern: sink(...)
```
— taint-mode skeleton. Source: https://docs.semgrep.dev/writing-rules/data-flow/taint-mode

### 2. "Match anything" spelling

Semgrep distinguishes these explicitly, unlike Comby:
- **One node/single item, unnamed**: `$_` (anonymous metavariable; can be repeated without forcing equality).
- **A sequence of zero or more items (statements, arguments, params, fields, characters)**: `...` — the ellipsis operator (paraphrase-quality summary of https://docs.semgrep.dev/writing-rules/pattern-syntax — re-verify exact wording before quoting in the doc).
- **A named/captured sequence of arguments**: `$...ARGS` (ellipsis metavariable) — e.g. `foo($...ARGS, 3, $...ARGS)`.
- **Arbitrary nesting depth (deep expression operator)**: `<... pattern ...>` — verbatim: "Use the deep expression operator `<... [your_pattern] ...>` to match an expression that could be deeply nested within another expression." (https://docs.semgrep.dev/writing-rules/pattern-syntax)
- **Important scope-limit caveat documented for `...`**: "The `...` ellipsis operator matches everything in its current scope. The current scope of this operator is defined by the patterns that precede `...` in a rule." And explicitly: **"The ellipsis operator does *not* jump from inner to outer statement blocks."** — with a worked counter-example: given
  ```
  if cond:
      foo()
  baz()
  bar()
  ```
  a pattern expecting `...` to span from `foo()` inside the `if` block out to `bar()` at the outer level will **not** match — ellipsis is scoped to one block, not the whole control-flow graph. (https://docs.semgrep.dev/writing-rules/pattern-syntax)

### 3. Captures/metavariables, naming, back-reference

- Syntax: `$X`, `$WIDGET`, `$USERS_2` — "They begin with a `$` and can only contain uppercase characters, `_`, or digits." (pattern-syntax)
- **Reuse = unification, confirmed by name**: Semgrep's own docs call this **"metavariable unification"** — for search-mode rules, "metavariables with the same name are treated as the same metavariable within the `patterns` operator," which is what forces reused occurrences to bind to the same underlying value (surfaced via WebSearch snippet of the same pattern-syntax page — pull the exact verbatim sentence from https://docs.semgrep.dev/writing-rules/pattern-syntax directly before quoting, since my direct fetch of the live page paraphrased it as "Re-using metavariables shows their true power" without capturing the defining sentence verbatim).
- Cross-clause unification is explicitly documented for taint mode: "A metavariable defined in `pattern-sinks` and `pattern-sources` with the same name is treated as the same metavariable." (same page)
- Ellipsis metavariable `$...ARGS` captures a variable-length slice, reusable the same way.

### 4. Constraints beyond shape

- **`metavariable-regex`**: regex (PCRE2) on the captured text. Example: `{metavariable: $METHOD, regex: (insecure)}` (rule-syntax).
- **`metavariable-comparison`**: arbitrary Python-like boolean expression over the value, with numeric literal `base:` support (`base: 8` for octal) — verbatim examples above.
- **`metavariable-type`**: filters by the language's own type inference/type checker, not text — example above (`type: String` on a Java metavariable bound via `$X == $Y`).
- **`metavariable-pattern`**: match a sub-pattern *within* what a metavariable captured (docs note it "tries to match the pattern within the captured metavariable" — and interacts with reserved-word parsing quirks; the KB page's documented workaround is to fall back to `metavariable-regex`, which "runs a regex on the text range... ignoring how its content would be parsed," when `metavariable-pattern` hits reserved-word parse errors. https://docs.semgrep.dev/kb/rules/pattern-parse-error)
- **`pattern-not` / `pattern-not-inside`**: negative containment constraints — e.g. the `unverified-db-query` and `return-in-init` examples above.
- **`pattern-inside`**: positive containment ("this match must occur inside a match of this other pattern") — `return-in-init` example.
- **Repetition/counts**: no dedicated "N times" quantifier was surfaced in the fetched pages; repetition is expressed structurally via ellipsis + fixed args (`func(1, ...)`), not via a counting operator.
- **`options:`** rule field can disable matching features per-rule, e.g. `constant_propagation: false` (https://docs.semgrep.dev/writing-rules/data-flow/constant-propagation).

### 5. Rewrite (`fix:`) syntax + formatting/whitespace/comment preservation

- `fix:` is a top-level rule field: "Simple search-and-replace capability" (rule-syntax schema table) using the same `$METAVAR` substitution as the match side. `fix: ""` deletes the match entirely (verbatim example above).
- Applied via `--autofix` (and dry-run tested with `--autofix --dryrun` together) — docs' own wording: "You can apply the Rule-defined fix directly to the file using the `--autofix` flag. To test the fix before applying it, use both the `--autofix` and `--dryrun` flags." (autofix page)
- **The autofix doc page itself makes NO explicit claim about whitespace/comment/formatting preservation** — that silence is itself notable for the design doc (contrast with Comby's FAQ, which explicitly disclaims formatting fidelity).
- **Known real-world formatting failure modes (GitHub issues, not doc caveats)**:
  - Multiline fix indentation bug: "If a user inputs an autofix that is multiple lines, all lines after the first are indented on an absolute basis rather than relative to the first line" — github.com/returntocorp/semgrep issue #3070, "Multiline auto-fix indentation is wrong."
  - Same-line multiple-fix corruption: applying autofix to two matches on the same line can garble/duplicate output — github.com/returntocorp/semgrep issue #3577, "interspersed autofix of multiple expressions on same line."
  - Broader tracking issue github.com/returntocorp/semgrep #2294 ("Autofix mega issue"); autofix is described (WebSearch synthesis, verify wording) as "an experimental feature which... receives limited support."

### 6. Parser model — real parser, and how partial patterns resolve

Semgrep **does** parse patterns with the target language's real parser/grammar, not a lite approximation — and this is exactly why partial/non-standalone patterns are a documented source of friction:
- Multi-entry-point behavior, verbatim: **"If your search pattern is a statement, Semgrep will automatically try to search for it as *both* an expression and a statement."** (pattern-syntax page)
- Explicit partial-statement support, verbatim: **"Partial statements are partially supported. For example, you can just match the header of a conditional with `if ($E)`, or just the try part of an exception statement with `try { ... }`."** (pattern-syntax page) — this directly answers the "bare catch block" question in spirit: Semgrep supports matching a truncated *header* of certain constructs (if-header, try-header) as a documented special case, not as a general "any partial fragment parses" rule.
- Outside those documented partial-statement exceptions, a pattern must be complete. A "Pattern parse error" means the pattern "does not look like complete source code in the selected language" (WebSearch synthesis of docs.semgrep.dev/kb/rules/pattern-parse-error and the troubleshooting/rules page — my direct fetch of the KB page actually surfaced a different, reserved-word-conflict scenario rather than this "complete expression or statement" wording, so verify verbatim wording directly before quoting in the design doc). Example given: `if $X < 5` is invalid and must become `if $X < 5: ...` to parse.
- The KB page I fetched cleanly documents a **different but related** parse-ambiguity gotcha: reserved-word conflicts inside `metavariable-pattern`, with the documented fix being to switch to `metavariable-regex`, verbatim: "metavariable-pattern tries to match the pattern within the captured metavariable, which is going to be affected by how reserved keywords are parsed, while metavariable-regex runs a regex on the text range associated with the metavariable, ignoring how its content would be parsed and bypassing the issue." (https://docs.semgrep.dev/kb/rules/pattern-parse-error)
- There is also a documented **generic/non-parser fallback mode** — `pattern-regex`/"Generic pattern matching" (https://docs.semgrep.dev/writing-rules/generic-pattern-matching, page title only fetched, not body) — for languages/cases where Semgrep has no real grammar; worth a follow-up fetch if the design doc needs Semgrep's degraded-mode story alongside Comby's always-lite approach.

### 7. Known ergonomic complaints / gotchas (docs' own caveats + issues)

- Ellipsis is block-scoped, not CFG-scoped — the "does not jump from inner to outer statement blocks" gotcha above is the single most-cited ellipsis footgun and is explicitly called out in the docs themselves (not just an issue tracker complaint).
- `metavariable-pattern` + reserved keywords → silent/confusing parse failures; documented workaround is switching to `metavariable-regex` (KB page above).
- Autofix explicitly labeled experimental/limited-support; multiline-indentation and same-line-multiple-fix bugs are open, acknowledged issues (GitHub #3070, #3577, #2294) rather than one-off user error.
- Pattern-parse-error is a recurring rough edge for anyone porting a fragment-style mental model (comby-like "any substring") onto Semgrep — patterns must be complete expressions/statements except for the specifically-carved-out partial-statement cases (`if (...)`, `try { ... }`).

### Semantic / type-resolution capabilities (docs-covered)

- **Constant propagation**: yes, documented. Verbatim: it "tracks whether a variable *must* carry a constant value at a given point in the program," across Boolean/numeric/string constants. Semgrep CE does this **intrafile** only; interprocedural/interfile constant propagation is gated to the paid AppSec Platform. Mutable-object caveat: Semgrep assumes called functions don't mutate a "constant" object, flagged as a source of false positives in languages with mutable strings (C, Ruby), with an exception for "method calls whose returning value is ignored" (treated as potentially mutating in Ruby to cut false positives). Java example given: `private String REGEX = "(a+)+$";` propagates, `public final String REGEX = "(a+)+$";` also propagates (private-or-final signals immutability); a plain `public` non-final field does not propagate across classes. Disable via `options: {constant_propagation: false}`. (https://docs.semgrep.dev/writing-rules/data-flow/constant-propagation)
- **Taint mode**: yes, explicitly dataflow-based, not syntactic. Verbatim: "Taint analysis is a dataflow analysis that tracks the flow of untrusted, or **tainted**, data throughout the body of a function or method," flowing "from sources to sinks through **propagators**, such as assignments and function calls." Rule shape uses `pattern-sources` / `pattern-sanitizers` / `pattern-sinks` (example above), and metavariables are unified across these clauses by name. (https://docs.semgrep.dev/writing-rules/data-flow/taint-mode)
- **Typed metavariables**: yes — both inline typed-metavariable syntax in the pattern itself (e.g. Java `(Logger $X).log(...)`, Go `($READER : *zip.Reader).Open($INPUT)`, C `$X == (char *$Y)`) and the standalone `metavariable-type` filter (`type: String` example above), which the docs frame as the cleaner replacement for the older inline-cast-style syntax.
- **`pattern-inside` / `pattern-not-inside`**: positional/structural containment, not full semantic scope resolution, but does let you constrain matches to "inside a function/class matching this other pattern" (e.g. `return-in-init` example) — closer to syntactic nesting than semantic call-graph resolution.
- No evidence surfaced in the fetched pages of full call-target/type resolution equivalent to a real type-checker's symbol resolution (e.g. resolving which overload or which class hierarchy a method call binds to) beyond what constant-propagation and typed-metavariable inference provide — flag as a follow-up if the design doc needs a definitive negative claim here; I did not fetch a page that explicitly disclaims this.

---

## Summary table for the doc

| Question | Comby | Semgrep |
|---|---|---|
| Real language parser? | No — "parser-lite," balanced-delimiter tracking, tree structure implicit (FAQ) | Yes — real grammar; patterns must parse as complete expr/stmt except documented partial-statement carve-outs |
| Handles malformed/partial code gracefully? | Yes, by design (more robust than Coccinelle per FAQ) | No — pattern parse errors are a named, documented failure mode |
| Match-anything spelling | One hole `:[x]` overloaded by context (delimiter-bounded = sequence) | Distinct operators: `$_`/`...`/`$...ARGS`/`<... ...>` |
| Reuse = must-match-identical? | Not documented for match-template reuse; explicit only via `where a == b` rule | Yes, documented as "metavariable unification" |
| Type/semantic awareness | No by default (separate `comby-semantic` package hinted, not detailed here) | Yes — constant propagation, taint mode, typed metavariables, `metavariable-type` |
| Rewrite formatting fidelity | Explicitly disclaimed for stylistic changes; indentation not modeled (FAQ) | Not addressed in docs; known GitHub-issue-level indentation/garbling bugs on multiline/same-line fixes |

### Gaps / follow-ups if the design doc needs more
- Comby: whether match-template hole reuse implicitly unifies (no doc sentence found either way) — worth a direct check of the full https://comby.dev/docs/syntax-reference page source rather than the summarized fetch.
- Comby-semantic (type-aware mode): only surfaced as a filename/blog title, not documented here.
- Semgrep generic-pattern-matching fallback (regex-only languages): title only, not fetched in depth.
- Exact verbatim sentence for Semgrep's "metavariable unification" definition and the KB page's "complete expression or statement" wording should be pulled directly (my automated fetches of those exact passages returned adjacent-but-not-identical content across two attempts) before quoting them as verbatim in a published design doc.
