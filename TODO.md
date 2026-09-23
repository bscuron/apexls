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

## 1. Done: the rest of the DML

| Form | NPSP | Command |
| --- | --- | --- |
| `undelete $x;` | 14 | `-r 'DML.doUndelete($x, true);'` |
| `Database.insert($x, $b);` | 9 | `-r 'DML.doInsert($x, $b);'` |
| `Database.update($x, $b);` | 9 | `-r 'DML.doUpdate($x, $b);'` |
| `upsert $x;` (no field) | 71 | one run per type: `--and '$x : Level__c' -r 'DML.doUpsert($x, Level__c.Id, true);'` |
| `merge $a $b;` | 17 | Moxygen has no `doMerge`; left alone |

`doUpsert` always takes an external-id field, which is why a bare `upsert`
needs the type. Every command is in `docs/moxygen-migration.md`.

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

## 6. Done: casts and call shapes

Ten recipes, one per place a query's result can go -- list and
single-record declarations, for-each loops, assignments, returns and
arguments, plus `COUNT()` -- in `docs/moxygen-migration.md`. Together they
convert 1,896 of NPSP's 2,132 inline queries (89%) across 313 files with no
parse failures.

The 236 left are queries inside larger expressions (`[SELECT ...][0].Name`),
in conditions, or aggregates, which change the surrounding code too.
