Type: task
Status: open

## Question

Implement `textDocument/semanticTokens/full` and `textDocument/semanticTokens/range` for apexls, per [`research.md`](../research.md)'s synthesis §1: a semantic-highlighting layer keyed off the binder's already-computed `SymbolTable`/`ReferenceTable` (not a TextMate grammar), advertised via `SemanticTokensServerCapabilities` in `initialize`. No new binder work; the token producer is a walk over data that already exists on the in-memory `BoundProgram`.

Follow the exact shape existing per-capability functions use in `capabilities.rs`, and the exact snapshot + `wait_for_rebuild` + `bind.program` handler pattern every other request handler in `lib.rs` uses (closest reference: `Backend::document_symbol` at `crates/apexls-server/src/lib.rs:1269`).

### 1. `SemanticTokensLegend`

Declared once in `initialize` (see §5) and reused by every emitted token. Type-index and modifier-bit assignments are the positions in the arrays below; the walker must never hard-code numeric indices anywhere else -- one `const LEGEND_TYPES: &[SemanticTokenType]` / `const LEGEND_MODIFIERS: &[SemanticTokenModifier]` at the top of `capabilities.rs`'s semantic-tokens section is the single source of truth for both the legend advertised at initialize time and the indices baked into each token.

Types (LSP 3.17 standard set only; every entry below is emitted by the mapping in §2/§3, none are dead):

```
0  CLASS
1  INTERFACE
2  ENUM
3  ENUM_MEMBER
4  PROPERTY
5  METHOD
6  PARAMETER
7  VARIABLE
8  TYPE
```

Omitted from LSP's standard set, because nothing this mapping emits ever needs them: `namespace` (Apex namespaces surface only inside stdlib references, which are already colored via their receiver's `class`/`method`), `struct`/`typeParameter` (Apex has neither), `function` (Apex has no free functions), `event`/`macro`/`regexp`/`decorator` (no analogue in Apex), and every non-identifier type (`keyword`/`string`/`number`/`operator`/`comment`) which the client's TextMate grammar already handles at the token level -- semantic tokens layer on top, they don't replace it.

Modifiers -- indices are bit positions in the emitted `u32` modifier bitset, so they must stay stable across releases (see §6 on encoding):

```
0  DECLARATION           (standard)
1  READONLY              (standard)
2  STATIC                (standard)
3  ABSTRACT              (standard)
4  DEFAULT_LIBRARY       (standard)
5  public                (custom)
6  private               (custom)
7  protected             (custom)
8  global                (custom)
```

The four visibility bits are custom modifier names, permitted by LSP 3.17's legend contract (the standard set enumerates only *encouraged* names). No LSP-standard modifier expresses Apex visibility; the alternative is dropping visibility from the walker's output entirely, which loses information Apex users actively care about (a `global` method in a managed-package context is a genuinely different color-worthy thing from a `public` one). Ceiling: `is_final`/`is_virtual`/`is_override`/`is_test_visible`/`is_testmethod`/`is_transient`/`is_webservice`/`@Deprecated` are not encoded in v1. Follow-up: adding any of them is one more modifier bit and one more `if flag { push_bit(N) }` in the walker; no design change.

### 2. `SymbolKind` -> token-type mapping at declaration sites

Iterate `program.symbols.symbols_of_file(file)` (already used by `document_symbols`, `enclosing_callable`, `dead_symbols_in_file`, and others); for each `Symbol`, emit one token at `symbol.name_range` (the identifier alone, exactly what goto-definition uses) with type index and modifier bitset per the table below. Every declaration site always sets the `DECLARATION` bit, plus the modifiers derived from its `ModifierSet` per §4.

