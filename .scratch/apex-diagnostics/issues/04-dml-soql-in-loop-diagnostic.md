Type: grilling
Status: resolved

## Question

Design the exact zero-false-positive rule for a DML-or-SOQL-inside-a-loop (governor-limit bulkification anti-pattern) diagnostic. "Textually inside a `for`/`while`/`do` loop body" alone is not precise enough to ship without false positives -- work through, with the user, which shapes should and shouldn't fire, e.g.:

- A loop provably guaranteed to run at most once (unusual, but does it need an exemption or is that not a real pattern worth guarding)?
- A query/DML statement inside a loop that's already querying/writing a bulk collection rather than a single record per iteration -- can this binder tell the difference, or does *any* DML/SOQL textually inside a loop body count regardless?
- Nested loops, loops inside a method called from a loop (cross-method bulkification -- almost certainly out of reach without interprocedural analysis; confirm it's out of scope for v1 rather than silently assumed).
- `Database.query`/dynamic SOQL vs. static SOQL -- same rule, or does dynamic SOQL need separate handling?

Once the precise rule is settled, this ticket's resolution should also decide whether implementation is folded into this same ticket or split into a follow-on `task`.

## Answer

**The rule (direct, same-loop-body case -- fully settled, ready to implement):** Flag unconditionally, no exemptions, whenever any of the following is found with a loop body (`ForStmt`/`ForEachStmt`/`WhileStmt`/`DoWhileStmt`) as an ancestor:
- A keyword-form DML statement (`InsertStmt`/`UpdateStmt`/`DeleteStmt`/`UndeleteStmt`/`UpsertStmt`/`MergeStmt`).
- A SOQL query expression (`SoqlExpr` -- the `[SELECT ...]` bracket-literal form, wherever it appears as an expression).
- A programmatic `Database.insert`/`update`/`delete`/`upsert`/`undelete`/`merge`/`query`/`countQuery`/`getQueryLocator` call -- recognized the same way `resolve.rs:3069` already recognizes `Database.query`/`countQuery`/`getQueryLocator` for dynamic-SOQL bind resolution (a plain textual receiver-name check, `base.eq_ignore_ascii_case("Database")` + method-name match), extended to the DML method names too.

No exemptions of any kind: not for a loop provably guaranteed to run at most once, not based on whether the DML/SOQL's own operand is a bulk collection or a single record (there's no legitimate escape hatch here -- the anti-pattern is the *statement* executing once per iteration, regardless of any one call's row count), not for nesting depth (an ancestor-chain walk catches arbitrary nesting for free). Severity: **WARNING**, not ERROR -- this is a real but *runtime* governor-limit risk (the code compiles and often runs fine below the limit), a different kind of confidence than every ERROR-severity check shipped so far (syntax errors, unknown schema, modifier conflicts), all of which are certain compile-time defects.

**Cross-method/transitive bulkification (loop calls a method whose own body -- or a method *it* calls -- contains DML/SOQL):** confirmed in scope for the eventual diagnostic (not the direct-rule case above), but this needs real call-graph machinery `apex-binder` has nowhere today (confirmed: no CFG/data-flow module exists in the crate) -- including a policy for what happens when a call in the chain goes through `Resolution::Candidates` (virtual/interface dispatch the binder already can't narrow to one concrete method). Too large a design question to spec inside this grilling session; split into its own research-then-decision pair, mirroring how the Ty-promotion question (ticket 08 -> ticket 09) was split, rather than folding it into the same ticket.

**Follow-on tickets:** [Implement the direct DML/SOQL-in-loop diagnostic](10-dml-soql-in-loop-implement.md) (`task`, ready now, unblocked) and [Research cross-method bulkification detection](11-cross-method-bulkification-research.md) (`research`, unblocked) blocking [Decide the cross-method bulkification architecture](12-cross-method-bulkification-decision.md) (`grilling`).
