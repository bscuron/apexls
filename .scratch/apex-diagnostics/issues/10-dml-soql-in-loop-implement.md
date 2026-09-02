Type: task
Status: open

## Question

Implement the direct (same-loop-body) DML/SOQL-in-loop diagnostic exactly as settled in [ticket 04's Answer](04-dml-soql-in-loop-diagnostic.md): a new `apexls-server` diagnostic (e.g. `bulkification_diagnostics`), `WARNING` severity, flagging unconditionally, no exemptions, whenever any of the following has a loop body (`ForStmt`/`ForEachStmt`/`WhileStmt`/`DoWhileStmt`) as an ancestor:

- A keyword-form DML statement (`InsertStmt`/`UpdateStmt`/`DeleteStmt`/`UndeleteStmt`/`UpsertStmt`/`MergeStmt`).
- A SOQL query expression (`SoqlExpr`).
- A programmatic `Database.insert`/`update`/`delete`/`upsert`/`undelete`/`merge`/`query`/`countQuery`/`getQueryLocator` call, recognized the same way `resolve.rs:3069` already recognizes `Database.query`/`countQuery`/`getQueryLocator` (a plain textual receiver-name check, no new binder capability needed).

Definition of done: wired into the merged `publish_diagnostics` alongside the five existing sources; tests covering each DML statement kind, a static SOQL query, a `Database.*` programmatic call, all four loop kinds, nested loops (still flagged), and a negative test confirming a DML/SOQL statement *outside* any loop is not flagged; a zero-false-positive sanity pass against the real NPSP corpus (matching how ticket 03's implementation grepped the whole corpus for its own trigger patterns before considering the check safe to ship).
