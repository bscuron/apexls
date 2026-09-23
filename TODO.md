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

## 2-5. Done: `--let`, transforms, groups in SOQL, template strings

All four blockers are built, and the query half of the migration now runs
as one command. Nothing in the tool knows what a SOQL bind is:

```sh
apexls query '$_ $v = ${[${SELECT ... FROM $o ...}]};'   --and '$2 ~ *WITH USER_MODE*'   --let 'q = $2 ~ :$e => :$e:id'   --let 'q = $2 ~ kind:SoqlWithClause => '   --let "m = \$2 * :\$e => '\$e:id' => \$e |, "   -r "\$1 => (List<\$o>) Selector.queryWithBinds('\$q:quote', new Map<String, Object>{\$m}, AccessLevel.USER_MODE)"
```

- **`--let NAME = $SRC ~ PATTERN => TEMPLATE`** rewrites every match inside
  a capture and keeps the rest; `*` renders each match and joins them with
  `| SEP`. The inner pattern and template are the same language as the
  outer ones. Repeating a name adds a rule, so one value can be two
  rewrites at once.
- **`:quote` and `:id`** are the only built-ins, both plain text functions.
- **`${...}` inside `[...]`** captures a query without its brackets, so
  nesting the groups gives both the replacement target and the string.
- **Captures inside a template's string literals** are substituted.

Measured on a copy of NPSP: 1,052 queries across 201 files in 1.3s, no
refusals, and every rewritten file parses.

**Access levels, confirmed against a real org rather than assumed.** A
dynamic query may not state an access level twice -- "Cannot use the WITH
AccessLevel clause in dynamic queries that also specify an access level" --
and `WITH SECURITY_ENFORCED` is rejected outright: "no longer supported,
use WITH USER_MODE instead". So the clause is always stripped (the second
`--let` above) and the level moves into the argument, one run per case:

| Query has | `--and` / `--not` | AccessLevel |
| --- | --- | --- |
| `WITH USER_MODE` | `--and '$2 ~ *WITH USER_MODE*'` | `USER_MODE` |
| `WITH SECURITY_ENFORCED` | `--and '$2 ~ *SECURITY_ENFORCED*'` | `USER_MODE` |
| `WITH SYSTEM_MODE` | `--and '$2 ~ *WITH SYSTEM_MODE*'` | `SYSTEM_MODE` |
| no clause | `--not` each of the above | `SYSTEM_MODE` |

**Still not expressible, by design.** One match in, one span out: edits
elsewhere in the file, coordinated edits across sites, state carried
between matches (numbering, deduplication), and anything needing a
computation the two transforms do not cover.

## 6. Cast and result shape

`Selector.queryWithBinds` returns `List<SObject>`, so a rewrite needs the
cast (`(List<Account>)`) and must know the queried object -- `$o` from the
pattern's `FROM` gives that. Single-row uses (`Account a = [SELECT ...]`)
need `[0]` or a different call, so they are a separate pattern from the
list case. Worth splitting the migration into: list-assigned queries,
single-row-assigned queries, `for (X x : [SELECT ...])` loops, and queries
used as a bare argument.
