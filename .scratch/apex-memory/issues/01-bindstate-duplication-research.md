Type: research
Status: resolved

## Question

Does `BindState`'s concurrent residency of a full `BoundProgram` (RwLock) and a full `BindCache` (Mutex) in `crates/apexls-server/src/lib.rs:212` represent real duplicated memory — two independent copies of parse trees / source text / symbol data — or shared `Arc`-based data where holding both is inherent to a snapshot(serve) + recompute(engine) design? Trace which LSP handlers read from which structure, and check field-by-field overlap in content.

## Answer

Not real duplication. Every LSP handler (hover, completion, goto-def, etc. — ~20 call sites in `crates/apexls-server/src/lib.rs`) reads only `bind.program.read()`. `bind.cache` is touched only by the rebuild worker (`spawn_rebuild_worker`, lib.rs:614) and the filesystem watcher's `invalidate_discovery()` (lib.rs:511). This is exactly the snapshot(serve)/engine(recompute) split the code's own doc comments describe (`apex-binder/src/lib.rs:103-121`, `BindState` doc at lib.rs:176-211) — both must be resident concurrently so readers never observe a rebuild in progress.

Field-by-field, the overlap is shared, not copied:
- `BoundProgram.texts` is built (`apex-binder/src/lib.rs:822-830`) by cloning the *same* `Arc<str>` held in `cache.file_text_inputs`'s salsa `FileTextInput` (`db.rs:168-180`) — one allocation, two pointers.
- `BoundProgram.parses` is `cache.file_parses.clone()` (lib.rs:814) — `Parse` is a cheap `Arc`-based `GreenNode` clone (refcount bump), not a second parse tree.
- `BoundProgram.symbols` is `cache.table.clone()` — every field of `SymbolTable` is `Arc`-wrapped specifically so this clone is refcount bumps (`symbol_table.rs:17-30`), with a documented prior regression (438ms→700ms) from a non-`Arc` version.
- `bodies`, `schema`, `labels`, `pages` are likewise `Arc`-shared (lib.rs:831-834, 148-165).

The one asymmetry: `BindCache.file_parses` is eviction-capped (`PARSE_EVICTION_WINDOW = 8`, `incremental.rs:245,259-277`) while `BoundProgram.parses` retains whatever was resident at snapshot-assembly time (an evicted file falls back to `reparse_evicted`, lib.rs:887-917) — small per-file bookkeeping, not bulk data. A prior fix (ticket 25 in the code's own history, "no permanent shadow copy" rule, lib.rs:305-306) already removed a redundant copy of `schema`/`labels`/`pages` that once lived in `BindCache`.

**Conclusion: not a memory lever. No action needed.**
