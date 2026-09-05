Type: research (ticket 03 output)
Status: resolved (2026-09-05)

## Question

Ticket 03: run `cargo run -p apex-binder --release --example mem_profile` against
the real NPSP corpus, inspect the resulting `dhat-heap.json`, and write up which
specific allocation sites in `BindCache` vs `BoundProgram` account for
apexls-server's ~180-200MB steady-state resident memory, and whether that
overlap is shareable (`Arc`) or independently-owned.

## How this was run

```
cargo run -p apex-binder --release --example mem_profile
```

against the real NPSP corpus (`tests/corpus/npsp`, submodule pinned at
`3dc817c` -- 1,070 `.cls`/`.trigger` files, 1.83MB of raw source on disk).
Output:

```
curr: 204421502 bytes / 1148413 blocks   max: 269626014 bytes / 1426653 blocks
dhat: Total:     888,922,027 bytes in 8,171,063 blocks
dhat: At t-gmax: 269,626,014 bytes in 1,426,653 blocks
dhat: At t-end:  204,422,408 bytes in 1,148,413 blocks
```

`t-end` (204MB) is the number that matters here -- it's the retained,
steady-state size with both `BindCache` and `BoundProgram` still alive
(`mem_profile.rs`'s own `std::mem::forget((cache, program))`), matching what
apexls-server actually holds resident for the process's whole lifetime. It
lines up with the ~180-200MB `mem_profile.rs` doc comment already cited.

`dhat-heap.json` (41MB) was parsed with a short Python script rather than the
interactive `dh_view.html` viewer, since the question here is a specific,
answerable aggregate ("how many retained bytes does each call site account
for"), not exploratory browsing. Every `pp` (profile point) record's `eb`
field (bytes still live at t-end) was summed, grouped by the first
`apex_binder`/`apex_parser`/`apexls`-crate frame in its call stack.

## Answer

### The specific question ticket 03 asked: is the `BindCache` -> `BoundProgram` `.clone()` step duplicating anything?

`BoundProgram::from_files_cached` (`crates/apex-binder/src/lib.rs:764-793`)
assembles its returned snapshot from `cache` via five clone operations:

```rust
let files = cache.paths.clone();               // lib.rs:774
let file_ids = cache.path_ids.clone();          // lib.rs:775
let parses = cache.file_parses.clone();         // lib.rs:776
let bodies: FxHashMap<FileId, Arc<FileBodies>> = // lib.rs:777-780
    current_ids.iter().filter_map(|&file| cache.bodies.get(&file).map(|fb| (file, Arc::clone(fb)))).collect();
...
symbols: cache.table.clone(),                   // lib.rs:786
```

Their retained-byte contribution, isolated by filtering every `pp` whose call
stack passes through one of these five lines:

| Line | Expression | Retained bytes |
|------|-----------|----------------:|
| 774 | `cache.paths.clone()` | 237,428 |
| 775 | `cache.path_ids.clone()` | 237,428 |
| 776 | `cache.file_parses.clone()` | 117,100 |
| 780 | `bodies` `Arc::clone`-collect | 34,832 |
| 786 | `cache.table.clone()` | 69,664 |
| **Total** | | **661,620 (0.3% of 204MB)** |

**This settles the question: no.** `cache.file_parses.clone()` -- the literal
`HashMap<FileId, Parse>` clone this ticket's own text singled out -- retains
only 117KB for 1,070 files (~110 bytes/file), which is exactly what cloning a
`FxHashMap`'s bucket array plus each `Parse`'s two `Arc` pointers (`green`,
and the new `text: Arc<str>` from ticket 01) and a near-always-empty
`Vec<ParseError>` costs -- not a deep copy of any green tree or source text.
`cache.table.clone()` (69.6KB) and the `bodies` `Arc::clone`-collect (34.8KB)
are equally cheap, confirming `symbol_table.rs`'s own module doc comment
("`Arc`-backed... an unaffected file only costs a pointer clone") and
`incremental.rs`'s `file_parses` doc comment ("no separate copy of the
content itself is kept here") were both accurate, not aspirational.

The actual heavyweight data -- green syntax trees, `ReferenceTable`
hashmaps, per-file symbol vectors -- is allocated **exactly once**, during
the bind itself (inside `pass1_collect_dirty_files`/`pass2_merge_bodies`,
*before* `BoundProgram` is ever assembled), and every place that later needs
it (`BindCache`'s own fields, plus the `BoundProgram` snapshot handed back to
the caller) holds an `Arc` pointer into that same single allocation. `dhat`
confirms this at the allocator level: an `Arc::clone` never shows up as a
new `alloc`/`allocate` call, only a refcount bump, so if `BindCache` and
`BoundProgram` were independently materializing the same tree/table twice,
it would show up as two roughly-equal-sized allocation sites (one "original
build," one "deep clone") -- and no such pair exists anywhere in the top 30
retained-byte sites.

### Where the 204MB actually is

Grouping every retained-byte `pp` by its nearest `apex_binder`/`apex_parser`
call-stack frame instead (not just the five assembly-clone lines above):

| Category | Retained bytes | % |
|---|---:|---:|
| Rowan syntax trees (`event::build`/`GreenNode`/`NodeCache`, built once per file during parsing) | 74,845,952 | 36.6% |
| Pass-2 reference-resolution + body-merge structures (`ReferenceTable`/`FileBodies`, `resolve.rs`'s `BodyBinder`) | 50,385,264 | 24.6% |
| Per-file declared-symbol vectors (`SymbolTable`'s `Vec<Symbol>` growth/append) | 11,850,144 | 5.8% |
| Raw source text (`std::fs::read_to_string` + salsa's `FileTextInput`) | 14,762,700 | 7.2% |
| Schema/stdlib indices (`SchemaIndex`, `StdlibIndex`) | 7,124,137 | 3.5% |
| `BindCache` -> `BoundProgram` `.clone()` assembly (the 5 lines above) | 661,620 | 0.3% |
| Everything else (unattributed/misc `apex_binder`/`apex_parser` allocations below the top-30 threshold) | 44,792,591 | 21.9% |

The two largest categories -- syntax trees and Pass-2's reference/body
structures -- together account for over 60% of the footprint. Both are
necessary, single-copy data: the whole NPSP corpus parsed and fully
cross-referenced, held resident because that's what "steady-state, ready to
answer any LSP request instantly" means for a ~1,070-file project. Nothing
found here suggests `BindCache`/`BoundProgram` coexisting is what inflates
this -- the corpus itself, fully bound, is genuinely this large.

### A caveat on precision, and one honest new finding from ticket 01

`Cargo.toml`'s `[profile.release]` (`lto = "thin"`, `codegen-units = 1`) plus
this build's heavy inlining across crate boundaries means `debug =
"line-tables-only"`'s line attribution isn't perfectly precise -- two
call-stack frames (`apex_binder::impl$0::from_files_cached::closure$2` at
`lib.rs:414`, the raw `std::fs::read_to_string` read, and
`apex_parser::parse_root` at `lib.rs:152`, the `Arc::from(src)` this
session's ticket-01 change added) each show ~14.7-14.8MB retained -- nearly
identical to each other, and roughly 8x the NPSP corpus's actual on-disk
size (1.83MB). That gap is too large to be plausibly just `String`
allocator overhead; it's more likely a handful of nearby allocations getting
attributed to the same nearest source line under this profile's inlining,
not two literal ~14.7MB copies. Read the two figures as "these two sites are
of comparable size to each other," not as precise absolute numbers.

That said, the *direction* of the finding is real and worth flagging even
though it's outside ticket 03's original BindCache/BoundProgram scope: this
session's ticket-01 change (`Parse` now retains `text: Arc<str>` so
capability handlers stop reconstructing a file's text from its syntax tree
on every request) added a **new, second, independently-allocated copy** of
every file's source text, on top of salsa's own `FileTextInput.text:
String` (already needed for parsing). The two aren't `Arc`-shared with each
other -- `Parse::text` is built via `Arc::from(src)` where `src` is borrowed
from `FileTextInput`'s own `String`, which copies the bytes rather than
reusing the same allocation. On the real NPSP corpus this is a small,
worthwhile trade (the corpus is only 1.83MB of source; even a full second
copy is a rounding error against 204MB), but it's a legitimate future
micro-optimization if apexls ever targets a corpus large enough for it to
matter: store `FileTextInput.text` as `Arc<str>` instead of `String` so
`Parse::text` can be a cheap `Arc::clone` of the same allocation instead of
a fresh copy. Out of scope to do here -- ticket 01 only asked for the
`Arc<str>` retention, not a change to salsa's own input storage -- noted for
whoever next touches `db.rs`'s `FileTextInput`.

### Explicit answers to ticket 03's own checklist

- **`mem_profile` run against the real NPSP corpus, `dhat-heap.json`
  captured**: yes (41MB, this checkout's working directory).
- **Which allocation sites/data structures in `BindCache` vs `BoundProgram`
  overlap, with approximate memory attributed to each**: the only overlap is
  the five `.clone()` calls in `BoundProgram::from_files_cached`
  (`lib.rs:774,775,776,780,786`), totaling 661,620 bytes (0.3% of the 204MB
  steady-state footprint) -- not a meaningful contributor.
- **Is the overlap shareable (`Arc`) or independently-owned**: shareable,
  and already shared. Every one of those five clones is either an `Arc`
  pointer bump (`parses`, `bodies`, `symbols`) or a small `PathBuf`-keyed
  index clone (`files`, `file_ids`) -- none of them duplicates a green tree,
  a `ReferenceTable`, or a `Vec<Symbol>`'s actual backing data. There is no
  independently-owned duplicate of anything sizeable between `BindCache` and
  `BoundProgram`.

## Recommendation for ticket 04

Close ticket 04 (de-duplicate the `BindCache`/`BoundProgram` overlap) as
**not-applicable**, per its own stated bail-out clause: there is no
duplicated-but-shareable data to restructure. The `mem_profile.rs` doc
comment's framing -- "the harness exists to attribute apexls-server's
~180-200MB to a `BindCache` *and* a separately-owned `BoundProgram` snapshot"
-- turns out to name the right suspect for the wrong reason: both structures
are alive at once, but they were already sharing their expensive data via
`Arc` before this ticket, not duplicating it. The real ~204MB is simply the
cost of holding one real-world 1,070-file project's parsed trees and fully
cross-referenced symbol/reference data resident, which no `BindCache`/
`BoundProgram` restructuring changes -- any future memory-reduction work
here would need to target the two largest categories directly (shrinking
rowan's own per-node overhead, or `ReferenceTable`'s per-reference
footprint), not the cache/snapshot split.
