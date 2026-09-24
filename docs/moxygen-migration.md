# Migrating SOQL and DML to Moxygen

A tested set of `apexls query --replace` commands for converting a project
to [Moxygen](https://github.com/ZackFra/Salesforce-Moxygen). Every command
here was run against a copy of the NPSP corpus; the counts are what it
actually rewrote, and every rewritten file still parsed.

It converts **every** inline query: 2,132 of 2,132, with nothing left for
`apexls query 'kind:SoqlExpr'` to find. What it took to get there, and
what still does not fit, is in [`GAPS.md`](../GAPS.md).

Run them from the project root, on a clean git tree — rewrites happen in
place, and `git diff` is the preview.

## Shared pieces

Every query command uses the same `--let`s and the same call, so they
are given once here and referred to as `$LETS` and `$CALL`:

```sh
LETS_Q="--let 'q = \$2 ~ :\$e => :\$e:id' --let 'q = \$2 ~ kind:SoqlWithClause => '"
LETS_M="--let \"m = \\\$2 * :\\\$e => '\\\$e:id' => \\\$e |, \""
CALL="Selector.queryWithBinds('\$q:quote', new Map<String, Object>{\$m}, System.AccessLevel.SYSTEM_MODE)"
```

- `q` is the query text with every bind `:a.Id` renamed to `:aId`, and with
  any `WITH` clause removed.
- `m` is the bind map, `'aId' => a.Id`, joined with `, `.
- Both derive the key with `:id` from the same expression, so the string
  and the map agree.
- The recipes that match a query with *no shape around it* -- the `COUNT()`
  and aggregate catch-alls -- bind it to `$1` rather than `$2`, so they
  take the same two `--let`s spelled over `$1`.

`$Q` below is the pattern piece that captures a query twice — `$1` is the
whole `[...]` (what gets replaced), `$2` is the query text without its
brackets (what goes in the string):

```
Q='${[${SELECT ... FROM $o ...}]}'
```

## Access level

What a dynamic query may say about security, confirmed against a real org
at several API versions (anonymous Apex runs at the org's *latest*, which
is what made the first probe misleading):

- **`WITH SECURITY_ENFORCED` works in a dynamic query up to API v66.0** and
  is rejected from **v67.0**: "no longer supported, use WITH USER_MODE
  instead". A class runs at its own `apiVersion`, so what matters is the
  class being rewritten -- NPSP's are 53.0.
- **It cannot be combined with `USER_MODE`**: that fails with "Cannot use
  the WITH SECURITY_ENFORCED clause in queries using USER_MODE access
  level". With `SYSTEM_MODE` it is fine, which is what the recipes use.
- **`WITH USER_MODE` / `WITH SYSTEM_MODE` cannot be combined with one
  either**: "Cannot use the WITH AccessLevel clause in dynamic queries that
  also specify an access level". Those two are stripped, and the level moves
  into the argument.

So `SECURITY_ENFORCED` is *kept*, which preserves its meaning exactly:
field and object permissions enforced, sharing left to the class's own
`with`/`without sharing`. Swapping it for
`USER_MODE` would additionally apply sharing and can return fewer rows.

| Query has | Clause | Access level in the call |
| --- | --- | --- |
| `WITH SECURITY_ENFORCED` | **keep** | `SYSTEM_MODE`, or omit it entirely with `Selector.query(...)` |
| `WITH USER_MODE` | strip | `USER_MODE` |
| `WITH SYSTEM_MODE` | strip | `SYSTEM_MODE` |
| no clause | -- | `SYSTEM_MODE` |

Inline SOQL runs in system mode unless it says otherwise, which is why the
no-clause case is `SYSTEM_MODE`.

`SECURITY_ENFORCED` with binds works too -- `queryWithBinds(q, binds,
AccessLevel.SYSTEM_MODE)` with the clause in the string was confirmed by
deploying a class at API 66 to a real org and calling it, because anonymous
Apex refuses `SYSTEM_MODE` and cannot test it.

### Keeping the clause

Run these before the recipes below, and exclude their queries from the rest
with `--not '$2 ~ *SECURITY_ENFORCED*'`. They are the ordinary recipes
minus the clause-stripping `--let`:

```sh
apexls query '$T $v = $Q;' --and '$2 ~ *SECURITY_ENFORCED*' \
  --let 'q = $2 ~ :$e => :$e:id' \
  --let "m = \$2 * :\$e => '\$e:id' => \$e |, " \
  -r "\$1 => (\$T) Selector.queryWithBinds('\$q:quote', new Map<String, Object>{\$m}, System.AccessLevel.SYSTEM_MODE)"
```

## Queries, by where the result goes

The whole set is a script: [`run.sh`](../../NPSP/run.sh) in the NPSP
checkout, which is what these counts come from. Run it from the project
root on a clean tree.

Order matters, and only in two places: `COUNT()` runs first, and
aggregates run before the record shapes. Everything after that is
independent.

| # | Shape | Pattern | Cast from | NPSP |
| --- | --- | --- | --- | --- |
| 1 | `COUNT()`, anywhere | `[${SELECT ...}]` + `--and '$1 ~ *COUNT()*'` | -- (an Integer) | 236 |
| 2 | aggregate declaration | `$T $v = $Q;` + the aggregate filter | `List<Aggregate>` | 6 |
| 3 | aggregate loop | `for ($T $v : $Q) $b` | `Aggregate` | 7 |
| 4 | aggregate, anywhere else | `[${SELECT ...}]` + the aggregate filter | -- | (in 2/3) |
| 5 | declaration | `$T $v = $Q;` | `$T` | 1,002 |
| 6 | for-each | `for ($T $v : $Q) ...;` | `List<$T>` | 108 |
| 7 | property getter | `$T $p { get { ... return $Q; ... } }` | `$T` | 1 |
| 8 | return | `$T $m(...) { ... return $Q; ... }` | `$T` | 185 |
| 9 | assignment | `$v = $Q;` + `'$v : List<*>'` | `$o` | 347 |
| 10 | argument | `$r.$f(..., $Q, ...)`, `$f(..., $Q, ...)` | `$o` | 37 |
| 11 | constructor argument | `new $T($Q)` | `$o` | 124 |
| 12 | postfix | `$Q.$f`, `$Q.$f(...)`, `$Q[$i]` | `$o`, parenthesised | 78 |
| 13 | aliased FROM | the same shapes over `FROM $o $a` | `$o` | 1 |

**2,132 of 2,132**, across 412 files, with no refusals and no parse
failures. `apexls query 'kind:SoqlExpr'` finds nothing afterwards.

### Where the cast comes from

Two sources, and the choice is not cosmetic.

- **The declared type (`$T`)** wherever the code states one: declarations,
  loops, properties, returns. It is more faithful -- it keeps the type the
  code already had -- and it sidesteps the FROM alias entirely.
- **The FROM object (`$o`)** where there is no declared type: assignments,
  arguments, constructors, postfix. `$o` binds the whole FROM *list*, so
  `FROM Account a` would give `(List<Account a>)`; those recipes exclude
  aliased lists with `--not '$o ~ * *'`, and a separate recipe writes
  `FROM $o $a`, which binds the object and the alias apart.

A postfix recipe parenthesises its cast -- `((List<X>) call)[0].Name` --
so the postfix applies to the call's result and not to the cast's.

### Two things that will bite

- **Qualify types in a replacement.** A replacement is text, not a
  resolved reference: `AccessLevel.SYSTEM_MODE` binds to a class's own
  `enum AccessLevel` if it declares one, and NPSP has exactly one such
  class. The recipes write `System.AccessLevel.SYSTEM_MODE`.
- **An aggregate changes the element type.** `aggregateQueryWithBinds`
  returns `List<Aggregate>`, not `List<AggregateResult>`, so anything
  carrying one along -- a parameter, a field, a return type -- follows.
  The recipes change the declaration the query sits in; the rest is a
  rename, and `GAPS.md` says why the tool will not do it.

## DML

These need no `--let`: the statement's shape is the whole rewrite. They run
**first**, before the query recipes: a query used as a DML operand
(`delete [SELECT ...];`) becomes an ordinary call argument once the
statement is wrapped, and the argument recipe then converts it.

| Statement | Command | Rewrote |
| --- | --- | --- |
| `insert` | `query 'insert $x;' -r 'DML.doInsert($x, true);'` | 2,506 |
| `update` | `query 'update $x;' -r 'DML.doUpdate($x, true);'` | 590 |
| `delete` | `query 'delete $x;' -r 'DML.doDelete($x, true);'` | 119 |
| `undelete` | `query 'undelete $x;' -r 'DML.doUndelete($x, true);'` | 14 |
| `upsert` with a field | `query 'upsert $x $f;' -r 'DML.doUpsert($x, $f, true);'` | 1 |
| `Database.insert(x, b)` | `query 'Database.insert($x, $b);' -r 'DML.doInsert($x, $b);'` | 9 |
| `Database.update(x, b)` | `query 'Database.update($x, $b);' -r 'DML.doUpdate($x, $b);'` | 9 |

An access-level form (`insert as user x;`) takes the third argument:
`-r 'DML.doInsert($x, true, System.AccessLevel.USER_MODE);'`. NPSP has
none.

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
