# 01: Stop rebuilding file text from the syntax tree on every LSP request

**What to build:** Every LSP capability request (hover, completion, goto-definition, references, rename, folding-range, selection-range, document-highlight, signature-help, inlay-hints, semantic-tokens, etc.) currently answers by re-deriving the file's full plain-text source from its syntax tree via `program.syntax(file).text().to_string()` — a fresh O(file-size) tree-walk-and-allocate on every single call, at ~30 call sites in `capabilities.rs`/`lib.rs`. For an open file, that text is already sitting for free in `BindState::documents.texts`. After this ticket, a request against a currently-open file reuses that already-resident string (or an equally cheap cached source) instead of re-walking the tree; a request against a closed-but-referenced file still falls back to a tree walk, since no cheaper option exists there yet.

Two fix shapes are acceptable, pick whichever is the smaller diff once you're in the code:
- (a) Thread `documents.texts.get(uri)` into the handlers that already have access to `self.bind.documents`, falling back to the existing tree-walk for files that aren't open.
- (b) Have `apex_binder::Parse`/`BoundProgram` retain the original source `Arc<str>` alongside its `GreenNode` at parse time, so every `program.syntax(file)` caller gets a cheap reference fetch with zero call-site churn in `capabilities.rs`. This is the more structurally honest fix — it matches what `line_index.rs`'s own doc comment already assumed was true ("callers already have it") — so prefer it unless it turns out to be a bigger `apex-binder`/`apex-parser` change than expected.

Don't change `LineIndex::new`'s own per-call recomputation — its doc comment already argues that specific cost is fine and it's out of scope here; this ticket is only about the text-recovery step feeding it.

**Blocked by:** None (can start immediately)

**Status:** done (2026-09-05) -- implemented via fix shape (b): `Parse` now retains
`text: Arc<str>` alongside its `GreenNode` (`apex-parser/src/errors.rs`), populated
once at parse time (`apex-parser/src/lib.rs`'s `parse_root`/`parse_with`).
`BoundProgram::source_text(file)` (`apex-binder/src/lib.rs`) exposes it as a cheap
`&str` reference fetch. Every whole-file-text call site in `capabilities.rs`/`lib.rs`
now uses it instead of `program.syntax(file).text().to_string()`.

- [x] A request against a currently-open file no longer triggers a `SyntaxNode::text().to_string()` tree walk to recover that file's source
- [x] A request against a closed-but-referenced file still works correctly (falls back to deriving text from the tree) -- no separate fallback needed: `Parse::text` is populated for every parsed file, open or not, so both cases are the same cheap path
- [x] Existing LSP capability tests (hover, completion, rename, semantic-tokens, etc.) still pass
- [x] No new `.clone()` of the full `BoundProgram` or a whole-file `SyntaxNode` is introduced as part of the fix
