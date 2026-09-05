Type: decision
Status: resolved (2026-09-05)
Blocked by: 08

## Question

Given [ticket 08](08-reference-table-footprint-research.md)'s findings on
`ReferenceTable`/`FileBodies`'s real per-entry memory shape, decide the
concrete architecture change (if any) to shrink it -- interning,
arena/flat layout, or another specific restructuring -- while preserving
every existing lookup (rename, find-references, goto-definition)
`ReferenceTable`'s current API guarantees. Lock the exact structural change,
sized for a follow-on implement ticket, accounting for `cargo bench -p
apex-binder`'s existing gates as a hard constraint.

## Answer

Locked both of ticket 08's findings, but with a simpler mechanism than that
ticket's own "arena + `(start,len)` index" framing for Finding 2:

1. **Box `ExternalKey::Stdlib`'s payload** (Finding 1) exactly as proposed:
   a new `StdlibKey { class_name: SmolStr, member: Option<SmolStr>,
   arg_count: Option<usize> }` struct, with `ExternalKey::Stdlib(Box<StdlibKey>)`
   replacing the inline struct variant. Shrinks every `Schema`/`Label`/
   `VisualforcePage` key by up to 40 bytes (64 -> as low as 24). Single
   construction site (`Resolution::external_key`'s `StdlibMember` arm,
   `reference_table.rs:279`) -- confirmed by grep to be the *only* place
   `ExternalKey::Stdlib` is constructed anywhere in the workspace, and
   nothing outside `reference_table.rs` pattern-matches it, so this is a
   fully self-contained change.

2. **For Finding 2 (`by_symbol`/`by_external`'s per-key `Vec<SyntaxPtr>`),
   use `SmallVec<[SyntaxPtr; 1]>` instead of an arena+index.** Rejected the
   arena approach ticket 08 raised: it requires either restructuring `set`/
   `map_ids_into`'s incremental per-reference construction into a two-phase
   build-then-finalize model, or maintaining a dual build-time/finalized
   representation -- real, correctness-sensitive complexity for a
   proportionally small (9.45MB/~4.6% of the 204MB footprint) win.
   `smallvec` (1.15.2) is already resolved in the workspace's own
   `Cargo.lock` (a transitive dependency of existing crates) -- adding it as
   a direct `apex-binder` dependency pulls in zero new supply-chain surface
   and matches the version already compiled. `SmallVec<[SyntaxPtr; 1]>`
   supports `.push()`/`Default` identically to `Vec`, so `set`/
   `set_with_highlight`/`map_ids_into`'s existing `.entry(id).or_default().push(reference)`
   call sites need no changes at all; it derefs to `&[SyntaxPtr]`, so
   `references_to`/`references_to_external`'s public return type is
   unchanged too -- a type-only swap, zero call-site churn anywhere. Inline
   capacity of 1 (not 2+) because ticket 08's own research states "a
   handful of references to any one symbol is the common case" -- most keys
   avoid heap allocation entirely; any key needing more than one reference
   falls back to a heap `Vec` exactly as today, no regression for that tail.

Both changes are additive and independent -- implemented together in one
ticket since neither risks the other, but each individually verifiable via
`cargo test -p apex-binder` and a before/after `mem_profile`/`cargo bench -p
apex-binder` run. See [ticket 12](12-reference-table-shrink-implement.md).
