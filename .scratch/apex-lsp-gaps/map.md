Label: wayfinder:map

## Destination

A decision on which of four code-intelligence LSP capabilities to build and how, each identified in [ticket 39](../apex-diagnostics/issues/39-rust-analyzer-parity-research.md) as the highest-value gaps against rust-analyzer specifically because each reuses data `apex-binder`'s `SymbolTable`/`ReferenceTable` already computes rather than requiring new inference: semantic tokens, quick-fixes attached to existing diagnostics, type hierarchy, and Go to Implementation.

## Notes

- Ticket 39 found apexls already advertises most of the LSP surface associated with rust-analyzer (hover, goto-definition, references, rename, document/workspace symbols, call hierarchy, inlay hints, completion, signature help, push+pull diagnostics -- `crates/apexls-server/src/lib.rs:657-765`) and merges ten diagnostic sources (`capabilities.rs:1140-1156`). These four are the specific, narrow remainder ticket 39 ranked highest, not a claim the rest of the LSP surface is missing.
- This repo already has an earlier, broader research pass over the same territory at `.scratch/apex-lsp-gaps/research.md` (protocol-mechanics gaps against the full LSP 3.17 surface, plus Apex/Salesforce-domain gaps like SOQL completion, trigger-context typing, and org/test/debug integration). Two of its top-ranked items (semantic tokens, type hierarchy) and this map's list overlap; the other two here (quick-fixes, Go to Implementation) are more specific than anything in that pass. This map does not supersede that research note -- it charts only the four items ticket 39 scoped to.
- Same zero-false-positive bar as `.scratch/apex-diagnostics/map.md`'s diagnostics apply here where relevant: a quick-fix must be a mechanical, provably-correct transformation of data the diagnostic already computed, never a heuristic guess.
- Consult `apex-grammar-oracle-sf-cli` practice (the `sf` CLI against a connected org) for any disputed real-Apex-compiler-behavior question, same as this project's established norm.
- This map's own tickets are charted **one destination-slice at a time**, not all up front -- mirrors this project's established norm (`apex-diagnostics/map.md`'s own Destination note: sequencing is "discovered from how tickets actually end up blocking each other, not asserted up front"). [Ticket 01](issues/01-semantic-tokens-implement.md) (semantic tokens) was charted first, per this map's own grilling session, as ticket 39's #1-ranked item; the items below stay unticketed until each one's turn comes.
- This map's `issues/` folder uses its own local numbering starting at `01` (`.scratch/apex-lsp-gaps/issues/01-...`), independent of `apex-diagnostics/issues/`'s counter -- this is its own destination with its own map, the same position `apex-diagnostics` was in before it had any tickets.

## Decisions so far

(none yet)

## Not yet specified

- **Quick-fix for `visibility_narrowing_diagnostics`** -- `code_action` (`lib.rs:1436-1463`) only produces a fix for `dead_code_diagnostics` (`dead_code_actions`, `capabilities.rs:2152-2198`); `visibility_narrowing_diagnostics` (`capabilities.rs:2041-2071`) surfaces a squiggle only, despite already carrying `candidate.current`/`candidate.required` and a `visibility_keyword` helper (`capabilities.rs:2107-2115`) -- a fix is a single-token text replacement. Split off from its stub-generation sibling below since the two turned out independent and differently-shaped (per this map's own grilling session, echoing ticket 35's precedent of splitting a similarly-bundled gap).
- **Quick-fix (stub generation) for `missing_implementation_diagnostics`** -- `missing_implementation_diagnostics` (`capabilities.rs:1928-1996`) already resolves each missing member's name/arity/params/return type/override-requirement; a "Generate stub" fix is mechanical text assembly, not new inference.
- **Type hierarchy** (`prepareTypeHierarchy`/`supertypes`/`subtypes`, LSP 3.17) -- not advertised; no `type_hierarchy_provider`. `SymbolTable::inherited_chain`/`direct_super`/`subtypes` (`symbol_table.rs:111,136,367-372,380-383,404-410`) are already `pub`, and `call_hierarchy.rs`'s own prepare/expand-two-directions module is a near-mechanical template to mirror -- inheritance edges, unlike call sites, are never ambiguous in this binder's model, so the one wrinkle `call_hierarchy.rs` had to handle doesn't even apply. Ranked #3.
- **Go to Implementation** -- not advertised; no `implementation_provider`. `Backend::definition` only follows `ReferenceTable`-style textual resolution, never an override/interface-satisfaction graph. That graph already exists as private, diagnostic-specific logic (`missing_implementation_diagnostics`'s `has_required_override`, `capabilities.rs:1928ff`; `visibility_narrowing`'s override/interface-satisfaction exclusion) but has never been extracted into a general-purpose queryable primitive the way `inherited_chain`/`subtypes` already are -- needs a small real extraction step first. Ranked #4.

## Out of scope

- Everything ticket 39 explicitly declined to re-recommend because `.scratch/apex-diagnostics/map.md` already has a live decision or exclusion covering it: a general per-expression stored `Ty` layer (ticket 09, Option B), `Resolution::Candidates` as its own diagnostic, stdlib-interface implementation checking (tickets 18/20), cross-method bulkification (tickets 11/12), general Salesforce security/style lints, a general per-diagnostic config/opt-out mechanism, and formatting (no de-facto-standard Apex formatter to delegate to, unlike rustfmt).
- Code lens -- ticket 39 surfaced it as a 5th, lower-priority item (pure UI sugar over `references_to`/`subtypes`, already fully reachable via existing requests) but this map's Destination was scoped to the top four; worth a future map entry if the four above ship and it's still wanted.
