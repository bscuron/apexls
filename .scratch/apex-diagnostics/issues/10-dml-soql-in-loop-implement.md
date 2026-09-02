Type: task
Status: resolved

## Question

Implement the direct (same-loop-body) DML/SOQL-in-loop diagnostic exactly as settled in [ticket 04's Answer](04-dml-soql-in-loop-diagnostic.md): a new `apexls-server` diagnostic (e.g. `bulkification_diagnostics`), `WARNING` severity, flagging unconditionally, no exemptions, whenever any of the following has a loop body (`ForStmt`/`ForEachStmt`/`WhileStmt`/`DoWhileStmt`) as an ancestor:

- A keyword-form DML statement (`InsertStmt`/`UpdateStmt`/`DeleteStmt`/`UndeleteStmt`/`UpsertStmt`/`MergeStmt`).
- A SOQL query expression (`SoqlExpr`).
- A programmatic `Database.insert`/`update`/`delete`/`upsert`/`undelete`/`merge`/`query`/`countQuery`/`getQueryLocator` call, recognized the same way `resolve.rs:3069` already recognizes `Database.query`/`countQuery`/`getQueryLocator` (a plain textual receiver-name check, no new binder capability needed).

Definition of done: wired into the merged `publish_diagnostics` alongside the five existing sources; tests covering each DML statement kind, a static SOQL query, a `Database.*` programmatic call, all four loop kinds, nested loops (still flagged), and a negative test confirming a DML/SOQL statement *outside* any loop is not flagged; a zero-false-positive sanity pass against the real NPSP corpus (matching how ticket 03's implementation grepped the whole corpus for its own trigger patterns before considering the check safe to ship).

## Answer

Implemented as `apexls_server::capabilities::bulkification_diagnostics`, wired into the merged `publish_diagnostics` alongside the five existing sources. Purely syntax-tree-based, per the settled design: walks the raw tree for keyword-form DML statements, `SoqlExpr`/`SoslExpr` nodes, and `Database.<method>` calls (textual receiver-name check, same pattern `resolve.rs:3070` already uses for dynamic-SOQL bind resolution), and checks each candidate via a new `is_inside_loop_body` helper.

**The one real precision issue found and handled during implementation, not foreseen in the design ticket:** a naive "any ancestor is a loop" check would have flagged Apex's own canonical bulkified idiom, the SOQL-for-loop (`for (Account a : [SELECT ... FROM Account]) { ... }`) -- its query is the `ForEachStmt`'s own `iterable()`, evaluated exactly *once* before the loop starts, not per iteration. `is_inside_loop_body` distinguishes a loop's `body()` specifically from its condition/init/update/iterable positions, so only genuine per-iteration code counts. This required care to get right for the *nested* case too: a query nested inside an inner loop's own iterable, where that inner loop itself sits inside an *outer* loop's body, must still count (the outer loop re-runs it once per outer iteration -- the classic N+1 pattern) -- handled by continuing the ancestor walk past a loop's non-body position rather than stopping there.

Tests: `crates/apexls-server/tests/bulkification_diagnostics.rs` -- one test per DML statement kind's representative case, all four loop kinds, a `Database.insert`/`Database.query` call, nested loops (flagged exactly once, not once per enclosing loop), DML outside any loop (not flagged), the SOQL-for-loop idiom itself (not flagged), and the nested-inside-outer-loop case above (still flagged). All 10 pass. Zero-false-positive sanity check against real NPSP files that use the SOQL-for-loop idiom (`ACCT_AccountMerge_TDTM.cls`, `AFFL_Affiliations_TDTM.cls`, `ALLO_Allocations_TDTM.cls`, and others) via a throwaway example harness (built, run, deleted -- not committed): zero false positives across every file checked. Full `apexls-server` suite passes (one pre-existing, unrelated flaky test in `unknown_schema_diagnostics.rs`'s `didChange`-clears-diagnostic race, confirmed to reproduce intermittently even in total isolation and predating this ticket -- not a regression from this work).
