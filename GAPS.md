# Query-surface gaps

What `apexls query` could not say, or said wrongly, and where that stands
now. Everything here was found the same way: by driving a real project's
inline SOQL to zero with `--replace` and looking at what refused to
convert. The corpus is NPSP (`tests/corpus/npsp`), 2,132 inline queries across
1,066 Apex files.

The measure of "covered" is deliberately blunt: after the migration in
`docs/moxygen-migration.md` runs, `apexls query 'kind:SoqlExpr'` finds
**0** queries left, and the project contains exactly 2,132
`Selector.*WithBinds` calls -- one per query that was there before, none
lost, none doubled.

## Closed

Each of these blocked real queries; each is now fixed with a test.

### A group could not carry a postfix

`${[${SELECT ... FROM $o ...}]}.Id` did not parse, so `[SELECT ...].Id`,
`[SELECT ...][0]` and `[SELECT ...].isEmpty()` had no spelling at all --
78 NPSP queries, the second largest group of leftovers.

`expr_unary` completed the group and returned it, jumping over the postfix
chain that every other primary goes through. The chain is now a function
(`postfix_chain`) applied to both.

### A FROM alias could not be separated from the object

`FROM $o` binds the whole FROM list, so `FROM AsyncApexJob a` bound
`AsyncApexJob a` and a cast built from it came out `(List<AsyncApexJob a>)`
-- which does not parse, so the rewrite was refused and, with it, every
other edit in that file.

`maybe_alias` refused *every* hole, because a `...` there is the pattern's
trailing clause hole rather than an alias. That is true of `...` only: the
clause hole has no other spelling, so a named hole is unambiguous.
`FROM $o $a` now binds the object and the alias apart.

### A parenthesised subject needed a parenthesised pattern

`f(([SELECT ...]), true)` did not match `f(..., [SELECT ...], ...)`.
Writing a parenthesised twin of every shape is the same rule spelled
twice, so redundant parentheses in the *source* are now transparent: a
pattern that does not write them matches through them, and the match is
the inner expression, so the parentheses stay where the author put them.

A match may not *start* at a parenthesised expression, or its span would
swallow the parentheses -- and a rewrite that drops them changes what
`(a + b) * c` means.

### A glob's `*` stopped at the end of a line

`--and '$q ~ *COUNT()*'` answered "no" for a query wrapped across lines.
Globs are matched against a whole node's text, which in Apex is routinely
several lines, so this was not a corner case: it routed multi-line
`SELECT Count()` queries into the record-list rewrite, cast and all.
`*` now spans newlines.

A wrong "no" from a filter is worse than a wrong "yes": nothing refuses,
the match simply goes somewhere else.

### `--let` dropped the last match inside a multi-line capture

A `--let` keeps the matches that fall inside its source capture. The
containment test used raw node ranges, which run past the last real token
into the whitespace after it, so the *final* match in a capture spanning
several lines looked like it had spilled out.

In the migration this silently emptied the bind map: 125 queries came out
as `Selector.queryWithBinds('... WHERE Id IN :accountIds', new Map<String,
Object>{})` -- a string naming a bind the map does not have, which is a
runtime failure, not a compile error. It now compares significant ranges.

### A group could not mark a type

`${List<AggregateResult>}` did not parse, so a rewrite could change a
declaration's *query* but never its *type* -- which is what an aggregate
conversion needs, since the element type changes with the call.
`type_ref` now accepts a group, and it nests.

### A replacement's type name could be captured by a local one

Not a parser gap but worth recording, because the binder caught it and a
human would not have: a replacement is *text*, not a resolved reference.
`AccessLevel.SYSTEM_MODE` in the migration's template silently bound to
NPSP's own `public enum AccessLevel` in the one class that declares one,
where no `SYSTEM_MODE` member exists. The recipes now write
`System.AccessLevel.SYSTEM_MODE`.

