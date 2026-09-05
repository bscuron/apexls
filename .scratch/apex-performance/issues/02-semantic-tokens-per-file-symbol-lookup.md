# 02: Make semantic-tokens' symbol lookup O(file symbols), not O(project symbols)

**What to build:** `textDocument/semanticTokens/full` and `.../range` build their declaration-token list via `collect_tokens` (`capabilities.rs:985`), which currently gets a file's symbols by flat-mapping `SymbolTable::iter()` over *every file in the project* and filtering by `s.file == file` (`capabilities.rs:1004`). On a large real project this is an O(total project symbols) scan on every semantic-tokens request — and VS Code fires this request on scroll and on every edit, so it runs far more often than most other capabilities. `SymbolTable` already stores symbols `by_file` internally with an O(1) per-file accessor (`symbols_of_file`, `symbol_table.rs:301-303`), but that accessor is `pub(crate)` to `apex-binder` only, so `apexls-server` can't reach it — `capabilities.rs:964-966`'s own comment names this exact visibility gap as the reason the flat-iterator workaround exists.

After this ticket, semantic-tokens' declaration-collection step looks up a file's symbols directly (O(file symbols)) instead of scanning and filtering the whole project's symbol table. Expose the existing per-file accessor outward — either widen `symbols_of_file` to `pub` and add a one-line `BoundProgram` passthrough (matching the shape of `BoundProgram::resolutions_in_file`, `lib.rs:1069-1071`), or add a new `BoundProgram::symbols_in_file(file) -> &[Symbol]` wrapper — whichever fits the existing `BoundProgram` API shape better.

**Blocked by:** None (can start immediately)

**Status:** done (2026-09-05) -- `SymbolTable::symbols_of_file` widened to `pub`,
exposed as `BoundProgram::symbols_in_file(file)` (matching `resolutions_in_file`'s
shape), and `collect_tokens` (`capabilities.rs`) now uses it instead of
`program.symbols.iter().filter(|(_, s)| s.file == file)`.

- [x] `collect_tokens` looks up a file's symbols via a per-file accessor instead of iterating + filtering `SymbolTable::iter()`
- [x] The newly-exposed accessor follows the existing visibility/naming pattern of comparable per-file `BoundProgram` accessors (e.g. `resolutions_in_file`)
- [x] Both `semantic_tokens_full` and `semantic_tokens_range` use the fixed lookup
- [x] Existing semantic-tokens tests still pass and produce identical token output to before the change
