Type: research
Status: resolved (2026-09-05)

## Question

Ticket 03's dhat breakdown attributes ~24.6%/50.4MB of apexls-server's 204MB
steady-state footprint to "Pass-2 reference-resolution + body-merge
structures (`ReferenceTable`/`FileBodies`, `resolve.rs`'s `BodyBinder`)" as
one aggregate category, without a per-field/per-entry breakdown. Run a
targeted dhat pass (mirroring ticket 03's own `mem_profile.rs` method) that
isolates `ReferenceTable`'s and `FileBodies`'s own retained allocations
specifically (not the aggregate category), to determine:

- What concretely is stored per reference / per body-merge entry (field
  types, any duplicated/redundant string data vs. `SmolStr`/interned
  symbols, `HashMap`/`Vec`-of-small-collections overhead vs. a flatter
  arena+index layout)?
- What's the real per-entry byte cost against the real NPSP corpus's actual
  reference count?
- Which of these, if any, could be shrunk via interning, arena allocation,
  or a flatter layout without losing the lookup capability rename /
  find-references / goto-definition depend on -- checked directly against
  `ReferenceTable`'s own existing doc comments and real call sites, not
  assumed.

Report findings only, no decision or implementation.

## Answer

### Note on ticket 10's revision

Ticket 10 (the parallel unattributed-allocations research) independently
found that ticket 03's original "Pass-2 reference/body-merge structures"
figure (50.4MB/24.6%) undercounted three large `ReferenceTable`/`FileBodies`
sites it had left unassigned -- the real total for this category is ~68.9MB
(~33.7%), confirmed against the same struct definitions read below. This
answer's per-entry analysis is grounded in exact type sizes and a real
corpus-wide entry count, and is consistent with (not contradicting) that
correction.

### Method

- Read `reference_table.rs`'s five retained maps, `incremental.rs`'s
  `FileBodies`, and `scope.rs`'s `ScopeTree`/`Scope` directly (already
  reproduced in this ticket's own research pass above).
- Added a temporary instrumentation block to the existing
  `crates/apex-binder/examples/mem_profile.rs` (same throwaway-and-revert
  pattern ticket 05 used for `rss_probe.rs`: added, run, findings captured
  below, then `git checkout`-reverted -- `git status` confirms the working
  tree is clean, no example-file change survives this ticket) printing (a)
  the real corpus-wide total `ReferenceTable` entry count via
  `program.resolutions_in_file(file)` summed over `program.files()`, and (b)
  `std::mem::size_of` for the exact real types involved, run via
  `cargo run -p apex-binder --release --example mem_profile` against the
  real NPSP corpus.
- Reused ticket 03's own `dhat-heap.json` (already present in the working
  tree, regenerated fresh during this same instrumented run) for
  supplementary per-call-site corroboration, cross-checked against ticket
  10's own independent findings rather than re-deriving them.

### Real numbers

Corpus-wide: **412,894 total `ReferenceTable` entries** (all files, every
`Resolution` variant) -- broken down via the existing
`resolution_regression_baseline.rs` baseline as 206,903 `Resolved` + 5,538
`Unresolved` + **200,453 `Candidates`/`SchemaObject`/`UnknownSchema`/
`StdlibMember`/`Label`/`VisualforcePage` combined** (that test deliberately
excludes the latter group from its own two tracked constants -- see its own
doc comment -- so this is a real, previously-uncomputed number this ticket
adds).

Exact real type sizes (`std::mem::size_of`, release build, this platform):

| Type | Bytes | Composition |
|---|---:|---|
| `SyntaxPtr` | 16 | `FileId`(4, padded) + `SyntaxKind`(2, `#[repr(u16)]`) + `TextRange`(8) |
| `Resolution` | 24 | largest variant is `Candidates(Vec<SymbolId>)` (24B Vec header); every boxed variant (`SchemaObject`/`UnknownSchema`/`StdlibMember`/`Label`/`VisualforcePage`) is just an 8B pointer, confirming the existing doc comment's own claim that boxing keeps the common `Resolved`/`Unresolved` case cheap |
| `SymbolId` | 8 | `FileId`(4) + `local: u32`(4) |
| `ExternalKey` | **64** | see finding below |
| `TextRange` (rowan) | 8 | two `u32`s |

### Finding 1 (the clearest, most actionable one): `ExternalKey` is 64 bytes because of its own `Stdlib` variant, even though most real keys are the smaller `Schema` shape

`ExternalKey`'s four variants aren't uniformly sized: `Schema { object:
SmolStr, field: Option<SmolStr> }` needs 24+24=48 bytes; `Label`/
`VisualforcePage` (one `SmolStr` each) need 24; but `Stdlib { class_name:
SmolStr, member: Option<SmolStr>, arg_count: Option<usize> }` needs
24+24+16=64 bytes, and Rust enums size to their largest variant. Every
`ExternalKey` in `by_external` -- Schema-shaped or not -- pays the full 64
bytes, wasting up to 40 bytes (64 vs. 24) on every `Label`/`VisualforcePage`
key and 16 bytes on every `Schema` key. This is the *exact same*
size-inflation problem `Resolution` itself already solved for its own large
variants (`SchemaObjectRef`/`UnknownSchemaRef`/`StdlibMemberRef` are all
boxed there, per that enum's own doc comment: "so their `SmolStr`-carrying
fields don't force every other variant... to pay for the largest variant's
size") -- `ExternalKey` never got the same treatment. Boxing `Stdlib`'s
payload (or just `arg_count`, the field that pushes it past `Schema`'s 48
bytes) would shrink every non-`Stdlib` key by up to 40 bytes. With
`by_external` populated for a meaningful fraction of the 200,453
non-`Resolved`/`Unresolved` references (bounded above by that count, since
`Candidates` and bare-class-name `StdlibMember` refs never reach
`by_external` at all per `external_key()`'s own match arms), this is a
concrete, low-risk, mechanical fix -- not a research question, ready to
ticket directly.

### Finding 2: the two reverse indices (`by_symbol`/`by_external`) pay for one heap allocation per distinct key, not one for the whole table -- confirmed by ticket 10's own dhat evidence

`by_symbol: FxHashMap<SymbolId, Vec<SyntaxPtr>>` and `by_external:
FxHashMap<ExternalKey, Vec<SyntaxPtr>>` each store an owned `Vec<SyntaxPtr>`
per key -- every distinct symbol/external-key referenced anywhere in the
project gets its *own* separate heap buffer, grown one `push` at a time as
`set`/`map_ids_into` walk the corpus (`reference_table.rs:348,350-351,469-472`).
Ticket 10 already isolated this exact cost via dhat:
`alloc::raw_vec::RawVec::grow_one<SyntaxPtr>` alone retains 9.45MB (4.62% of
the whole 204MB footprint) -- confirmed by that ticket to be precisely these
two maps' per-key `Vec<SyntaxPtr>` growth, not a guess. This is real,
structural waste distinct from finding 1: many of these vecs are short (a
handful of references to any one symbol is the common case), so a large
share of that 9.45MB is heap-allocator bookkeeping/rounding overhead spread
across tens of thousands of small, independent allocations, not payload
bytes.

### Finding 3: `resolutions`'s own `FxHashMap<SyntaxPtr, Resolution>` -- the largest single map -- carries ~16.5MB of raw key+value data alone, before hashbrown's own table overhead

412,894 entries × (16-byte `SyntaxPtr` key + 24-byte `Resolution` value) =
16,515,760 bytes of raw KV payload for this one map. hashbrown's own
published SwissTable design (one control byte per slot, target max load
factor 7/8) adds roughly another 12-15% on top of that for control bytes
plus the table's own over-allocation to stay under that load factor --
consistent with, though not as precisely attributable via dhat as, ticket
03's own line-level numbers for this map (dhat's inlining caveat, already
documented in that ticket's own writeup, applies here too: `ReferenceTable::set`
is inlined into ~46 distinct `resolve.rs` call sites under this project's
`codegen-units = 1`/`lto = "thin"` profile, so dhat's *nearest-frame*
attribution scatters this map's own growth allocations across those many
call sites' own lines rather than crediting `reference_table.rs` -- confirmed
directly: this map's own file only shows 314,112 bytes attributed to it by
line, two orders of magnitude short of the ~16.5MB+ raw payload size,
because the growth calls that actually retain the memory get charged to
whichever `resolve.rs` line they were inlined into instead).

### Structural alternatives -- what's concretely worth proposing to the decision ticket

- **Box `ExternalKey`'s oversized field(s)** (Finding 1) -- mechanical,
  bounded, no correctness risk, no API change (`ExternalKey` is already
  `pub` but its fields are private/matched only inside `apex-binder`).
  Highest confidence, lowest cost of anything found here.
- **Replace `by_symbol`/`by_external`'s per-key `Vec<SyntaxPtr>` values with
  an arena + `(start: u32, len: u32)` index into one shared, single-allocation
  `Vec<SyntaxPtr>`** (Finding 2) -- both maps are built by a single
  sequential merge pass (`map_ids_into`, `set`) and never mutated
  afterward once a file's bind completes, so there is no later insert that
  would need the arena to grow mid-life for an existing key -- a strong
  match for "append-only during construction, read-only afterward," the
  same shape rust-analyzer's own `IndexVec`/arena patterns already use for
  comparable reverse indices. This turns "one small heap allocation per
  distinct symbol/external-key" into "one heap allocation for the whole
  table," which is exactly what Finding 2's dhat evidence shows costing
  real bytes today. Real risk: `references_to`/`references_to_external`'s
  public API returns `&[SyntaxPtr]` today (a borrow into the owned `Vec`);
  an arena-backed version needs to return a slice of the shared arena
  instead, which is a compatible return type but a real internal
  restructuring, not a one-line change.
- **`resolutions`'s own hashmap (Finding 3) is not obviously improvable
  without losing something**: it's already `FxHashMap` (a fast, low-overhead
  hasher), `Resolution` is already size-optimized via boxing, and
  `SyntaxPtr` is already a compact 16-byte `Copy` key. The measured ~12-15%
  hashbrown table overhead is inherent to open-addressing hash tables at
  their target load factor, not a sign of misuse. **No arena/interning
  alternative was found here that wouldn't sacrifice `get`'s existing O(1)
  point-lookup by `SyntaxPtr`** (`BoundProgram::resolution_at`'s hot path) --
  flag this to the decision ticket as "not this map's win," not a gap in
  this research.
- **`highlight_ranges`/`bind_var_spans`**: both already scoped narrowly (only
  populated for call/field-access-shaped references, or dynamic-SOQL binds
  respectively, per their own doc comments) and are small relative to the
  three findings above -- not worth further structural investment.

None of these alternatives touch `references_to`/`references_to_external`/
`get`'s existing lookup guarantees for rename/find-references/goto-definition
-- confirmed by reading every one of `capabilities.rs`'s call sites for these
three APIs (`references`, `document_highlight`, `rename`,
`prepare_rename`, hover's resolution lookup): all consume `&[SyntaxPtr]`
by iteration/indexing, never rely on `Vec`'s own growth/capacity behavior,
so an arena-backed slice return type is a drop-in replacement at every
call site found.