Nothing in the tool can know this -- resolving a replacement against the
scope it lands in is a different job from matching a shape. The practical
rule is to qualify types in replacements, and to run `apexls check`
afterwards and compare, which is what found it.

### `--replace` with no project root rewrote everything below

`find_project_root` falls back to the current directory when no
`sfdx-project.json` is found above it. That is right for searching and
wrong for rewriting: run one directory too high and every Apex file
underneath is in scope. This ate the test corpus twice.

`--replace` with neither a project root nor explicit paths is now refused:

```
error: no sfdx-project.json here or above C:\...\apexls, so --replace has no project to scope it
note: run it from inside the project, or name the files and directories to rewrite
```

Searching from anywhere is still fine.

## Open

### A group cannot be the first token of a statement

`${AggregateResult} $v = $x;` does not parse: at statement start `${`
already means "group this whole statement", and the parser does not
backtrack to try the other reading. Everywhere else a type group works --
after a modifier (`private ${List<X>} $m(...) { ... }`), in a `for`
header, in a parameter list.

So the type of a *local variable declaration* whose type is the first
token cannot be replaced on its own. Match the whole statement instead, or
give the pattern something to start with.

### One match, one span

By design, and worth stating because it is what the remaining work in a
migration consists of. A rewrite replaces the span it matched; it cannot
edit somewhere else in the file, coordinate edits across sites, or carry
state between matches.

The visible consequence in the Moxygen migration is the **type cascade**:
`Selector.aggregateQueryWithBinds` returns `List<Aggregate>`, not
`List<AggregateResult>`, so every variable, parameter and return type that
carries one along has to change too. The recipes convert the aggregate
query and the declaration it sits in; NPSP is left with 128 mentions of
`AggregateResult` across 18 files, and one of them is a genuine type error
the language server reports (`argument of type 'List<Aggregate>' does not
match 'rcfFindCurrency''s declared parameter type 'list<sobject>'`).

That is a rename, not a query rewrite, and a rename wants a different
tool -- one that follows a symbol rather than matching a shape.

### Shape times variant is combinatorial

The FROM alias is captured by a *different pattern* (`FROM $o $a`), so a
recipe that wants the object for a cast needs two versions: one for plain
FROM lists, one for aliased. NPSP's two aliased queries are both
declarations, so the migration has the aliased variants only for
declarations and assignments, not for arguments or postfix.

This is thin rather than broken: a shape with no matching recipe leaves
its query behind, and `apexls query 'kind:SoqlExpr'` reports it. A cast
built from an aliased FROM does not parse, so it is refused and reported
rather than written.

### Converting to dynamic SOQL gives up compile-time checking

Not a tool gap, but the migration's real cost, and it shows up in the
diagnostics: a query inside a string is opaque, so the ~40 SOQL
diagnostics NPSP had (`'npe03__Recurring_Donation__c' is not a valid field
on object 'Opportunity'`) simply disappear. They were not fixed; they
stopped being visible.

## What the corpus run looks like now

| | |
| --- | --- |
| inline queries before | 2,132 |
| `Selector.*WithBinds` calls after | 2,132 |
| queries left (`kind:SoqlExpr`) | 0 |
| DML statements converted | 3,248 |
| files rewritten | 412 of 1,066 |
| files refused (rewrite would not parse) | 0 |

Diagnostics, `apexls check` before against after: 8,258 -> 17,050.

- **10,790 new**, of which 10,780 are one unresolved name or another from
  Moxygen itself (`Selector`, `DML`, `Aggregate`), which is not installed
  in the corpus. Of the remaining ten, nine are casts naming custom
  objects this corpus has no metadata for -- already unresolved over 100
  times before the migration -- and one is the type cascade above.
- **1,998 gone**, 1,909 of them SOQL field and object diagnostics. Those
  queries were not fixed; they moved inside string literals, where
  nothing checks them. That is the price of dynamic SOQL, paid in full.
