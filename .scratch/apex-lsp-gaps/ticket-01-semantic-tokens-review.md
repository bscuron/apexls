Type: review
Status: open
Subject: ticket 01 ("spec: semantic-tokens ticket 01", commit `8421cd3`), implemented on branch `salvage/backend-fc11a860` (`ad56835` "feat: LSP semantic tokens (ticket 01)" + `f7a4d98` "fix: LSP semantic tokens compile fixes"), not yet merged to `master`.

## Verified

Built `salvage/backend-fc11a860` in a scratch worktree and ran it directly (not inferred from branch name/commit message):

- `cargo check -p apexls-server --tests` -- clean, no warnings.
- `cargo test -p apexls-server --test semantic_tokens` -- 13/13 pass.
- Implementation matches the spec's dedup rule (`(line, start_col)`, decl preferred over reference at the same position via insertion order + stable sort), the multi-line-token skip, the `range` filter via `contains_range`, and every `SymbolKind`/`Resolution` -> token-type mapping table in the spec's §2/§3.

## Blocking findings

1. **Hardcoded token/modifier indices, contradicting the spec's own explicit requirement.** The ticket's §1 states: "the walker must never hard-code numeric indices anywhere else -- one `const LEGEND_TYPES` / `const LEGEND_MODIFIERS` ... is the single source of truth." The shipped code violates this: `symbol_kind_to_type_idx` (`capabilities.rs`) returns bare literals (`0`..`7`), and `modifier_bits_from` plus every `Resolution` match arm use bare `1 << 2`, `1 << 4`, etc. instead of deriving from `LEGEND_TYPES`/`LEGEND_MODIFIERS`'s own positions. If either const array is ever reordered, these numbers silently drift out of sync with the advertised legend, with no compiler error -- exactly the failure mode the spec named. Fix: derive each index/bit from `LEGEND_TYPES.iter().position(...)` / the modifier array's position (const-evaluable, or asserted once at startup), not re-typed as a literal.

2. **Missing test: §"Coverage" item 12, "encoding sanity."** The spec calls for one fixture with a non-ASCII character shifting UTF-16 offsets, to confirm `delta_start`/`length` respect the negotiated `PositionEncoding` rather than assuming UTF-8 byte offsets. Not present in the 13 shipped tests. This is the one coverage gap worth requiring before merge -- unlike the untested minor `SymbolKind`/`Resolution` variants (`Trigger`, `CatchVar`/`ForEachVar`/`SwitchBindingVar`, `Label`, `VisualforcePage`, `Candidates`), which are structurally identical dispatch arms to already-tested ones and don't carry the same risk of a silent, encoding-specific off-by-N bug.

## Not blocking

Everything else: the acceptance criteria's other coverage bar, the `.sdd/` fleet bookkeeping mixed into the branch's diff vs. `master`, and the untested `SymbolKind`/`Resolution` variants above.

## Disposition

Not merged and not amended by this note -- this review is informational only; the branch belongs to a separate (fleet-managed) process, per this repo's own attestation/identity tracking under `.sdd/`, that this note does not touch.
