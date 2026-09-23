# TODO: what a Moxygen migration needs

Converting a project's SOQL and DML to
[Moxygen](https://github.com/ZackFra/Salesforce-Moxygen) with
`apexls query --replace`. Moxygen wants DML as wrapper calls and queries
as *strings* with binds in a `Map<String, Object>`:

```apex
insert acc;                                  // ->  DML.doInsert(acc, true);
List<Account> a = [SELECT Id FROM Account WHERE Name = :name];
// ->
List<Account> a = (List<Account>) Selector.queryWithBinds(
    'SELECT Id FROM Account WHERE Name = :name',
    new Map<String, Object>{ 'name' => name },
    AccessLevel.USER_MODE
);
```

Counts below are the NPSP corpus (`tests/corpus/npsp`), measured with the
current build, so the work can be sized against something real.

## Already works

DML converts today, and every rewritten file still parses. Tried on a copy
of NPSP: 3,216 statements across 271 files.

```sh
apexls query 'insert $x;'    -r 'DML.doInsert($x, true);'    # 2,509
apexls query 'update $x;'    -r 'DML.doUpdate($x, true);'    #   590
apexls query 'delete $x;'    -r 'DML.doDelete($x, true);'    #   119
apexls query 'upsert $x $f;' -r 'DML.doUpsert($x, $f, true);'#     1
```

## 1. DML forms the four patterns above miss

Each is a pattern plus a template, no new machinery -- but each needs its
own Moxygen call shape decided first.

| Form | NPSP | Note |
| --- | --- | --- |
| `upsert $x;` | 71 | no external id: which `doUpsert` overload? |
| `undelete $x;` | 14 | |
| `merge $a $b;` | 17 | Moxygen may have no wrapper; check |
| `insert as user $x;` | 0 here | access-level forms exist in the language |
| `Database.insert(...)` | 46 | already a call; different rewrite |
| `Database.update(...)` | 39 | |
| `Database.query(...)` | 327 | already a string: the easy query case |

## 2. Substitute captures inside a template's string literal

**Blocker.** `-r "Selector.query('\$1')"` writes the literal text `'$1'`
into the file: the replacement scanner treats string contents as opaque,
as the pattern side deliberately does. Every query rewrite needs the
captured text *inside* a string, so nothing else in this list matters
until this works.

Where: `Replacement::compile` (`crates/apexls/src/query.rs`) scans with
`apex_parser::hole_spans`, which folds holes but never looks inside a
literal. It needs a template-only pass that also substitutes within
literals, leaving the pattern side untouched.

## 3. Let a group sit inside a SOQL expression

**Blocker.** `${...}` attaches where a construct starts -- statement,
prefix expression, member, declared name -- so the smallest capturable
thing is the whole `[SELECT ...]`, brackets included. The string needs the
*inner* query text, so a group has to be allowed inside the brackets:

```sh
apexls query '[${SELECT ... FROM $o ...}]' -r "\$1 => ..."
```

Where: the group hooks in `crates/apex-parser/src/grammar/` (statement,
expression, declaration) plus one in `soql.rs` around the query body.

## 4. Template transforms, starting with quoting

**Blocker.** Pasted text goes in verbatim, but a string literal cannot
hold a raw `'` or a newline:

- 32 NPSP queries contain a string literal, so pasting them into
  `'...'` produces broken Apex.
- 269 query sites span more than one line; Apex string literals do not.

So a template needs something like `$1:quote` -- escape `'` and `\`,
collapse newlines and runs of whitespace to single spaces. Useful well
beyond this migration.

Where: `ReplacementPart::Capture` gains a transform; parse `:name` after
the capture in `Replacement::compile`.

## 5. Bind extraction

**The real work, and not a template job.** 1,387 of NPSP's 2,132 queries
contain at least one bind. Moxygen keeps `:name` inside the string and
expects a matching map entry, so a migration has to, for every query:

1. find each bind expression (`:opp.Id`, `:new Set<Id>(ids)`),
2. invent a unique, legal map key per bind,
3. rewrite the string to use that key,
4. build `new Map<String, Object>{ 'key' => expr, ... }`.

A capture-and-template language cannot express "for each of a variable
number of scattered sub-expressions, rename it and accumulate a map".
Two ways to get it:

- **A dedicated pass** (a small Rust codemod over the parsed tree, reusing
  the matcher to find query sites). Most direct; one job, one tool.
- **Or a general escape hatch**: let a replacement call out to a script
  with the match's captures as JSON and splice back what it prints. Much
  bigger decision -- it makes replacements arbitrary code -- but it would
  cover every future migration of this shape, not just this one.

The pass is the smaller, more honest first step; the escape hatch deserves
its own design discussion.

## 6. Cast and result shape

`Selector.queryWithBinds` returns `List<SObject>`, so a rewrite needs the
cast (`(List<Account>)`) and must know the queried object -- `$o` from the
pattern's `FROM` gives that. Single-row uses (`Account a = [SELECT ...]`)
need `[0]` or a different call, so they are a separate pattern from the
list case. Worth splitting the migration into: list-assigned queries,
single-row-assigned queries, `for (X x : [SELECT ...])` loops, and queries
used as a bare argument.

## 7. Safety rail (independent of Moxygen)

`apexls query --replace` run from a directory with no `sfdx-project.json`
above it treats the whole tree as the project. Run from this repo's root,
that swept in `tests/corpus/npsp` and rewrote 493 files. Refuse `--replace`
when no project root is found unless explicit paths are given.

## Suggested order

1. Item 7 (safety), then item 1 (the rest of the DML) -- both small, and
   item 1 is already usable value.
2. Items 2, 3, 4 -- together they make `Database.query(...)`-style
   conversions and every no-bind query expressible with templates alone.
3. Item 5 -- decide pass versus escape hatch, then build it.
4. Item 6 falls out as patterns written against the above.
