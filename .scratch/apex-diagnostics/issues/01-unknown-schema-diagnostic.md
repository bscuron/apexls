Type: task
Status: open

## Question

Wire `Resolution::UnknownSchema` (already computed during binding via `schema_index`/`resolve.rs`, whenever a SOQL/SOSL query references an object or field with no matching local or standard schema entry -- e.g. `FROM Unknown_Object__c`, a bad `WHERE` field) into a new `unknown_schema_diagnostics` source in `crates/apexls-server/src/capabilities.rs`, following the exact same shape as `unresolved_reference_diagnostics` (range from the binder's own recorded span, `ERROR` severity, `source: "apexls"`). Add it to the merged `publish_diagnostics` call in `crates/apexls-server/src/lib.rs` alongside the three existing sources. This is purely a "surface already-computed data" ticket -- no new binder analysis is needed, matching BACKLOG.md's own note that this is the cheapest candidate.

Definition of done: a protocol-level test (mirroring `crates/apexls-server/tests/*diagnostics*.rs`'s existing pattern) confirming a bad SOQL object/field produces the diagnostic and a `didChange` fix clears it; confirm no regression against the real NPSP corpus's existing `resolution_regression_baseline.rs` counts.