| `SymbolKind`       | Token type    | Notes                                                                                                                                                                                    |
| ------------------ | ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Class`            | `CLASS`       |                                                                                                                                                                                          |
| `Interface`        | `INTERFACE`   |                                                                                                                                                                                          |
| `Enum`             | `ENUM`        |                                                                                                                                                                                          |
| `EnumConstant`     | `ENUM_MEMBER` |                                                                                                                                                                                          |
| `Trigger`          | `CLASS`       | LSP has no `trigger` type; `event` was rejected (see §1 omissions). `document_symbols` maps `Trigger` to `EVENT` for the outline, but a trigger name in a source file is class-shaped for coloring purposes. |
| `Method`           | `METHOD`      |                                                                                                                                                                                          |
| `Constructor`      | `METHOD`      | LSP has no `constructor` type; `METHOD` is the standard-editor precedent (rust-analyzer/clangd/gopls all render constructors as methods).                                              |
| `Field`            | `PROPERTY`    | LSP standard set has no `field`; `PROPERTY` is the closest fit and matches how VS Code's own built-in scopes color member variables.                                                    |
| `Property`         | `PROPERTY`    |                                                                                                                                                                                          |
| `Parameter`        | `PARAMETER`   |                                                                                                                                                                                          |
| `LocalVar`         | `VARIABLE`    |                                                                                                                                                                                          |
| `CatchVar`         | `VARIABLE`    |                                                                                                                                                                                          |
| `ForEachVar`       | `VARIABLE`    |                                                                                                                                                                                          |
| `SwitchBindingVar` | `VARIABLE`    |                                                                                                                                                                                          |

No `SymbolKind` variant is skipped: every declaration in the file gets exactly one token.

### 3. `Resolution` -> token-type mapping at reference sites

Iterate `program.resolutions_in_file(file)` (already used by `unresolved_reference_diagnostics` and others). For each `(SyntaxPtr, Resolution)`, emit one token at `program.highlight_range(ptr)` (the same identifier-only range `document_highlights`/`ptr_location` uses, so a call/field-access reference doesn't over-highlight the receiver chain -- see `highlight_range`'s own doc comment at `apex-binder/src/lib.rs:877`). The `DECLARATION` bit is never set on a reference-site token. Visibility bits and `STATIC`/`READONLY`/`ABSTRACT` mirror the referent's `ModifierSet` when there is one.

| `Resolution` variant           | Token type                                    | Modifiers                                                                                        |
| ------------------------------ | --------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `Resolved(SymbolId)`           | Look up `program.symbols.get(id).kind`, reuse the §2 table verbatim | Mirror the target's `ModifierSet` per §4, minus `DECLARATION`.                                    |
| `Candidates(Vec<SymbolId>)`    | Same as `Resolved`, using `candidates[0]`     | Same as `Resolved`, using `candidates[0]`. Order is stable (`ReferenceTable::set`'s insertion order), so the choice is deterministic. Skip only if the vec is empty (shouldn't happen; be defensive). |
| `SchemaObject(SchemaObjectRef)`  | `PROPERTY` if `field.is_some()`, else `TYPE`    | `DEFAULT_LIBRARY`.                                                                                |
| `UnknownSchema(UnknownSchemaRef)` | `PROPERTY` if `field.is_some()`, else `TYPE`    | `DEFAULT_LIBRARY`. (`SchemaObject`/`UnknownSchema` are the same shape at the token layer -- `apex-metadata`'s local-schema availability is invisible to the client.) |
| `StdlibMember(StdlibMemberRef)`  | `member: None` -> `CLASS`; `member: Some(m)` and `program.stdlib.property(&class_name, m).is_some()` -> `PROPERTY`; otherwise -> `METHOD`. The property/method disambiguation reuses the same stdlib lookup `describe_stdlib_member` (`capabilities.rs:312`) already performs on hover, so the two features can't disagree about what a given stdlib name is. | `DEFAULT_LIBRARY`. `STATIC` bit is not set in v1; ceiling: consulting `program.stdlib.method(...)/property(...).is_static` (both fields exist per `describe_stdlib_member`'s own use of them) would add it, but a stdlib method's staticness is a low-value coloring signal since the call syntax (`String.isBlank(x)` vs `s.trim()`) already communicates it. Follow-up if a client asks. |
| `Label(LabelRef)`                | `VARIABLE`                                    | `DEFAULT_LIBRARY`, `READONLY`. (A custom label is a read-only project-metadata constant; `variable` is the closest LSP-standard fit.) |
| `VisualforcePage(VisualforcePageRef)` | `CLASS`                                       | `DEFAULT_LIBRARY`. (Matches the `Page.<name>` receiver-shape: an identifier that acts like a namespaced type reference.) |
| `Unresolved`                     | Emit no token.                                | The TextMate grammar's syntactic coloring is the correct fallback -- semantic tokens layer on top of grammar-based coloring, so *not* emitting a token here means the client falls back to its own default, rather than us positively asserting an incorrect kind. Skipping unresolved is also what rust-analyzer does. |

Edge cases the walker handles explicitly, called out because they'd otherwise be silent:

- Two resolutions with the same range (a same-name unqualified call that records against both a concrete override and its interface's own abstract declaration -- the same shape `visibility_narrowing_diagnostics`'s ticket 34 called out at `apex-binder/src/visibility_narrowing.rs`). Emit only the token derived from the first-encountered resolution; skip duplicates by `(line, start_col)` key during the pre-sort pass in §6. Downstream color contention was not observed with rust-analyzer's own implementation of a comparable de-dup; this matches.
- A resolution's `SyntaxPtr` whose `highlight_range` spans more than one line (a multi-line dotted access, wrapped across a linebreak). LSP semantic tokens are single-line by spec; skip these entirely in v1. Same LSP-imposed limit rust-analyzer honors.
- A declaration and a reference at the same range (a self-reference recorded against a declaration's own identifier -- can happen for e.g. `this` propagation). Prefer the declaration; skip the duplicate reference token. Same `(line, start_col)` de-dup key.

### 4. `ModifierSet` -> `SemanticTokenModifier` bits

Applied identically at both declaration sites (§2, always with `DECLARATION` set) and reference sites (§3, `DECLARATION` never set) when there is a referent `Symbol` to read modifiers off (i.e. `Resolved`/`Candidates`; the schema/stdlib/label/page variants don't have a `SymbolId`-backed `ModifierSet` -- their modifiers are the fixed `DEFAULT_LIBRARY` (+`READONLY` for `Label`) set by §3).

- `modifiers.is_static` -> `STATIC` bit
- `modifiers.is_final` -> `READONLY` bit
- `modifiers.is_abstract` -> `ABSTRACT` bit
- `modifiers.visibility`:
  - `Visibility::Public` -> `public` bit
  - `Visibility::Private` -> `private` bit
  - `Visibility::Protected` -> `protected` bit
  - `Visibility::Global` -> `global` bit
- Every other `ModifierSet` field (`is_virtual`, `is_override`, `is_testmethod`, `is_transient`, `is_webservice`, `is_test_visible`, `sharing`) -> not encoded in v1, per §1's ceiling.

`DEFAULT_LIBRARY` is set only by §3, per its variant table -- never derived from `ModifierSet`.

### 5. Advertised capability

In `Backend::initialize` (`crates/apexls-server/src/lib.rs:657-758`), insert a new `semantic_tokens_provider` field alphabetically-ish in the `ServerCapabilities` block -- between `selection_range_provider` (line 700) and `signature_help_provider` (line 679; keep the existing ordering as-is around it, matching this block's already-loose alphabetical convention):

```rust
semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
    SemanticTokensOptions {
        work_done_progress_options: Default::default(),
        legend: SemanticTokensLegend {
            token_types: LEGEND_TYPES.to_vec(),
            token_modifiers: LEGEND_MODIFIERS.to_vec(),
        },
        range: Some(true),
        full: Some(SemanticTokensFullOptions::Bool(true)),
    },
)),
```

`full: Bool(true)` (not `Delta { delta: Some(true) }`): delta (`textDocument/semanticTokens/full/delta`, `edits`) is **not** implemented in v1. See non-goals. `range: true` because a range request is trivially the same walk filtered by `range.contains_range(...)` on each token's `TextRange`, which is materially cheaper than always producing the full document's tokens when a client only wants the visible viewport.

No dynamic registration: the capability is advertised statically at `initialize` time and never re-registered, matching every other capability this server declares.

### 6. Encoding

Per LSP 3.17's `SemanticTokens.data` contract: five `u32`s per token -- `(delta_line, delta_start, length, type_index, modifier_bitset)`. First token's `delta_line`/`delta_start` are relative to `(0, 0)`; every subsequent token's are relative to the previous token's *original* (line, start), with `delta_start` resetting to the token's absolute start on any `delta_line != 0`.

Positions and lengths respect the client-negotiated `line_index::PositionEncoding` (already threaded into every other position-emitting capability). Concretely:

- `delta_line` is a raw line number (encoding-independent).
- `delta_start` and `length` are counted in the negotiated encoding's units (UTF-8 bytes, UTF-16 code units, or UTF-32 code points), same as any `Position.character` this server emits elsewhere. Reuse `line_index::LineIndex::to_position` (which already handles all three encodings, `line_index.rs:75-89`) for the per-token `(line, character)`; compute `length` as `to_position(range.end()).character - to_position(range.start()).character` for the single-line tokens the walker emits (multi-line tokens are already skipped in §3).

Pre-sort: collect every token as `(line, start_col, length, type_idx, modifier_bits)`, sort ascending by `(line, start_col)` (stable), de-dup by `(line, start_col)` (§3 edge cases), then delta-encode in a single linear pass.

### 7. Handler shape

Two new `LanguageServer` impl methods on `Backend`, each following `Backend::document_symbol`'s exact structure (`crates/apexls-server/src/lib.rs:1269-1292`):

```rust
fn semantic_tokens_full(
    &mut self,
    params: SemanticTokensParams,
) -> BoxFuture<'static, Result<Option<SemanticTokensResult>, Self::Error>> {
    let uri = params.text_document.uri;
    let encoding = self.position_encoding;
    let target_version = self.bind.documents.lock().version;
    let bind = Arc::clone(&self.bind);
    Box::pin(async move {
        wait_for_rebuild(&bind, target_version).await;
        let program_guard = bind.program.read();
        let Some(program) = program_guard.as_ref() else {
            return Ok(None);
        };
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let Some(file) = program.file_id(&path) else {
            return Ok(None);
        };
        Ok(Some(SemanticTokensResult::Tokens(
            capabilities::semantic_tokens_full(program, file, encoding),
        )))
    })
}
```

`semantic_tokens_range` is the same, plus `params.range` -> `TextRange` via `LineIndex::to_offset` (start and end) before dispatching to `capabilities::semantic_tokens_range(program, file, range, encoding)`. When the incoming `Range` doesn't fit in `text` (a stale request against a since-shortened file), fall through to returning `Ok(None)` -- same defensive shape as the existing `resolve_position` returning `None`.

Neither handler needs a new `Router` route: `Router::from_language_server` (`lib.rs:1526`) already dispatches `textDocument/semanticTokens/full` and `.../range` to the corresponding trait methods.

### 8. Where the walk goes

New `pub(crate)` functions in `crates/apexls-server/src/capabilities.rs`, placed alphabetically -- after `selection_range_at` (currently at line 750) and before `RenameRefusal` (line 802). Signatures:

```rust
pub(crate) fn semantic_tokens_full(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> SemanticTokens;

pub(crate) fn semantic_tokens_range(
    program: &BoundProgram,
    file: FileId,
    range: TextRange,
    encoding: PositionEncoding,
) -> SemanticTokens;
```

Both delegate to one private `fn collect_tokens(program, file, filter: Option<TextRange>)` that does the §2/§3 walk once; the `_range` wrapper passes `Some(range)`, `_full` passes `None`. Delta-encoding (§6) happens after `collect_tokens` returns, so `result_id` is left `None` (no server-side caching in v1).

### 9. Tests

New file `crates/apexls-server/tests/semantic_tokens.rs`. Note: the task's original file-listing hint referred to `tests/document_symbol*.rs` / `folding_range*.rs`, which don't exist in this repo -- the actual style precedents to match are `crates/apexls-server/tests/inlay_hints.rs`, `signature_help.rs`, and `references_highlight.rs`: each spawns the real `apexls-server` binary via `Command::new(env!("CARGO_BIN_EXE_apexls-server"))`, drives it over stdio with the local `send`/`recv`/`Session` helpers, and waits on the background rebuild's `"rebuild complete"` stderr line before sending a request. Copy that harness verbatim (an even shorter, in-process alternative would need `run_server`'s internals to be re-exported, which they aren't).

Coverage, minimum -- one test per major mapping bucket, each driven by a small inline Apex source string, each asserting either the raw `SemanticTokens.data` u32 sequence or (more readable) a helper-decoded `Vec<(line, start_col, length, type_name, modifiers)>`:

1. **Class declaration** -- top-level `class Foo {}`, one `CLASS` + `DECLARATION` token at `Foo`.
2. **Interface declaration + implementing class** -- `interface I {}` + `class C implements I {}`; assert `INTERFACE`+`DECLARATION` on `I`'s decl, `INTERFACE` (no `DECLARATION`) on the `implements I` reference, `CLASS`+`DECLARATION` on `C`.
3. **Method + constructor declaration** -- `class C { public C() {} public void run() {} }`; two `METHOD`+`DECLARATION` tokens, one with the `public` visibility bit each.
4. **Field + property declaration** -- `class C { private static final Integer X = 0; private Integer Y { get; set; } }`; `PROPERTY`+`DECLARATION`+`private`+`static`+`readonly` on `X`, `PROPERTY`+`DECLARATION`+`private` on `Y`.
5. **Local variable + parameter** -- `class C { void run(Integer p) { Integer q = p; } }`; `PARAMETER`+`DECLARATION` on `p`'s decl, `VARIABLE`+`DECLARATION` on `q`, `PARAMETER` (no `DECLARATION`) on the `p` reference.
6. **Static call to a project-local method** -- `class A { public static void go() {} } class B { void run() { A.go(); } }`; `CLASS` on the `A` receiver ref, `METHOD`+`static`+`public` on the `go` call.
7. **Stdlib call (bare class + method)** -- `class C { void run() { System.debug('hi'); } }`; `CLASS`+`DEFAULT_LIBRARY` on `System`, `METHOD`+`DEFAULT_LIBRARY` on `debug`.
8. **Stdlib property vs method disambiguation** -- `class C { void run() { Integer n = 'x'.length(); String s = 'x'.toString(); } }` -- both `.length()` and `.toString()` are methods; assert `METHOD`+`DEFAULT_LIBRARY` for each and no `PROPERTY` for the stdlib references. (A concrete property/method contrast pair against real scraped data can be added when a scraped property surfaces cleanly; the shape is what's being tested here.)
9. **Schema reference** -- `class C { void run() { Account a; a.Name = 'x'; } }`; `TYPE`+`DEFAULT_LIBRARY` on `Account`, `PROPERTY`+`DEFAULT_LIBRARY` on `Name`.
10. **Unresolved reference** -- `class C { void run() { nonExistentThing(); } }`; assert **no** token is emitted for `nonExistentThing`. (Explicitly check the token stream contains nothing at that range.)
11. **Range mode** -- pick any of the above fixtures, request `semanticTokens/range` for a subset, assert every returned token's `(line, start_col)` falls inside the requested range and no others.
12. **Encoding sanity** -- one fixture with a non-ASCII identifier character in a comment above a declaration (so the comment shifts UTF-16 offsets vs UTF-8); assert the emitted `delta_start`/`length` matches the negotiated encoding. (The client negotiation happens at `initialize` time; the harness in `inlay_hints.rs`/`references_highlight.rs` already sets `general.position_encodings` in its `initialize` request -- reuse whatever encoding those default to, and add one test that overrides it.)

Tests 1-9 are per-major-category, per the task's explicit "at least one test per major token category" bar. 10-12 cover the walker's own edge cases identified in §3/§6.

### 10. Acceptance criteria

- `semanticTokensProvider` appears in the server's `initialize` response with the exact `SemanticTokensLegend` from §1 and `range: true`/`full: true`.
- `textDocument/semanticTokens/full` returns a valid delta-encoded `SemanticTokens.data` per §6 for every file the binder has in `program.bodies`; unknown/unbound files return `Ok(None)`.
- `textDocument/semanticTokens/range` returns a strict subset of the corresponding `full` response, filtered by `range` (each returned token's absolute `(line, start_col)` lies inside `params.range`).
- Every `SymbolKind` variant produces its §2 token type at its own declaration site. Every `Resolution` variant produces its §3 token type at each reference site, or (for `Unresolved`) no token at all.
- Every `ModifierSet` field enumerated in §4 sets its corresponding bit; every reference site's modifier bitset mirrors its referent's (`DECLARATION` excluded at reference sites).
- Zero panics against the full real NPSP corpus (`tests/corpus/npsp/`) when a semantic-tokens request is issued for every discovered file -- add one throwaway harness under `examples/` (built, run, deleted before commit, matching `missing_implementation_diagnostics.rs`'s NPSP-sanity precedent recorded in ticket 17's Answer) to confirm this before landing. No assertion on the *content* of the corpus's tokens -- just that the walker completes for every file without panicking, and that no file produces a `SemanticTokens.data` array whose length isn't a multiple of 5 (a valid delta encoding always is).
- Full `cargo test -p apexls-server` passes with the new file; full `cargo check --workspace` passes.

### 11. Non-goals (explicit, so scope creep is easy to spot in review)

- **No delta encoding** (`textDocument/semanticTokens/full/delta`, `edits`). `full: Bool(true)` in §5 excludes it deliberately. Follow-up if a real client (or a real perf number) asks for it: the walker's output is already sorted and de-duped, so a diff between two runs is mechanical to add, but v1 pays no `result_id` cache cost.
- **No dynamic registration.** The capability is declared once at `initialize` and never re-registered. `client/registerCapability`/`unregisterCapability` for semantic tokens is out of scope.
- **No re-highlighting on hover / on config change.** The tokens for a file change exactly when its `program.bodies[file]` changes -- which is exactly when the rebuild worker fires a `publishDiagnostics` push. Clients that want to re-fetch semantic tokens on that signal already do; apexls doesn't push a semantic-tokens refresh notification (`workspace/semanticTokens/refresh`) in v1.
- **No per-token tooltip / debug metadata.** LSP has no such extension; the client's own token-inspector command reads the type/modifier legend, and that's the whole surface.
- **No custom modifiers beyond visibility.** Every §4 ceiling (`is_virtual`, `is_override`, `is_testmethod`, etc.) stays unencoded until a specific need surfaces.
- **No re-implementation of TextMate-grammar coloring.** Semantic tokens *supplement* the client's grammar; anything the walker doesn't emit falls back to the client's own grammar, which is the desired behavior for keyword/string/number/comment tokens (§1).
