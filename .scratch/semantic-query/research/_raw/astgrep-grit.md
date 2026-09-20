# Structural Search-and-Replace Tools: ast-grep vs GritQL

Research for design doc, sourced from primary documentation only (the tool's own docs site, GitHub README, or source), not blog summaries. All quoted text is verbatim from the fetched pages.

---

## 1. ast-grep (ast-grep.github.io)

### 1.1 Verbatim example patterns

**Pattern syntax / metavariables** — [pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html):
```
$META
$META_VAR
$META_VAR1
$_
$_123
```
Back-reference example: pattern `$A == $A` matches `a == a` and `1 + 1 == 1 + 1` but not `a == b`.

**Rule config (search)** — [rule-config.html](https://ast-grep.github.io/guide/rule-config.html):
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

**Atomic rules** — [atomic-rule.html](https://ast-grep.github.io/guide/rule-config/atomic-rule.html):
```yaml
rule:
  pattern: console.log($GREETING)
```
```yaml
rule:
  kind: field_definition
```
```yaml
rule:
  regex: "\w+"
```

**Relational rules** — [relational-rule.html](https://ast-grep.github.io/guide/rule-config/relational-rule.html):
```yaml
rule:
  pattern: await $PROMISE
  inside:
    kind: for_in_statement
    stopBy: end
```
```yaml
kind: pair
has:
  field: key
  regex: 'prototype'
```
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
```yaml
pattern: console.log('hello');
follows:
  pattern: console.log('world');
```
```yaml
inside:
  stopBy:
    kind: function
  pattern: function test($$$) { $$$ }
```

**Composite rules** — [composite-rule.html](https://ast-grep.github.io/guide/rule-config/composite-rule.html):
```yaml
rule:
  all:
    - pattern: console.log('Hello World');
    - kind: expression_statement
```
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

**Rewrite (fix)** — [rewrite-code.html](https://ast-grep.github.io/guide/rewrite-code.html):
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

**Rewriter / transform (rewrite)** — [rewriter.html](https://ast-grep.github.io/guide/rewrite/rewriter.html):
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
```yaml
transform:
  NEW_VAR:
    rewrite:
      rewriters: [rewrite-num, rewrite-str]
      source: $$$LIST
      joinBy: ' + '
```

**Transform (transform.html)**:
```yaml
transform:
  NEW_VAR:
    replace:
      source: $VAR_NAME
      replace: regex
      by: replacement
```
```yaml
transform:
  LIST:
    substring:
      source: $GEN
      startChar: 1
      endChar: -1
```
```yaml
transform:
  KEBABED:
    convert:
      source: $OLD_FN
      toCase: kebabCase
```
Function-style syntax (0.38.3+):
```yaml
transform:
  NEW_VAR: replace($VAR, replace=regex, by=replacement)
  LIST: substring($GEN, startChar=1, endChar=-1)
  KEBABED: convert($OLD_FN, toCase=kebabCase)
```

**context/selector escape hatch** — [pattern-parse.html](https://ast-grep.github.io/advanced/pattern-parse.html):
```yaml
pattern:
  context: '{ "a": 123 }'
  selector: pair
```

### 1.2 "Match anything" spellings

- **One node**: `$META_VARIABLE` — "is a wildcard expression that can match any **single** AST node." Non-capturing form `$_` (underscore prefix means not bound, "micro-optimize pattern matching speed, since we don't need to create a HashMap for bookkeeping"). ([pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html))
- **Sequence of statements/args**: `$$$` — "to match zero or more AST nodes, including function arguments, parameters or statements." Named form `$$$ARGS` captures the list. FAQ warns: "$$$MULTI are lazy, stopping at the first matching node rather than matching all nodes." ([pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html), [faq.html](https://ast-grep.github.io/advanced/faq.html))
- **Arbitrary nesting depth**: no dedicated deep/descendant metavariable syntax inside a *pattern string*. Depth is handled by the **relational rule** `stopBy`. Default: "by default, relational rule will only match nodes one level further" (`stopBy: neighbor`). `stopBy: end` makes ast-grep "search surrounding nodes until it reaches the end." `stopBy` can also be a sub-rule (custom stop condition), e.g. `stopBy: { kind: function }`. ([relational-rule.html](https://ast-grep.github.io/guide/rule-config/relational-rule.html))

### 1.3 Metavariable naming and reuse

- Naming: "Meta variables start with the `$` sign, followed by a name composed of upper case letters `A-Z`, underscore `_` or digits `1-9`." Valid: `$META`, `$META_VAR`, `$META_VAR1`, `$_`, `$_123`. Invalid: `$invalid`, `$Svalue`, `$123`, `$KEBAB-CASE`, `$`. ([pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html))
- Reuse **does** back-reference (structural equality of matched code): "You can reuse same name meta variables to find previously occurred AST nodes." `$A == $A` matches `a == a` but not `a == b`. ([pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html))
- Across separate rule objects (e.g. `pattern` + `inside`), FAQ notes ordering matters: "Rule matching is ordered because previous rules' matched meta-variables can affect later rules. Only the first rule can specify what a `$META_VAR` matches." ([faq.html](https://ast-grep.github.io/advanced/faq.html))
- Unnamed-node capture uses `$$VAR` (double-dollar) — distinct from the `$$$` sequence operator. ([pattern-syntax.html](https://ast-grep.github.io/guide/pattern-syntax.html))

### 1.4 Constraints beyond shape

- `kind:` — matches by tree-sitter node kind directly: "Sometimes it is not easy to write a pattern because it is hard to construct the valid syntax," used when pattern syntax is ambiguous/incomplete. Example: `kind: field_definition`. ([atomic-rule.html](https://ast-grep.github.io/guide/rule-config/atomic-rule.html))
- `regex:` — "The `regex` atomic rule searches the node's full text, including its children, using a Rust regular expression" (Rust regex syntax, so no arbitrary lookahead). ([atomic-rule.html](https://ast-grep.github.io/guide/rule-config/atomic-rule.html))
- `field:` targeting inside `has`, e.g. `has: { field: key, regex: 'prototype' }`. ([relational-rule.html](https://ast-grep.github.io/guide/rule-config/relational-rule.html))
- Relational: `inside`, `has`, `follows`, `precedes`, all with `stopBy: neighbor|end|<subrule>`.
- Composite: `all`, `any`, `not`, `matches` (references a named utility rule by id — "matches is a special composite rule that takes a rule-id string... The rule will match the same nodes that the utility rule matches," enabling reuse/recursion). ([composite-rule.html](https://ast-grep.github.io/guide/rule-config/composite-rule.html))
- "A node will match a rule if and only if it satisfies all fields in the rule object." ([rule-config.html](https://ast-grep.github.io/guide/rule-config.html))

### 1.5 Rewrite side — formatting/indentation/comments

- Core constraint: **"ast-grep rule can only fix one target node at one time by replacing the target node text with a new string."** `fix` is a *string template*, not an AST transform. ([rewrite-code.html](https://ast-grep.github.io/guide/rewrite-code.html))
- Indentation is explicitly handled, not left to chance: **"ast-grep's rewrite is indentation sensitive. That is, the indentation level of a meta-variable in the fix string is preserved in the rewritten code."** — i.e. when a `$$$`-bound multi-line block is substituted, its lines get re-indented to match the indentation of the metavariable's position in the fix template (not simply pasted verbatim, not fully re-parsed/re-printed by a formatter). ([rewrite-code.html](https://ast-grep.github.io/guide/rewrite-code.html))
- Object form of `fix` supports `template` + `expandEnd`/`expandStart` (regex-based) to grow the replaced text beyond the matched node — used for trailing-comma cleanup: `fix: { template: '', expandEnd: { regex: ',' } }`. ([rewrite-code.html](https://ast-grep.github.io/guide/rewrite-code.html))
- Rewriters (`rewriters:` + `transform: { X: { rewrite: { rewriters: [...], source: $$$ARGS } } }`) let you apply sub-fixes to parts of a match and splice results back via a metavariable in the outer `fix` template — this is the mechanism for composing multiple rewritten fragments before final string substitution. "Only the matching rewriter that appears first in the `rewriters` list will be applied." `joinBy` sets an "alternative joiner to join the transformed sub nodes." ([rewriter.html](https://ast-grep.github.io/guide/rewrite/rewriter.html))
- No doc statement found claiming comments are specially preserved beyond ordinary text substitution — since `fix` is string-template substitution of only the matched span, unmatched surrounding text (including comments) is untouched verbatim; but nothing beyond that indentation-sensitivity note is documented about reformatting/comment handling.

### 1.6 Pattern parsing — detailed mechanism and the ambiguity problem

From [pattern-parse.html](https://ast-grep.github.io/advanced/pattern-parse.html), "Deep Dive into ast-grep's Pattern Syntax":

Four-step algorithm:
1. **Preprocess** the pattern text (e.g. replacing `$` with a language-specific placeholder character tree-sitter's own grammar can parse).
2. **Parse** the preprocessed text into a tree-sitter AST (pattern must be syntactically valid/parseable code — **"First and foremost, pattern is AST based."**).
3. **Extract the effective node** using heuristics, or an explicit user-specified `selector`.
4. **Detect wildcards** and convert placeholders back into metavariables.

**Effective node extraction**: rather than matching against the whole tree, ast-grep picks "the most specific node while still keeping all structural information." The heuristic walks down single-child chains (which carry no structural information — just wrapping) and stops at the first node with multiple children, or a leaf. Example: pattern `foo(bar)` collapses through wrapper nodes and resolves to the `call_expression` node, not (say) an outer `expression_statement` or `program` node.

**The ambiguity problem**: some short code snippets parse as different node kinds depending on context, so the "obvious" effective node is not unique. Example given: `a: 123` in JavaScript can parse as either an object property (`pair`) or a labeled statement, and without more context ast-grep's default heuristic picks one (labeled statement) — which may not be what the user intended if they meant an object literal's key-value pair.

**Escape hatch — `context` + `selector`**: when the intended fragment can't stand alone as valid/unambiguous code, wrap it in a pattern object that supplies full valid surrounding code (`context`) plus the tree-sitter node kind to select out of that parse (`selector`):
```yaml
pattern:
  context: '{ "a": 123 }'
  selector: pair
```
This parses `{ "a": 123 }` (valid JS) and then selects the `pair` node inside it — resolving the `a: 123` ambiguity to "object property" rather than the default "labeled statement" guess.

This same `context`/`selector` pattern-object form is also FAQ's prescribed fix for **"My pattern does not work"**: *"The most common scenario is that you only want to match a sub-expression or one specific AST node in a whole syntax tree. However, the code fragment corresponding to the sub-expression may not be valid code."* ([faq.html](https://ast-grep.github.io/advanced/faq.html))

### 1.7 Known ergonomic complaints / failure modes

From [faq.html](https://ast-grep.github.io/advanced/faq.html):
- **"My pattern does not work, why?"** → incomplete/invalid code fragments won't parse; fix via `context`/`selector`.
- **"Pattern cannot match my use case, how?"** → *"Patterns are a quick and easy way to match code in ast-grep, but they might not handle complex code. YAML rules are much more expressive."*
- **"MetaVariable does not work, why?"** → metavariables must match exactly one whole AST node; can't be glued to surrounding text like `use$HOOK` (partial-identifier matching doesn't work).
- **"Multiple MetaVariable does not work"** → `$$$MULTI` is **lazy** — "stopping at the first matching node rather than matching all nodes" (surprising if you expect greedy behavior).
- **"Why is rule matching order sensitive?"** → composite/relational sub-rules are evaluated in order; only the first occurrence of a metavariable name in that order "binds" it, later occurrences constrain via back-reference — ordering therefore changes results, and `all` is recommended to force a specific evaluation order.
- **"Does ast-grep support advanced static analysis?"** → **"Short answer: NO."** No scope analysis, no type information, no control-flow analysis, no data-flow/taint analysis, no constant propagation. Cannot: find undefined variables, resolve types, detect unreachable code, or trace user-input flow.
- **Multi-language rules**: **"ast-grep does not support multiple languages in one rule"** — different ASTs/node kinds per language/grammar; write separate rules per language or target a superset grammar.
- From GitHub Discussions (secondary but illustrative of community friction): a `now()` pattern unexpectedly also matches `pendulum.now()` because plain identifiers/short call patterns are treated as "at least as general," which one user found counter-intuitive coming from grep-style expectations (Discussion #801/#2030 threads on chained-expression and function-usage matching, ast-grep/ast-grep repo).

### 1.8 Syntax-only, explicit statement

**"Short answer: NO."** — in response to "Does ast-grep support advanced static analysis?" — no scope analysis, type information, control/data-flow, taint, or constant propagation. ([faq.html](https://ast-grep.github.io/advanced/faq.html))

---

## 2. GritQL (docs.grit.io / github.com/getgrit/gritql, now maintained at github.com/biomejs/gritql)

Note: primary docs site docs.grit.io is still live and was used below; GitHub org has since moved under biomejs (search results surfaced `github.com/biomejs/gritql` as the current canonical repo — original `getgrit/gritql` redirects/forwards to the Biome-maintained fork per research done for this doc). Flagging this because a design doc citing "getgrit/gritql" may want the updated org name.

### 2.1 Verbatim example patterns

From [language/patterns](https://docs.grit.io/language/patterns) and [tutorials/gritql](https://docs.grit.io/tutorials/gritql):
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
`println` => `console.log`
```
```grit
`println($message)` => `console.log($message)`
```
```grit
augmented_assignment_expression(operator = $op, left = $x, right = $v)
```
```grit
`const $logger = logger.$action($message)` where {
  $special_logger = js"$[action]Logger",
  $logger => $special_logger
}
```
```grit
range(start_line=1, end_line=3) => .
```
```grit
`console.log($_)`
```
```grit
`console.log($message, $...)`
```
Tutorial examples ([tutorials/gritql](https://docs.grit.io/tutorials/gritql)):
```grit
`console.log("Hello world!")`
```
```grit
`console.log($my_message)`
```
```grit
`console.log($my_message)` => `winston.info($my_message)`
```
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
```grit
`console.$method($my_message)` => `winston.$method($my_message)` where {
  $method <: or { `log` => `debug`, `error` => `warn` }
}
```
```grit
`console.log($my_message)` => `winston.error($my_message)` where {
  $my_message <: within `try { $_ } catch($_) { $_ }`
}
```
```grit
`console.log($my_message)` => `winston.info($my_message)` where {
  $my_message <: literal(value="42")
}
```

### 2.2 "Match anything" spellings

- **One node**: any `$identifier` binds a single node; the anonymous form `$_` matches a node "without binding to a value" — used as a wildcard placeholder, e.g. `` `console.log($_)` ``. ([language/patterns](https://docs.grit.io/language/patterns))
- **Sequence (statements/args)**: `$...` — "the spread metavariable ... matches 0 or more nodes, and can be used anywhere a metavariable can be used," e.g. `` `console.log($message, $...)` ``. ([language/patterns](https://docs.grit.io/language/patterns))
- **Arbitrary nesting depth**: the `contains` modifier — "The `contains` keyword is used to modify a pattern to match any node that contains a specific pattern by **traversing downwards through the syntax tree**":
  ```grit
  `function ($args) { $body }` where {
    $args <: contains `x`
  }
  ```
  Deep traversal can be bounded with `until`: **"The `until` modifier is appended to `contains` pattern is used to stop traversal within a `contains` clause"**:
  ```grit
  `console.$_($content)` where {
    $content <: contains `secret` until `sanitized($_)`
  }
  ```
  This is GritQL's analog to ast-grep's `stopBy: end` (unbounded deep) vs `stopBy: <subrule>` (bounded deep) — `contains` alone = unbounded descent, `contains ... until ...` = bounded descent. ([language/modifiers](https://docs.grit.io/language/modifiers))
  There is also `within`, the ancestor-direction counterpart — **"`within` restricts the pattern to only match if the target node appears within code matching another pattern"**:
  ```grit
  `console.log($arg)` where {
    $arg <: within `if (DEBUG) { $_ }`
  }
  ```

### 2.3 Metavariable naming and reuse

- Naming: lowercase-snake style, "must be alphanumeric" and "must conform to the regex `$[a-zA-Z_][a-zA-Z0-9_]*`." ([language/patterns](https://docs.grit.io/language/patterns))
- No pre-declaration needed: **"Metavariables can be used without being declared, simply by replacing some of a code snippet with a metavariable meant to represent the substitution's part of the syntax tree."** ([language/patterns](https://docs.grit.io/language/patterns))
- Reuse is back-referencing within one pattern scope, same as ast-grep — implied by usage like `$logger` bound once then reused, and explicit via the `<:` match operator repeatedly constraining the same variable (e.g. `$my_message <: string()`). The bubble page makes this constraint explicit for the *failure* case (see below).
- **"Rewrites can contain metavariables"** — i.e. the right-hand side of `=>` can reuse names bound on the left. ([language/patterns](https://docs.grit.io/language/patterns))

### 2.4 Constraints beyond shape

- `where { ... }` clause — **"The `where` clause introduces one or more conditions that must be true for the pattern preceding it to execute."** ([language/conditions](https://docs.grit.io/language/conditions))
- Match operator `<:` — **"Grit's most common condition,"** binds/constrains a metavariable against a sub-pattern: `` $message <: `Hello, world!` ``.
- Negation `!` — **"The `!` operator is used to negate a condition."**
  ```grit
  `console.log('$message');` => `console.warn('$message');` where {
    ! $message <: "Hello, world!"
  }
  ```
- Logical `and`/`or` — `` or { `console.log($my_message)`, `console.error($my_message)` } ``; **"The `or` operator is true if any of the conditions are true."**
- `if`/`else` conditional rewrite (branch which rewrite template applies based on a condition).
- Assignment `=` inside `where` to construct/derive new metavariable bindings dynamically.
- Node-kind/type constructor calls act like ast-grep's `kind:`, e.g. `string()`, `literal(value="42")`, `augmented_assignment_expression(operator=$op, left=$x, right=$v)` — named-field access into the AST node's own structure. ([language/conditions](https://docs.grit.io/language/conditions), [language/patterns](https://docs.grit.io/language/patterns))
- Regex constraint via `r"..."` literal: `` !$my_message <: r".+user-facing.+" ``.
- List/collection quantifiers — `some { ... }` ("match a metavariable which represents a list against a pattern which represents some element") and `every or {...}` ("matches only if all elements match"):
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
- `maybe` — optional match that still succeeds if the inner pattern doesn't match:
  ```grit
  `throw new Error($err)` as $thrown => `throw new CustomError($err);` where {
    $err <: maybe string(fragment=$fun) => `{ message: $err }`
  }
  ```
- `after` / relative-position clauses:
  ```grit
  `console.warn($_)` as $warn where {
    $warn <: after `console.log($_)`
  }
  ```
  (analogous to ast-grep's `follows`/`precedes`.) ([language/modifiers](https://docs.grit.io/language/modifiers))

### 2.5 Rewrite side — `=>`, `where`, `bubble`

- `=>` is the rewrite operator: `LHS_pattern => RHS_template`, RHS can be a code template (backtick string) with metavariable substitution, or `.` to mean "delete" (`` range(start_line=1, end_line=3) => . ``).
- `where` attaches conditions to a `=>` rewrite (conditions must hold for the rewrite to fire, shown throughout section 2.1/2.4 examples).
- **`bubble`** — solves the "first match consumes the metavariable" problem: **"The `bubble` clause introduces a new scope without the necessity of defining a separate pattern"**, and **"variables inside the `bubble` clause are isolated from the surrounding code."** Docs explain the failure mode it fixes directly: *"Absent the `bubble`, only the first `console.log` call would be rewritten"* because *"When attempting to match the second `console.log`, `$message` would try to bind to `'How are you?'` and fail"* (since `$message` was already bound from the first match). `bubble` re-opens a fresh binding scope per match so the same pattern can rewrite multiple independent occurrences:
  ```grit
  pattern `function() { $body }` where {
    $body <: contains bubble `console.log($message)` => `console.warn($message)`
  }
  ```
  `bubble(...)` can take explicit captured arguments from the outer scope while still isolating everything else:
  ```grit
  pattern `function $name() { $body }` where {
    $body <: contains bubble($name) `console.log($message)` => `console.warn($message, $name)`
  }
  ```
  ([language/bubble](https://docs.grit.io/language/bubble))
- **Formatting/indentation/comment preservation**: no explicit statement was found in `language/patterns`, `language/modifiers`, `language/conditions`, `language/bubble`, or the tutorial about indentation re-flow or comment preservation mechanics (unlike ast-grep, which has an explicit documented indentation-sensitivity rule). GritQL's docs emphasize it does **structural matching via a real syntax tree** rather than string substitution on the match step (**"GritQL is designed to do _structural_ matching, not just string matching. Every code snippet is automatically converted into a syntax tree before it is matched against the codebase"** — [tutorials/gritql](https://docs.grit.io/tutorials/gritql)), which suggests rewrites are closer to tree-level splicing than ast-grep's plain string-template `fix`, but the docs pages fetched do not spell out the reformatting algorithm explicitly the way ast-grep's rewrite-code.html does.

### 2.6 Pattern parsing note

GritQL's docs pitch the opposite ergonomic tradeoff from ast-grep's ambiguity-heavy model: **"Start simply without learning AST details: any code snippet is a valid GritQL query"** (GitHub README, [github.com/getgrit/gritql](https://github.com/getgrit/gritql)) — i.e., GritQL does not require the "effective node" heuristic / `context`+`selector` disambiguation dance that ast-grep documents at length; no equivalent "pattern-parse.html" deep-dive page was found on docs.grit.io. No page among `language/patterns`, `language/modifiers`, `language/conditions`, `language/bubble` describes an ambiguous-parse fallback mechanism analogous to ast-grep's `context`/`selector`.

### 2.7 Known ergonomic complaints / failure modes

- The single most-documented pitfall is metavariable binding scope across multiple matches within one pattern — solved by `bubble`; the docs frame this as a "gotcha" worth its own page (see 2.5). ([language/bubble](https://docs.grit.io/language/bubble))
- No FAQ/gotchas page equivalent to ast-grep's `advanced/faq.html` was found under docs.grit.io in this research pass (only `language/idioms` surfaced as a "Common Idioms" page in search, not fetched here — flagging as a page worth checking if deeper GritQL failure-mode coverage is wanted for the design doc).

### 2.8 Semantic analysis

No statement found in the fetched GritQL pages claiming type resolution, scope analysis, or other semantic analysis beyond tree-sitter's syntax tree. Docs describe it as structural (tree) matching, not text matching, but do not claim symbol/type resolution. GritQL is built on tree-sitter like ast-grep, so its base matching substrate is likewise syntax-only; nothing in `language/patterns`, `language/modifiers`, `language/conditions`, `language/bubble`, the tutorial, or the GitHub README claims semantic/type-aware matching.

---

## Summary table

| Aspect | ast-grep | GritQL |
|---|---|---|
| One-node wildcard | `$FOO` / `$_` (non-capturing) | `$foo` / `$_` (non-capturing) |
| Sequence wildcard | `$$$` / `$$$ARGS` (lazy — stops at first match, per FAQ) | `$...` |
| Deep/descendant match | No pattern-string operator; relational rule `has`/`inside` + `stopBy: end` (or `stopBy: <subrule>`) | `contains` modifier (unbounded), `contains ... until ...` (bounded) |
| Metavar reuse = back-reference | Yes — quoted explicitly | Yes — implied/used throughout; `bubble` needed to *avoid* back-reference across independent matches |
| Rule/condition format | YAML (`rule:`, `has:`, `inside:`, `all:`, `any:`, `not:`, `matches:`) | GritQL's own query language (`where { }`, `<:`, `!`, `or{}`, `some{}`, `every{}`, `maybe`, `bubble`) |
| Rewrite mechanism | `fix:` string template (indentation-sensitive substitution) or object form with `template`/`expandEnd`; `rewriters`/`transform` for composing sub-fixes | `=>` operator, RHS is a backtick template with metavariable substitution; `.` deletes |
| Type/semantic analysis | Explicitly **no** ("Short answer: NO" — no scope/type/CFA/DFA/taint/constant-prop) | Not claimed in docs fetched; built on tree-sitter syntax trees like ast-grep, no stated semantic layer |

## Source URLs used
- https://ast-grep.github.io/guide/pattern-syntax.html
- https://ast-grep.github.io/guide/rule-config.html
- https://ast-grep.github.io/guide/rule-config/atomic-rule.html
- https://ast-grep.github.io/guide/rule-config/relational-rule.html
- https://ast-grep.github.io/guide/rule-config/composite-rule.html
- https://ast-grep.github.io/guide/rewrite-code.html
- https://ast-grep.github.io/guide/rewrite/rewriter.html
- https://ast-grep.github.io/guide/rewrite/transform.html
- https://ast-grep.github.io/advanced/pattern-parse.html
- https://ast-grep.github.io/advanced/faq.html
- https://docs.grit.io/language/patterns
- https://docs.grit.io/language/modifiers
- https://docs.grit.io/language/conditions
- https://docs.grit.io/language/bubble
- https://docs.grit.io/tutorials/gritql
- https://github.com/getgrit/gritql (README)
