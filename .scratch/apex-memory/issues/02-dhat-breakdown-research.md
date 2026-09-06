Type: research
Status: resolved

## Question

Generate a fresh dhat heap-profile breakdown of `BoundProgram::from_files_cached` over the NPSP corpus (current mainline HEAD, independent of the prior memory-reduction effort's data) to find peak/retained bytes by allocation site, determine whether binding is eager or on-demand at startup, and estimate the stdlib reference data's in-memory footprint.

## Answer

Ran `crates/apex-binder/examples/mem_profile.rs` fresh (`cargo run --release -p apex-binder --example mem_profile`) against `tests/corpus/npsp` (submodule, commit `3dc817c`).

**Headline numbers:**
- Peak (t-gmax): 248,787,326 bytes / 1,339,004 blocks
- Retained at end (`BindCache` + `BoundProgram` both alive): 188,064,048 bytes / 1,121,928 blocks
- Total ever allocated: 887,558,178 bytes / 8,175,343 blocks

**Top allocation sites by peak-live bytes:**

| Site | Peak-live bytes | % of peak |
|---|---|---|
| `rowan::arc::ThinArc::from_header_and_iter` (green-tree nodes) | 57.1M | 22.9% |
| `apex_binder::reference_table::ReferenceTable::set` | ~28.7M | ~11.5% |
| rayon `join_context` closures over `Symbol`/`resolve` tuples (parallel Pass 2 collect) | ~29M | ~11.6% |
| `BoundProgram::from_files_cached` itself (incl. `body.refs.map_ids_into`) | ~19.9M | ~8% |
| `from_files_cached::closure$2` (`CandidateFile` construction, discovery) | 14.8M | 5.9% |
| `ScopeTree::push` / `FileCollection::push` | ~11.5M | ~4.7% |
| `BodyBinder::declare_local_raw` | 6.0M | 2.4% |

At end-of-run (retained shape), the two biggest are rowan `ThinArc` green trees (57.1M, 30.3% of retained) and the `from_files_cached` reference/scope remap step (41.5M, 22.0% of retained).

**BoundProgram vs BindCache duplication:** none for the big items (see ticket 01) — `BindCache` carries its own bookkeeping (`freshness`, `file_parses`, `parse_last_used`, discovery/salsa inputs) not mirrored in `BoundProgram`, but these are small per-file metadata maps, not bulk data.

**Eager or on-demand binding:** Eager, full-corpus, at startup. `initialized` (lib.rs:974-993) immediately calls `schedule_rebuild()` against the whole workspace root, not just open documents. `apex_discover::discover(root)` walks and classifies every `.cls`/`.trigger` file. On a cold cache every discovered file is `dirty`, so declaration collection *and* full Pass 2 body/reference binding run project-wide, for every file, whether or not it's open. Declaration collection is genuinely load-bearing project-wide (cross-file `extends`/`implements`/member resolution needs the whole declaration set before Pass 1.5/Pass 2). Full Pass 2 (body/reference binding — the expensive stage, and the source of most of the top allocation sites above) is not obviously load-bearing project-wide: `files_to_rebind` (lib.rs:639-643) already distinguishes "declaration changed → rebind everything" from "just dirty files," i.e. the machinery to scope Pass 2 narrower than "all declared files" already exists in shape.

**Stdlib in-memory estimate:** `apex_reference.json` (3.8M on disk) and `standard_objects.json` (11M on disk) parse into `Vec<SObjectSchema>`/`Vec<FieldSchema>` using `SmolStr` fields (inlines ≤23 bytes, no heap alloc for typical short names). No existing size-measurement helper in the crate. Reasoned estimate: roughly 1.5-3x on-disk size once deserialized, i.e. ~6-12MB each, ~15-25MB combined — modest relative to the ~250MB peak, and smaller than a naive 3-5x String-overhead guess would suggest.

**Conclusion:** the dominant, addressable cost is eager project-wide Pass 2 binding (rowan trees, reference tables, scope trees, symbol/resolve tuples, ID remapping) — not stdlib data, and not BindState duplication. Scoping Pass 2 to a working set (open files + dependency closure), leaving the rest declaration-only, is the top-priority lever for the next decision ticket.
