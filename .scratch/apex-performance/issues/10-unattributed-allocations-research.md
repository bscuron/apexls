Type: research
Status: resolved (2026-09-05)

## Question

Ticket 03's dhat breakdown left ~21.9%/44.8MB of apexls-server's 204MB
steady-state footprint as "everything else (unattributed/misc
`apex_binder`/`apex_parser` allocations below the top-30 threshold)".
Re-run or re-analyze the `dhat-heap.json` from `cargo run -p apex-binder
--release --example mem_profile` against the real NPSP corpus with a lower
attribution threshold (not just the top 30 call-stack groupings) to
determine concretely what this slice actually is: is it a long tail of many
small, genuinely-distinct sites, or does a large chunk of it actually belong
to one of the already-named categories (rowan trees, `ReferenceTable`/
`FileBodies`, symbol vectors, source text, schema/stdlib indices) but got
split across multiple similar call-stack frames that each individually fell
under the top-30 cutoff?

Report a finer-grained breakdown and flag anything that looks like a real,
nameable, sizeable (>1-2% of total) category worth its own future ticket --
this ticket only investigates and reports.

## Answer

### Method

Reused the existing `dhat-heap.json` (41MB) already present in the
working tree from ticket 03's own `cargo run -p apex-binder --release
--example mem_profile` run against the real NPSP corpus (same file, same
methodology -- no need to regenerate). Reproduced ticket 03's own grouping
method exactly (sum each `pp` record's `eb` field, keyed by the nearest
`apex_binder`/`apex_parser`/`apexls`/`apex_lexer`/`apex_syntax`/
`apex_metadata`/`apex_discover` frame in its call stack), then went well
past the top-30 cutoff: all 634 distinct attributed call-stack groupings
were sorted and inspected, not just the top 30.

### The real long tail is much smaller than 44.8MB -- most of "everything else" is actually large, individually-significant sites that were never assigned to one of ticket 03's five named categories

Summing every one of the 634 distinct attributed frames (not just the top
30) accounts for 204,304,976 bytes -- 99.9% of the total 204,422,408-byte
footprint (only 117,432 bytes, 0.1%, have no `apex_*`-crate frame anywhere
in their call stack at all). Of that:

- **Top 30 frames by themselves: 191,402,820 bytes (93.6%).**
- **Genuine long tail below the top-30 cutoff: 604 distinct frames, only
  12,902,156 bytes (6.3%)** -- real, but far short of the 44.8MB/21.9%
  ticket 03 reported as "everything else."

Reconciling ticket 03's own five reported category totals (rowan trees
74.85M, Pass-2 reference/body-merge 50.39M, symbol vectors 11.85M, source
text 14.76M, schema/stdlib 7.12M -- summing to 159.63M) against my
independently-reproduced top-30 list (191.40M) leaves a **20,023,248-byte
(9.8%) gap **inside the top 30 itself**: large, individually-ranked
allocation sites that simply were never assigned to any of the five named
categories in ticket 03's writeup, not because they're small or diffuse,
but because they were left out of the manual categorization pass.

### The three biggest "unassigned top-30" sites are, in fact, definitively `ReferenceTable`/`FileBodies` -- confirmed by reading the struct definitions directly, not guessed from names

Checked directly against `crates/apex-binder/src/reference_table.rs` and
`crates/apex-binder/src/lib.rs`:

| Site | Bytes | % | Confirmed source |
|---|---:|---:|---|
| `alloc::raw_vec::RawVec::grow_one<SyntaxPtr>` | 9,445,952 | 4.62% | `reference_table.rs:309,316` -- `by_symbol: FxHashMap<SymbolId, Vec<SyntaxPtr>>` and `by_external: FxHashMap<ExternalKey, Vec<SyntaxPtr>>`; this is their per-key `Vec<SyntaxPtr>` value-vecs growing |
| `hashbrown::raw::RawTable::reserve_rehash<(SyntaxPtr, TextRange)>` | 5,460,188 | 2.67% | `reference_table.rs:329` -- `highlight_ranges: FxHashMap<SyntaxPtr, TextRange>` |
| `apex_binder::BoundProgram::from_files_cached<&PathBuf>` (`lib.rs:744`) | 3,641,588 | 1.78% | `lib.rs:744` -- literally `FxHashMap::with_capacity_and_hasher(bodies.len(), ...)` allocating `FileBodies.scopes`, `lib.rs:742-744` |

These three alone total **18,547,728 bytes (9.1%)**, all unambiguously
`ReferenceTable`/`FileBodies` fields per their own declarations -- ticket
03's own "Pass-2 reference-resolution + body-merge structures" category
therefore undercounts its true size. **The real total for that category is
closer to ~68.9M (~33.7%), not the reported 50.4M (24.6%)** -- a materially
bigger share of the footprint than the map's own Destination/Notes
currently states, and now the single largest category after rowan trees,
closing most of the gap between it and rowan's ~36.6%.

The two remaining unassigned top-30 sites are small and are genuinely
different, narrower sites, not mis-filed into "everything else" by
category -- verified by reading their call sites:

- `apex_binder::collect::type_ptr_and_name::closure$0` (`collect.rs:119`,
  879,744 bytes / 0.43%) -- Pass-1 collect-phase `Vec<SmolStr>`
  type-argument-text extraction (`ty.type_args().map(...).collect()`), not
  part of any of the five named categories.
- `alloc::raw_vec::RawVec::grow_one<Option<Ty>>` (595,776 bytes / 0.29%) --
  small `Ty`-tracking `Vec` growth, likely Pass-2-adjacent but not folded
  into the reference-table figure above since it isn't a
  `ReferenceTable`/`FileBodies` field itself.

### The genuine long tail (below top-30, 604 sites, 12.9M/6.3%) is exactly what it looks like: ordinary per-file binding bookkeeping, no hidden category

Inspected the next ~100 entries beyond the top 30 individually (full list
generated, not just sampled). Nothing resembles a new, nameable,
sizeable (>1-2%) category -- the largest single entry beyond the top 30 is
578,880 bytes (0.28%, a `SymbolTable::rebuild_indices` call site), and every
entry after that drops off further. The tail is a wide, shallow spread
across ordinary per-expression/per-file binding sites already implied by
the five named categories' own mechanics: `BodyBinder::bind_field_expr`/
`bind_name_expr`/`bind_sobject_field_init` (many call sites, `resolve.rs`,
each a few hundred KB or less), `soql::resolve_field_path`,
`apex_syntax::ast::Type::text`, `LabelIndex::from_labels`,
`resolve_type_ref_base`'s other call sites, `apex_metadata::xml`
label/custom-metadata parsing, and `apex_discover`'s file-walk `.clone()`
sites. None of these individually or as a group suggests an
undiscovered structural category worth its own ticket -- they're the
long, ordinary tail of walking and binding 1,070 real files.

### Bottom line

- **The 44.8MB "everything else" figure significantly overstates the
  genuine unattributed tail.** The real below-top-30 tail is only
  ~12.9MB (6.3%).
- **~18.5MB (9.1%) of the reported "everything else" is actually
  `ReferenceTable`/`FileBodies` data** (confirmed against the struct
  definitions directly), meaning that category's true size is ~68.9M
  (~33.7%), not the previously-reported 50.4M (24.6%).
- **No new, sizeable (>1-2%), nameable category was found.** Nothing
  in the long tail or the remaining unassigned top-30 stragglers (~1.5M
  combined) looks like a hidden structural opportunity distinct from the
  categories the map already knows about.
- **Recommendation:** don't open a new ticket for "the unattributed 22%" --
  there isn't a distinct thing there to design against. Instead, whoever
  works [ticket 08](08-reference-table-footprint-research.md)/[ticket
  09](09-reference-table-footprint-decision.md) should use ~68.9M (~33.7%),
  not ~50.4M (~24.6%), as `ReferenceTable`/`FileBodies`'s real target size --
  a meaningfully bigger opportunity, and the map's own Notes section should
  be corrected to reflect this revised split between rowan trees (~36.6%,
  unchanged) and the `ReferenceTable`/`FileBodies` family (~33.7%, revised
  up from ~24.6%).
