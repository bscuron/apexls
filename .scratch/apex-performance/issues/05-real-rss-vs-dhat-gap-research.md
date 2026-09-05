Type: research
Status: resolved (2026-09-05)

## Question

After tickets 01-04 landed, the user reported apexls-server's real-world
memory usage at ~400MB -- much higher than the ~204MB `dhat` measured in
ticket 03's breakdown -- and asked two things: is this caused by ticket 01
(`Parse` now retains each file's source text as `Arc<str>`), and would
storing source text via `mmap` instead keep memory usage down?

## Answer

### Method

`dhat` (used for ticket 03) only instruments logical `alloc`/`dealloc`
calls -- it can't see what a real allocator does underneath (arena/segment
reservation, size-class rounding, fragmentation, freed-but-not-yet-returned
pages). To get a number comparable to what the user actually observes, this
measured the *real* `apexls` binary (release profile, `mimalloc` global
allocator -- the same one `apexls-server` ships with) instead of the `dhat`
harness.

A throwaway example (`crates/apexls/examples/rss_probe.rs`, deleted after
use, never committed) called `apex_binder::BoundProgram::from_files` against
the real NPSP corpus, printed its PID, then slept 60s so the process's real
Windows memory could be sampled externally via PowerShell
`Get-Process -Id <pid> | select WorkingSet64,PrivateMemorySize64`.
Deliberately used `BoundProgram::from_files` (no `BindCache` at all, unlike
`apexls-server`'s own `BindState`) so the measurement isolates "one bound
project's real memory," with the `BindCache`/`BoundProgram` double-storage
question ticket 03/04 already closed as not-applicable removed from the
picture entirely.

This was run twice, at two commits, to isolate ticket 01's actual
contribution:
- **Before** (`a80fad5`, the commit immediately preceding this session's
  tickets 01-04): a `git worktree add` checkout, with the same throwaway
  example added there too (pointing at the main checkout's NPSP corpus
  directory, since worktrees don't carry submodule content).
- **After** (current `HEAD`, ticket 01 landed): the main checkout.

Both built via `cargo build -p apexls --release --example rss_probe`
(same `[profile.release]`: `lto = "thin"`, `codegen-units = 1`).

### Results

| | Working Set (RSS) | Private/committed bytes |
|---|---:|---:|
| Before ticket 01 (`a80fad5`) | 345.8 MB (362,545,152 B) | 549.3 MB (576,045,056 B) |
| After ticket 01 (current `HEAD`) | 355.8 MB (373,075,968 B) | 577.7 MB (605,659,136 B) |
| **Difference** | **+10.0 MB (~2.9%)** | **+28.4 MB (~5.2%)** |

**Ticket 01 is not the cause of the ~400MB the user is seeing.** It added
roughly 10-30MB (2-5%), not the ~150-200MB gap between `dhat`'s 204MB and
the user's observed ~400MB. That gap was already present at the commit
*before* any of this session's tickets landed -- binding the real NPSP
corpus with the real production binary (mimalloc, no `BindCache` in the
picture at all) already cost ~346-350MB of working set / ~550MB of
committed memory, before ticket 01 ever touched `Parse`.

### Where the ~150-200MB gap between `dhat` and reality actually comes from

`dhat`'s ~204MB (ticket 03) counts bytes requested via `alloc`/`dealloc`.
The real, `mimalloc`-backed binary shows ~350-580MB for the *same*
workload (parsing + fully cross-referencing the same 1,070-file corpus),
with **zero `BindCache`** in either measurement. Since the gap exists with
or without `BindCache`, it cannot be a `BindCache`/`BoundProgram`
duplication issue (ticket 04 already closed that door on separate grounds).
It's the ordinary, expected difference between "logical bytes an
allocator was asked for" and "real resident memory a production allocator
actually holds": `mimalloc` reserves memory in per-thread segments/arenas,
rounds allocations up to size classes, and -- like essentially every
modern general-purpose allocator -- doesn't eagerly return freed pages to
the OS. None of this is visible to `dhat`, which only sees the logical
`alloc`/`dealloc` call pattern, not the allocator's own internal
bookkeeping or its retained-but-freed capacity.

### Why `mmap`-backed source text would not meaningfully help

Per ticket 03's breakdown, the two largest categories of *logical* memory
are rowan syntax trees (~37%) and Pass-2 reference/body-merge structures
(~25%) -- both *derived from* parsing, not raw source bytes. Raw source
text (salsa's `FileTextInput::text` plus ticket 01's `Parse::text`)
accounts for a much smaller slice of the total even combined. `mmap`ing
source files would, at best, avoid one copy of that smaller slice -- it
does nothing for the syntax tree or reference-table memory that dominates,
and does nothing for the `mimalloc`-vs-`dhat` gap identified above (that
gap comes from the allocator, not from how source text is stored).

`mmap` also stops being usable the moment a file is edited: an open
buffer's in-memory content no longer matches its on-disk `mmap`ed bytes,
so every dirty/open file would still need a `String`/`Arc<str>` fallback
anyway, on top of whatever bookkeeping keeps ~1,000+ file mappings/handles
open and invalidated correctly on Windows. That's real added complexity in
exchange for shrinking a small slice of a problem that isn't where the
memory actually is.

**Conclusion: don't pursue `mmap`.** It targets the wrong layer.

### What would actually move the ~400MB number

Two real levers, not pursued as part of this research pass (out of scope --
this ticket is measurement/diagnosis only, matching ticket 03's own
precedent):
1. **`mimalloc` purge/decommit tuning** (e.g. `MIMALLOC_PURGE_DELAY` or
   equivalent `mi_option` settings) to make it return freed pages to the OS
   sooner. Cheap, config-only, no architecture risk -- worth trying first
   and re-measuring with the same `rss_probe` method before touching
   anything else.
2. **Shrinking rowan's per-node overhead or `ReferenceTable`'s per-entry
   footprint** -- a real architecture change targeting the two categories
   that actually dominate the logical byte count (ticket 03's breakdown),
   not attempted here.

## Explicit answers to the two questions asked

- **Is ticket 01 (caching each file's source text) why real memory is
  ~400MB?** No -- measured directly, it added ~10-30MB (2-5%) over the
  pre-ticket-01 baseline, which itself was already ~346-350MB/~550MB
  (working set/committed) for the same corpus via the same real binary.
- **Should source text be stored via `mmap` instead, to keep memory
  down?** No -- the dominant memory (syntax trees, reference tables) isn't
  raw source text at all, so `mmap` wouldn't touch it, and it introduces
  real complexity (edited-file invalidation, many-open-file-handle
  bookkeeping on Windows) for a small potential saving. `mimalloc`
  purge/decommit tuning is the cheaper, more targeted next experiment.
