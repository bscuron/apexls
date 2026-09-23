# Migrating SOQL and DML to Moxygen

A tested set of `apexls query --replace` commands for converting a project
to [Moxygen](https://github.com/ZackFra/Salesforce-Moxygen). Every command
here was run against a copy of the NPSP corpus; the counts are what it
actually rewrote, and every rewritten file still parsed.

Run them from the project root, on a clean git tree — rewrites happen in
place, and `git diff` is the preview.

## Shared pieces

Every query command uses the same three `--let`s and the same call, so they
are given once here and referred to as `$LETS` and `$CALL`:

```sh
LETS_Q="--let 'q = \$2 ~ :\$e => :\$e:id' --let 'q = \$2 ~ kind:SoqlWithClause => '"
LETS_M="--let \"m = \\\$2 * :\\\$e => '\\\$e:id' => \\\$e |, \""
CALL="Selector.queryWithBinds('\$q:quote', new Map<String, Object>{\$m}, AccessLevel.SYSTEM_MODE)"
```

- `q` is the query text with every bind `:a.Id` renamed to `:aId`, and with
  any `WITH` clause removed.
- `m` is the bind map, `'aId' => a.Id`, joined with `, `.
- Both derive the key with `:id` from the same expression, so the string
  and the map agree.

`$Q` below is the pattern piece that captures a query twice — `$1` is the
whole `[...]` (what gets replaced), `$2` is the query text without its
brackets (what goes in the string):

```
Q='${[${SELECT ... FROM $o ...}]}'
```

## Access level

A dynamic query may not state an access level twice, and
`WITH SECURITY_ENFORCED` is rejected outright. Both confirmed against a
real org:

> Cannot use the WITH AccessLevel clause in dynamic queries that also
> specify an access level.

> WITH SECURITY_ENFORCED is no longer supported, use WITH USER_MODE
> instead.

So the `WITH` clause is always stripped (the second `--let`) and the level
moves into the argument. Run each case separately:

| Query has | Add | AccessLevel in `$CALL` |
| --- | --- | --- |
| `WITH USER_MODE` | `--and '$2 ~ *WITH USER_MODE*'` | `USER_MODE` |
| `WITH SECURITY_ENFORCED` | `--and '$2 ~ *SECURITY_ENFORCED*'` | `USER_MODE` |
| `WITH SYSTEM_MODE` | `--and '$2 ~ *WITH SYSTEM_MODE*'` | `SYSTEM_MODE` |
| no clause | `--not` each of the three | `SYSTEM_MODE` |

Inline SOQL runs in system mode unless it says otherwise, which is why the
no-clause case is `SYSTEM_MODE`.

## Queries, by where the result goes

Run these in order. Counts are from NPSP, which has 2,132 inline queries.

| # | Shape | Pattern and filters | Replacement | Rewrote |
| --- | --- | --- | --- | --- |
| 1 | `COUNT()` into a variable | `'$T $v = $Q;' --and '$2 ~ *COUNT()*'` | `'$1 => Selector.countQueryWithBinds(...)'` | 35 |
| 2 | list declaration | `'$T $v = $Q;' --and '$T ~ List<*>'` | `'$1 => ($T) $CALL'` | 542 |
| 3 | one-record declaration | `'$T $v = $Q;' --not '$T ~ List<*>'` | `'$1 => ($T) $CALL[0]'` | 475 |
| 4 | for-each loop | `'for ($T $v : $Q) { ... }'` | `'$1 => (List<$T>) $CALL'` | 114 |
| 5 | assignment, list variable | `'$v = $Q;' --and '$v : List<*>' --not '$o ~ * *'` | `'$1 => (List<$o>) $CALL'` | 209 |
| 6 | assignment, record variable | `'$v = $Q;' --not '$v : List<*>' --and '$v : *' --not '$o ~ * *'` | `'$1 => ($o) $CALL[0]'` | 151 |
| 7 | return, list method | `'$T $m(...) { ... return $Q; ... }' --and '$T ~ List<*>'` | `'$1 => ($T) $CALL'` | 139 |
| 8 | return, record method | `'$T $m(...) { ... return $Q; ... }' --not '$T ~ List<*>'` | `'$1 => ($T) $CALL[0]'` | 51 |
| 9 | `COUNT()` as an argument | `'$r.$f(..., $Q, ...)' --and '$2 ~ *COUNT()*'` | `'$1 => Selector.countQueryWithBinds(...)'` | 159 |
| 10 | query as an argument | `'$r.$f(..., $Q, ...)' --not '$2 ~ *COUNT()*' --not '$o ~ * *'` | `'$1 => (List<$o>) $CALL'` | 23 |

Together: **1,898 of 2,132 (89%)** across 314 files, with **no refusals and no parse
failures** afterwards.

Order matters in two places: `COUNT()` before the declaration shapes (a
count goes into an `Integer`, not a list), and the declaration shapes
before the assignment ones.

Cases 5 and 6 read the *variable's* type, so they bind the project and are
slower (about half a second extra on NPSP). Everything else stays
parse-only.

### Casting: prefer `$T`, and mind the FROM alias

`$o` is the query's whole **FROM list**, not just the object name, so a
query written `FROM AsyncApexJob a` binds `AsyncApexJob a` and a cast built
from it comes out as `(List<AsyncApexJob a>)` -- which does not parse. Two
consequences:

- Where the code states a type, cast with that (`$T`) instead of `$o`:
  cases 2, 3, 4, 7 and 8 above. It is both safer and more faithful, since
  it keeps the type the code already declared.
- Where there is no declared type (cases 5, 6, 10), exclude aliased FROM
  lists with `--not '$o ~ * *'` -- a glob for "contains a space".

NPSP has two such queries; convert those by hand. The refusal is not
dangerous either way: a rewrite that would not parse is reported and the
file is left alone, but a refusal discards *every* edit in that file, so
it is worth filtering rather than ignoring.

### What the 234 left over are

Two aliased FROM lists (above), plus queries in positions with no recipe here: inside a larger expression
(`[SELECT ...][0].Name`, `[SELECT ...].Id`), in an `if` condition or
ternary, or as part of a bigger call chain. They need either a pattern per
shape or a hand edit; the reparse check means a wrong one is refused, not
written.

Aggregate queries (`GROUP BY`, `SUM`, `MIN`) want
`Selector.aggregateQueryWithBinds`, which returns `List<Aggregate>` rather
than `List<AggregateResult>`, so the surrounding code changes too. NPSP has
15; they are left for hand conversion.

## DML

These need no `--let`: the statement's shape is the whole rewrite.

| Statement | Command | Rewrote |
| --- | --- | --- |
| `insert` | `query 'insert $x;' -r 'DML.doInsert($x, true);'` | 2,509 |
| `update` | `query 'update $x;' -r 'DML.doUpdate($x, true);'` | 590 |
| `delete` | `query 'delete $x;' -r 'DML.doDelete($x, true);'` | 119 |
| `undelete` | `query 'undelete $x;' -r 'DML.doUndelete($x, true);'` | 14 |
| `upsert` with a field | `query 'upsert $x $f;' -r 'DML.doUpsert($x, $f, true);'` | 1 |
| `Database.insert(x, b)` | `query 'Database.insert($x, $b);' -r 'DML.doInsert($x, $b);'` | 9 |
| `Database.update(x, b)` | `query 'Database.update($x, $b);' -r 'DML.doUpdate($x, $b);'` | 9 |

An access-level form (`insert as user x;`) takes the third argument:
`-r 'DML.doInsert($x, true, AccessLevel.USER_MODE);'`. NPSP has none.

### `upsert` with no external id

`doUpsert` always takes a field, so a bare `upsert x;` needs the record's
own `Id` field, which depends on its type. Select the type and name the
field to match — one run per type:

```sh
apexls query 'upsert $x;' --and '$x : Level__c' -r 'DML.doUpsert($x, Level__c.Id, true);'
```

NPSP has 71 of these across a handful of types.

### `merge`

Moxygen has no `doMerge`, so NPSP's 17 `merge` statements stay as they
are. They will not be mocked.

## Checking the result

```sh
git diff --stat                 # what changed
apexls check .                  # diagnostics, compared against before
apexls query 'kind:SoqlExpr'    # inline queries still left
```

A rewrite that would not parse is refused per file and reported, and the
command exits non-zero, so a bad pattern cannot silently corrupt a file.
