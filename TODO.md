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
so `WITH USER_MODE` and `WITH SYSTEM_MODE` are stripped and the level moves
into the argument. `WITH SECURITY_ENFORCED` is different: it still works in
a dynamic query up to API v66.0 (it is rejected from v67.0, and NPSP's
classes are 53.0), and it cannot be combined with `USER_MODE`. So it is
*kept*, with `SYSTEM_MODE` passed in the call, which preserves its meaning
exactly. `docs/moxygen-migration.md` has the table and how each fact was
checked.

**Still not expressible, by design.** One match in, one span out: edits
elsewhere in the file, coordinated edits across sites, state carried
between matches (numbering, deduplication), and anything needing a
computation the two transforms do not cover.

## 6. Done: every shape a query can sit in

Thirteen recipes, one per place a query's result can go, in
`docs/moxygen-migration.md` and as a script in the NPSP checkout's
`run.sh`. Together they convert **2,132 of NPSP's 2,132 inline queries**
across 412 files, with no refusals and no parse failures, and
`apexls query 'kind:SoqlExpr'` finds nothing afterwards.

Getting from 89% to all of it took six fixes to the query surface itself
rather than more recipes -- a group could not carry a postfix, a FROM
alias could not be named apart from its object, a parenthesised subject
needed a parenthesised pattern, a glob's `*` stopped at a line end, a
`--let` dropped the last match in a multi-line capture, and a group could
not mark a type. Two of those were silently producing *wrong* rewrites,
not refusing them. All six, and what is still not expressible, are written
up in [`GAPS.md`](GAPS.md).

## 7. Done: a rewrite cannot run away with the filesystem

`--replace` with neither a project root nor explicit paths is refused.
Without a marker the walk falls back to the current directory, so one run
from a directory too high rewrites everything below it -- which ate the
test corpus twice while this was being built. Searching from anywhere is
still fine.

## Not done, and why

- **`upsert $x;`** (71 in NPSP) needs the record's type, since `doUpsert`
  always takes an external-id field: one run per type.
- **`merge $a $b;`** (17) has no Moxygen wrapper.
- **The `AggregateResult` -> `Aggregate` cascade** (128 mentions in 18
  files) is a rename, not a query rewrite. See `GAPS.md`.
