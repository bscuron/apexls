# Substrate survey for a future `apexls query` (structural search & replace)

Facts only, with `path:line` citations. Section 1 (syntax tree representation) is
omitted — delivered separately. Sections 2-7 plus the crate dependency graph.

Snapshot taken at `227590d` + one uncommitted change to `crates/apexls/src/soql.rs`
(trigger-extension dispatch, see §4).

---

## 2. Round-tripping / printing (`crates/apex-printer`)

`crates/apex-printer/src/lib.rs` is **27 lines total**, including its test. It is a
**faithful reproducer, not a formatter** — there is no formatter anywhere in the
workspace.

```rust
pub fn render(node: &SyntaxNode) -> String { node.text().to_string() }
```
`crates/apex-printer/src/lib.rs:13-15`

- Module doc, `crates/apex-printer/src/lib.rs:1-8`: "`render(parse(source)) == source`
  (byte-for-byte) is the core round-trip correctness property this crate exists to
  make checkable… rendering is just concatenating every token's text in source
  order: `SyntaxNode::text()` already does exactly that."
- **Round-trip holds unconditionally, errors or not.**
  `crates/apex-parser/tests/roundtrip.rs:12-21`: "the tree-building sink flushes
  every remaining raw token (including anything the grammar didn't recognize)
  before the root node closes, so a fragment hitting an unsupported construct
  (SOQL, a declaration, …) still round-trips — it just does so with recorded errors
  and a less useful tree shape, not dropped bytes. A round-trip *mismatch* is
  therefore always a real bug (a byte genuinely lost or duplicated), never an
  expected-scope gap."
- Also asserted inside the parser's own recovery test:
  `crates/apex-parser/src/lib.rs:226-230` — "recovery must still keep the tree
  lossless".

**Current consumers:**
- `crates/apexls/src/ast.rs:42-50` — the `apexls ast` command renders the tree and
  prints `round-trips exactly: {bool}`, exiting FAILURE if it doesn't.
- `apex-parser` dev-dependency (`crates/apex-parser/Cargo.toml:12`) for
  `tests/roundtrip.rs` and `tests/ast_roundtrip.rs`.
- `apex-printer` is a declared dependency of `crates/apexls`
  (`crates/apexls/Cargo.toml:15`) but used only by `ast.rs`.

**Implication for rewrite:** printing an *unmodified* subtree back is free and
exact. There is no machinery for printing a *modified* tree — rowan's green-tree
mutation APIs are not used anywhere in the workspace, and all existing editing is
done as text-range splices over the source string (see §5).

---

## 3. Error recovery (`crates/apex-parser`)

**A tree is always produced, and it is always lossless.**

- `crates/apex-parser/src/lib.rs:3-5`: "Error recovery is a first-class design
  goal: on a syntax error the parser should resynchronize at a statement/member
  boundary and keep producing a best-effort tree for the rest of the file."
- `crates/apex-parser/src/errors.rs:3-6`: "Nothing in this crate panics on
  malformed *input* — a syntax error is always represented as an entry in
  `Parse::errors` plus a best-effort tree (holes left by `Parser::expect` failures,
  bad spans wrapped in `SyntaxKind::ErrorNode` by recovery), never an abort."

**`Parse`** (`crates/apex-parser/src/errors.rs:32-36`):
```rust
pub struct Parse {
    pub(crate) green: GreenNode,
    pub errors: Vec<ParseError>,
    pub(crate) text: Arc<str>,
}
```
Accessors `syntax()`, `ok()`, `text()` at `errors.rs:38-52`. Cloning is two `Arc`
bumps plus a `Vec<ParseError>` copy (`errors.rs:19-30`).

**`ParseError`** (`errors.rs:13-17`): `{ message: String, offset: u32 }` — note it
carries a single byte **offset, not a range**.

**Malformed goldens confirm the shape a partial parse leaves behind:**
- `a = ;` →`StmtRoot > ExprStmt > BinExpr` with a hole where the RHS would be
  (`crates/apex-parser/tests/snapshots/golden__malformed@03_unexpected_token.cls.snap`).
- `a = foo.bar(` → a full `MethodCallExpr` with an unterminated `ArgList`, three
  recorded errors
  (`crates/apex-parser/tests/snapshots/golden__malformed@05_dangling_method_call.cls.snap`).
- Five malformed goldens exist in total (`golden__malformed@01..05`).

### Fragment-parse entry points already exist

This is the directly relevant fact for parsing a *pattern fragment*. `SyntaxKind`
declares dedicated fragment roots — `ExprRoot, StmtRoot, BlockRoot, ErrorNode`,
commented "Roots (fragment-parse entry points) / recovery"
(`crates/apex-syntax/src/syntax_kind.rs:147`). Public API:

| Entry point | Root kind | Line |
|---|---|---|
| `apex_parser::parse_expression(src)` | `ExprRoot` | `crates/apex-parser/src/lib.rs:62-67` |
| `apex_parser::parse_statement(src)` | `StmtRoot` | `crates/apex-parser/src/lib.rs:71-76` |
| `apex_parser::parse_block(src)` | `BlockRoot` | `crates/apex-parser/src/lib.rs:79-84` |
| `parse_compilation_unit(src)` | `CompilationUnit` | `crates/apex-parser/src/lib.rs:88-92` |
| `parse_trigger_unit(src)` | `TriggerUnit` | `crates/apex-parser/src/lib.rs:96-100` |
| `_with_cache` / `_with_cache_and_text` variants | — | `crates/apex-parser/src/lib.rs:121-141` |

- **Caveat on `parse_expression`** (`crates/apex-parser/src/lib.rs:56-61`):
  "Trailing content after the expression is left unconsumed in the token stream but
  does not appear in the returned tree — callers wanting 'this whole string must be
  exactly one expression' should check `Parse::errors` is empty **and** that the
  tree's text covers all of `src`."
- `for (...) { ... }` parses via `parse_statement` (see the round-trip fixture list
  at `crates/apex-parser/src/lib.rs:400-444`, which includes `for` classic, for-each,
  `for (;;)`, `try/catch/finally`, `switch on`, all six DML forms, `System.runAs`).
- **A bare `catch` clause has no entry point.** `CatchClause` is only reachable
  through `grammar::statements::statement`'s `try` arm; parsing `catch (...) {...}`
  standalone would yield an error tree.
- `NodeCache` is re-exported (`crates/apex-parser/src/lib.rs:37`) — the fragment
  entry points each build a throwaway `NodeCache::default()` internally
  (`lib.rs:63, 72, 80`); only the compilation-unit/trigger paths accept a shared one.

### Stack-safety constraint any new walker must respect

`crates/apex-parser/src/lib.rs:7-30`: dropping a deeply left-nested `BinExpr` chain
recurses one frame per tree level via rowan's `Drop` glue and overflows a default
(~1 MiB) Windows thread stack at roughly **6,500-7,000 terms** — on *drop*, not on
parse. `pub const RECOMMENDED_MIN_STACK_SIZE: usize = 64 * 1024 * 1024`
(`crates/apex-parser/src/lib.rs:50`).

Two established in-tree patterns:
- Spawn a big-stack thread and join: `crates/apexls/src/ast.rs:21-27`.
- Build the global rayon pool with that stack size:
  `crates/apexls/src/soql.rs:123-125`.

### Corpus coverage

`crates/apex-parser/tests/whole_file_parse_rate.rs:1-6` gates that **every** real
NPSP `.cls`/`.trigger` compilation unit parses with **zero errors**. Two path
patterns are excluded as genuinely-not-compilation-units (anonymous Apex scripts):
`scripts/*.cls` and `datasets/rd2/config_npsp_for_ldv_data_load.cls`
(`whole_file_parse_rate.rs:8-15, 19-23`).

---

## 4. Existing traversal precedent — `apexls soql` and `apexls ast`

### `crates/apexls/src/soql.rs` (338 lines)

**This file changed on disk mid-survey.** `queries_in_file` now dispatches on file
extension (`crates/apexls/src/soql.rs:163-178`): a `.trigger` file goes to
`apex_parser::parse_trigger_unit`, everything else to `parse_compilation_unit`,
because "A `.trigger` file is not a compilation unit — parsing one as a class
yields an error tree with no `SoqlExpr` in it at all, which would silently drop
every query written in a trigger." Citations below reflect current on-disk state.

**Control flow:**

1. `run(paths)` — `crates/apexls/src/soql.rs:87-106`. Gets CWD via
   `std::env::current_dir()`, calls `find_queries`, prints, always returns
   `ExitCode::SUCCESS`. An `ArgError` prints to stderr and returns its code.
2. `find_queries(paths, cwd) -> Result<Vec<Query>, ArgError>` —
   `crates/apexls/src/soql.rs:111-161`. The testable core, factored out of `run`
   specifically so tests don't touch process-global CWD or capture stdout
   (`soql.rs:108-110`; same rationale spelled out at
   `crates/apexls/src/check.rs:67-71`).
   - `canonicalize_filters(paths)?` — `soql.rs:112`
   - Global rayon pool with the 64 MiB stack — `soql.rs:123-125`; comment notes
     failure only means the pool was already built, which is out of this command's
     control.
   - `find_project_root(cwd)` then `apex_discover::find_apex_files(&root)` —
     `soql.rs:127-128`
   - **`files.par_iter()`** — `soql.rs:131`. **Yes, the walk is rayon-parallel**,
     one file per work item.
   - Path filter first, short-circuiting the `canonicalize()` syscall entirely when
     `filters.is_empty()` — `soql.rs:132-142`.
   - `std::fs::read_to_string(path).ok()?` — an unreadable file is skipped, never
     fatal, "matching how `apex_discover`'s own walk treats an entry it can't read"
     (`soql.rs:143-151`).
   - `.flatten().collect()`, then a **global sort after collection**
     (`soql.rs:155-158`): `queries.sort_by(|a, b| (&a.path, a.line, a.col).cmp(...))`,
     because the parallel walker's order varies run to run.
3. `queries_in_file(display_path, src) -> Vec<Query>` —
   `crates/apexls/src/soql.rs:163-227`. Parse → `LineIndex::new(src)` →
   `parse.syntax().descendants().filter_map(|node| match node.kind() { … })`
   (`soql.rs:180-184`).

**Two matched shapes** (`soql.rs:5-14` documents both):
- Bare `SyntaxKind::SoqlExpr` (`soql.rs:185`). A subquery completes as its own
  `SoqlSubQuery` kind, so a top-level inline query is reported exactly once and
  never again for its own subqueries (`soql.rs:9-12`, test at `soql.rs:286-301`).
- `SyntaxKind::MethodCallExpr` cast to the typed `MethodCallExpr`, whose `.target()`
  must be an `Expr::Name` whose token is `eq_ignore_ascii_case("Database")`, and
  whose `.method_name_token()` lowercases into `DATABASE_QUERY_METHOD_NAMES`
  (`soql.rs:186-201`). That list is 10 entries (`soql.rs:66-77`), sourced from the
  scraped `System.Database` signatures in `apex-stdlib`'s `apex_reference.json`
  rather than from memory (`soql.rs:53-60`), and deliberately narrower than
  `bulkification_diagnostics`' `DATABASE_BULK_METHOD_NAMES`.

**Position anchoring — the reusable trick.** `soql.rs:203-217` deliberately does
*not* use `node.text_range()`:

```rust
let mut significant = node
    .descendants_with_tokens()
    .filter_map(|e| e.into_token())
    .filter(|t| !t.kind().is_trivia());
let first = significant.next()?;
let last = significant.last().unwrap_or_else(|| first.clone());
let start = usize::from(first.text_range().start());
let end = usize::from(last.text_range().end());
let (line, col) = index.line_col(src, start as u32);
```

Comment at `soql.rs:203-209`: "a node's green-tree span can swallow trivia at either
end (the sink starts a node before flushing its first token's leading trivia, and
flushes trailing trivia before finishing it…), so a query preceded by a comment
would otherwise report the comment's position and print the comment as part of the
query." The same hazard is documented for declared names at
`crates/apex-syntax/src/ast/mod.rs:106-125` (`Name::ident_range()` exists solely for
it: "Every caller that needs an exact, tight identifier span (a `Symbol::name_range`,
a rename's replacement range, …) must go through this rather than the raw node
range").

**Output format** (`crates/apexls/src/soql.rs:97-103`) — ripgrep `--vimgrep` shape,
no spaces around separators, one match per line:
```rust
println!("{}:{}:{}:{}", q.path.display(), q.line, q.col, q.text);
```
Multi-line matches are flattened by `collapse()` (`soql.rs:223-225`), which is just
`text.split_whitespace().collect::<Vec<_>>().join(" ")`. It collapses runs inside
string literals too — documented as deliberate: "this is a grep-shaped locator
format, and a query's exact internal spacing is something to go read at
`path:line:col`, not something this output claims to preserve" (`soql.rs:217-222`).

`Query` is a private struct `{ path: PathBuf, line: usize, col: usize, text: String }`
(`soql.rs:79-85`). Five unit tests live in the same file (`soql.rs:227-338`), each
writing fixtures into `std::env::temp_dir()` and calling `find_queries` directly.

`LineIndex` is `apexls_server::LineIndex` (`soql.rs:48`), re-exported at
`crates/apexls-server/src/lib.rs:125`. Its CLI-facing method is
`line_col(&self, text: &str, offset: u32) -> (usize, usize)`
(`crates/apexls-server/src/line_index.rs:159`); `to_position`/`to_offset`
(`line_index.rs:75, 96`) are LSP-facing and take a `PositionEncoding`.

**`soql` binds nothing** — `soql.rs:33-35`: "Because neither shape needs any name
resolution, this binds nothing: it discovers, parses, and walks, skipping the whole
`BoundProgram` cost `check`/`fix` pay."

### `crates/apexls/src/ast.rs` (57 lines)

Single-file only. Runs everything on a spawned 64 MiB-stack thread and joins
(`ast.rs:21-27`). Reads the file, `parse_compilation_unit` (note: **no** trigger
dispatch here, unlike `soql.rs`), `println!("{:#?}", parse.syntax())` using rowan's
own `Debug`, prints `{n} error(s)` and each `message (byte offset N)`, then
`apex_printer::render` and `round-trips exactly: {bool}`; exit code is FAILURE iff
it doesn't round-trip (`ast.rs:39-56`).

### `crates/apexls/src/project.rs` (144 lines)

Shared plumbing. It does **not** load or bind the project — three helpers only:

- `find_project_root(start: &Path) -> PathBuf` (`project.rs:14-25`) — walks
  *upward* for `sfdx-project.json`, falls back to `start`. "there's no override
  flag, matching how `cargo`/`git` locate their own project roots" (`project.rs:11-13`).
- `matches_any(file_path, filters) -> bool` (`project.rs:34-36`) —
  `filters.is_empty() || filters.iter().any(|f| file_path.starts_with(f))`. Empty
  means everything; `Path::starts_with` is component-aware so `Foo2.cls` doesn't
  match a `Foo` directory filter (`project.rs:27-33`, test at `project.rs:90-104`).
- `canonicalize_filters(paths) -> Result<Vec<PathBuf>, ArgError>` (`project.rs:46-65`).
- `ArgError(pub String, pub u8)` (`project.rs:41`) — message + process exit code; 2
  for a nonexistent/unresolvable path argument.

Its module doc (`project.rs:1-4`) still names only "(`check`, `fix`)" and was not
updated when `soql` began using it.

**Project loading is per-subcommand, not shared:**
- `check`: `BoundProgram::from_files(&root)` — `crates/apexls/src/check.rs:75`
- `fix`: `BoundProgram::from_files_cached(&root, &HashMap::new(), &mut cache)` —
  `crates/apexls/src/fix.rs:108`
- `soql`: `apex_discover::find_apex_files(&root)` only — `crates/apexls/src/soql.rs:128`

`check` also parallelizes per-file over rayon (`check.rs:85-119`) and globally sorts
afterward, for the same walk-order reason (`check.rs:76-84`).

### Adding a subcommand

Three edits in `crates/apexls/src/main.rs`: a `mod` line (`main.rs:9-13`), a
`#[derive(Subcommand)]` variant whose doc comment becomes the `--help` text
(`main.rs:34-45`), and a match arm (`main.rs:48-61`). Every subcommand returns
`ExitCode`. clap 4.6 with `derive` (`crates/apexls/Cargo.toml:18`).

---

## 5. Existing edit/rewrite machinery

The entire pipeline lives in **`crates/apexls-server/src/fix.rs` (203 lines)** and
every item in it is **`pub(crate)`** — none of it is publicly reachable.

### Edit representation

`crates/apexls-server/src/fix.rs:19-33`:
```rust
pub(crate) struct CandidateFix {
    pub range: TextRange,          // what gets replaced
    pub trigger_range: TextRange,  // narrower range an LSP codeAction must overlap
    pub new_text: String,
    pub description: String,
}
```
So: **range + replacement text**, as byte-offset `rowan::TextRange`. `trigger_range`
is LSP-only — e.g. a dead symbol's own *name*, so a cursor elsewhere inside the
deletion span doesn't spuriously trigger the action; "`apexls fix`'s batch
application ignores this entirely" (`fix.rs:26-32`).

### Production

`candidate_fixes_for_file(program: &BoundProgram, file: FileId) -> Vec<CandidateFix>`
(`crates/apexls-server/src/fix.rs:38-52`) is the **only** producer, hardwired to
`apex_binder::dead_symbols_in_file`. It maps each
`DeadSymbol { kind, visibility, name, name_range, deletion_range }`
(`crates/apex-binder/src/dead_code.rs:290-296`) to `range: deletion_range`,
`trigger_range: name_range`, `new_text: String::new()` — **deletions only**.

Module doc `fix.rs:1-10`: "Protocol-agnostic candidate-fix production and conflict
resolution, shared between the LSP's own `textDocument/codeAction` handlers
(`capabilities::dead_code_actions`) and `apexls fix`'s batch CLI command… v1 covers
only `dead_code_diagnostics`."

### Overlap resolution

`resolve_fix_conflicts(Vec<CandidateFix>) -> FixResolution { applied, conflicts }`
(`crates/apexls-server/src/fix.rs:60-122`). Two order-independent phases
(`fix.rs:65-82`):

1. Drop every fix **strictly contained** by another —
   `fixes[j].range.contains_range(fixes[i].range) && fixes[j].range != fixes[i].range`
   (`fix.rs:85-93`). Containment is transitive so nesting depth doesn't matter. A
   subsumed fix is dropped **silently** and appears in **neither** output list
   ("its target text is erased by the containing fix regardless", `fix.rs:54-59`).
2. Among survivors — none of which contains another, by construction — any non-empty
   `range.intersect(other)` is a genuine **crossing** overlap; **both** fixes are
   skipped and reported as conflicts (`fix.rs:95-108`).

Touching-but-not-overlapping (`0..5` and `5..10`) is **not** a conflict
(`fix.rs:181-188`). `fix.rs:73-75`: "this is the one place overlap policy lives,
never special-cased per diagnostic pair."

### Application

`apply_fixes(text: &str, mut fixes: Vec<CandidateFix>) -> String`
(`crates/apexls-server/src/fix.rs:129-137`):
```rust
fixes.sort_by_key(|f| std::cmp::Reverse(f.range.start()));
let mut text = text.to_string();
for fix in fixes {
    let range = usize::from(fix.range.start())..usize::from(fix.range.end());
    text.replace_range(range, &fix.new_text);
}
```
Highest-starting-offset first, so each edit lands against a still-untouched prefix
and no offset remapping is needed. Requires pairwise-disjoint ranges — exactly what
`resolve_fix_conflicts`' `applied` guarantees (`fix.rs:124-128`). **Pure string in /
string out; it never touches disk.**

Six unit tests cover the policy (`fix.rs:139-202`): disjoint, nested-subsumed,
crossing, touching, multi-deletion apply, nested apply.

### The one public seam

`apexls_server::resolve_fixes_for_file(program: &BoundProgram, file: FileId) -> CliFixResolution`
(`crates/apexls-server/src/lib.rs:2100-2130`). Pulls `program.source_text(file)`,
builds a `LineIndex`, runs `resolve_fix_conflicts(candidate_fixes_for_file(…))`,
converts each surviving fix to `CliFix`, and returns:

```rust
pub struct CliFix { pub line: usize, pub col: usize, pub description: String }   // lib.rs:2075-2079
pub struct CliFixResolution {                                                    // lib.rs:2089-2093
    pub applied: Vec<CliFix>,
    pub conflicts: Vec<CliFix>,
    pub new_text: Option<String>,
}
```

`new_text` is `None` when `applied` is empty. **The public type discards every
`TextRange`** — only line/col/description survive the boundary.

### Disk writes and the cascade loop

`crates/apexls/src/fix.rs`:
- `std::fs::write(&file_path, new_text)` at `crates/apexls/src/fix.rs:140-147` is
  the only write, sequential, after a parallel `par_iter()` resolve phase
  (`fix.rs:114-133`), explicitly because writes are real I/O (`fix.rs:110-113`).
- **Always writes; no dry-run mode** (`fix.rs:2-3`).
- Loops bind → fix → write pass-by-pass until a pass applies nothing, capped at
  `MAX_PASSES: u32 = 20` (`fix.rs:36`; loop `fix.rs:107-186`), reusing a persistent
  `apex_binder::BindCache` so unchanged files' parses stay cached (`fix.rs:102, 108`).
- The cap is defensive only: "every pass strictly deletes code and a fixed finding
  can't reappear, so convergence is structurally guaranteed" (`fix.rs:20-25`).
  Non-convergence returns `ArgError(..., 1)` (`fix.rs:177-185`).
- Only the **final** pass's conflicts are reported; applied fixes are flattened
  across all passes (`fix.rs:93-98`).
- Exit code: FAILURE iff any conflict remains (`fix.rs:78-86`).

### Could a query-rewrite reuse this as-is?

Mechanically the core is generic — `resolve_fix_conflicts` and `apply_fixes` know
nothing about dead code and would work unchanged on rewrite edits. Two concrete
blockers:

1. **Visibility.** `CandidateFix`, `candidate_fixes_for_file`,
   `resolve_fix_conflicts`, `apply_fixes` and `FixResolution` are all `pub(crate)`
   in `apexls-server` (`fix.rs:19, 38, 60, 83, 129`). The only `pub` door,
   `resolve_fixes_for_file`, hardcodes dead-code as its sole fix source and requires
   a `&BoundProgram` (so, a full project bind that a syntax-only query wouldn't
   otherwise need).
2. **Semantics tuned for deletion.** Phase-1 silent subsumption is correct when the
   outer edit erases the inner one's text; for a *rewrite*, an outer replacement
   silently discarding a nested replacement is a different (and probably wrong)
   policy.

### A second, separate edit path (LSP-typed, not reusable)

`crates/apexls-server/src/capabilities.rs` builds `lsp_types::WorkspaceEdit` /
`TextEdit` directly:
- `rename_edits(...) -> Result<WorkspaceEdit, RenameRefusal>` —
  `capabilities.rs:1374-1437`, grouping `HashMap<Url, Vec<TextEdit>>` at
  `capabilities.rs:1414-1435`; collision check `renamed_symbol_collides`
  at `capabilities.rs:1314`; `prepare_rename_range` at `capabilities.rs:1340`;
  `rename_target` at `capabilities.rs:1182`.
- `dead_code_actions(...)` — `capabilities.rs:2519-2560`, builds its own
  `WorkspaceEdit` eagerly rather than deferring to `codeAction/resolve`
  (`capabilities.rs:2515`).
- A multi-file method-operation edit builder with a per-file `LineIndex` cache —
  `capabilities.rs:2634-2710`.

These are protocol-typed and share no code with `fix.rs`'s range machinery.

---

## 6. Semantic layer (`crates/apex-binder`)

**Reachable from the CLI: yes, directly.** `apex-binder.workspace = true` is the
first entry in `crates/apexls/Cargo.toml:12`, and `check.rs`/`fix.rs` construct a
`BoundProgram` themselves rather than routing through `apexls-server`.

### Public surface (`crates/apex-binder/src/lib.rs:65-88`)

| Re-export | Line |
|---|---|
| `call_hierarchy::{incoming_calls, outgoing_calls, is_callable, IncomingCall, OutgoingCall}` | `lib.rs:65` |
| `completion::{complete_at, CompletionCandidate, CompletionCandidateKind, CompletionContext}` | `lib.rs:66` |
| `dead_code::{kind_label, dead_symbols_in_file, is_platform_invoked_test_method, is_test_class, DeadSymbol}` | `lib.rs:67-69` |
| `file_id::FileId` | `lib.rs:70` |
| `incremental::BindCache` | `lib.rs:71` |
| `ptr::{AstPtr, SyntaxPtr}` | `lib.rs:72` |
| `reference_table::{ExternalKey, LabelRef, ReferenceTable, Resolution, SchemaObjectRef, StdlibMemberRef, UnknownSchemaRef, VisualforcePageRef}` | `lib.rs:73-76` |
| `resolve::TypeMismatch` | `lib.rs:77` |
| `label_index::LabelIndex` | `lib.rs:78` |
| `page_index::{PageIndex, VisualforcePage}` | `lib.rs:79` |
| `schema_index::SchemaIndex` | `lib.rs:80` |
| `stdlib_index::StdlibIndex` | `lib.rs:81` |
| `scope::{Scope, ScopeId, ScopeKind, ScopeTree}` | `lib.rs:82` |
| `symbol::{ModifierSet, Sharing, Symbol, SymbolId, SymbolKind, Visibility}` | `lib.rs:83` |
| `symbol_table::SymbolTable` | `lib.rs:84` |
| `visibility_narrowing::{narrowing_candidates_in_file, type_narrowing_candidates_in_file, NarrowingCandidate}` | `lib.rs:85-87` |
| `apex_parser::ParseError` | `lib.rs:88` |

Private modules (not reachable): `ci_key`, `collect`, `conversions`, `db`,
`file_table`, `generics`, `inherit`, `resolve` (except `TypeMismatch`),
`salsa_stage1_dual_run`, `salsa_stage2_dual_run`, `soql`, `ty`
(`crates/apex-binder/src/lib.rs:35-63`).

### "What does this identifier resolve to" — YES

`crates/apex-binder/src/reference_table.rs:137-146`:
```rust
pub enum Resolution {
    Resolved(SymbolId),
    Candidates(Vec<SymbolId>),
    SchemaObject(Box<SchemaObjectRef>),
    UnknownSchema(Box<UnknownSchemaRef>),
    StdlibMember(Box<StdlibMemberRef>),
    Label(Box<LabelRef>),
    VisualforcePage(Box<VisualforcePageRef>),
    Unresolved,
}
```
Large variants are boxed to keep the per-reference size down — a real measured cost
(`reference_table.rs:131-136`).

Payloads:
- `SchemaObjectRef { object: SmolStr, field: Option<SmolStr> }` — `reference_table.rs:48-51`
- `UnknownSchemaRef { object: Option<SmolStr>, field: Option<SmolStr> }` — `reference_table.rs:54-57`
- `StdlibMemberRef { namespace: Option<SmolStr>, class_name: SmolStr, member: Option<SmolStr>, arg_count: Option<usize>, narrowed_param_types: Option<Vec<SmolStr>> }` — `reference_table.rs:100-106`
- `LabelRef { full_name: SmolStr }` — `reference_table.rs:117-119`
- `VisualforcePageRef { name: SmolStr }` — `reference_table.rs:127-129`

`ExternalKey` (`reference_table.rs:186-198`) is the stable identity for
non-`SymbolId` references: `Schema { object, field }`, `Stdlib(Box<StdlibKey>)`,
`Label { full_name }`, `VisualforcePage { name }`. `StdlibKey { class_name, member,
arg_count }` (`reference_table.rs:206-210`) — arity **is** part of the key, so
`System.debug(msg)` and `System.debug(level, msg)` are distinct groups
(`reference_table.rs:170-184`). Every component is lowercased on construction
(`reference_table.rs:162-168`).

### "Is this call to a stdlib method" — YES

`Resolution::StdlibMember(Box<StdlibMemberRef>)`. `member: None` means a bare class
name used as a value/receiver in its own right, e.g. the `String` in
`String.isBlank(...)` (`reference_table.rs:59-63`). There is no method-vs-property
flag; a consumer looks the rest back up via `StdlibIndex`
(`reference_table.rs:63-67`).

### "What is this expression's type" — NO. This is the real gap.

`crates/apex-binder/src/ty.rs:29` — `pub(crate) enum Ty`, and `ty.rs:6-8` states it
outright: "`Ty` is purely a walker-internal chaining value: it never appears in
`crate::reference_table::Resolution` (what a *reference* resolved to, a separate and
unaffected concern) **or gets stored on `BoundProgram`**."

Two variants (`ty.rs:29-55`): `Project(SymbolId)` and `System { name: SmolStr, args:
Vec<Ty> }`. `System` deliberately conflates an unmodeled stdlib type (`String`,
`Integer`, bare `List`) with a schema SObject type (`Account`) — "`Ty` only needs
'not project-local, but at least named'" (`ty.rs:14-27`).

So a predicate like "this expression has type `List<Account>`" is **not available at
any layer today**. The only publicly visible type information is per-*declaration*,
on `Symbol`:
- `type_ref: Option<AstPtr<Type>>` — `crates/apex-binder/src/symbol.rs:205`
- `type_name: Option<SmolStr>` — `symbol.rs:217`, the declared type's dotted name
  text, cached eagerly because Pass 2 only ever holds the currently-walked file's
  root (`symbol.rs:206-217`)
- `type_args: Vec<SmolStr>` — `symbol.rs:226`, **one level deep only**
  (`symbol.rs:218-226`)

All three are raw declared-name text, unresolved.

### `BoundProgram` query methods (`crates/apex-binder/src/lib.rs:210-1430`)

Construction:
- `from_files(root)` — `lib.rs:218`
- `from_files_with_overrides(...)` — `lib.rs:229`
- `from_files_cached(root, overrides, cache)` — `lib.rs:263`

Files / text / trees:
- `files() -> impl Iterator<Item = FileId>` — `lib.rs:885`
- `file_path(file) -> &Path` — `lib.rs:893`; `file_id(path) -> Option<FileId>` — `lib.rs:902`
- `syntax(file) -> SyntaxNode` — `lib.rs:938`; `source_text(file) -> &str` — `lib.rs:951`
- `syntax_errors(file) -> Cow<[ParseError]>` — `lib.rs:969`
- `type_mismatches(file) -> &[TypeMismatch]` — `lib.rs:981`; `file_count()` — `lib.rs:985`

Resolution:
- `resolution(ptr: SyntaxPtr) -> Option<&Resolution>` — `lib.rs:992`
- `resolution_at(file, offset: TextSize) -> Option<&Resolution>` — `lib.rs:1077`
- `resolutions_in_file(file) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)>` — `lib.rs:1216`
- `all_resolutions()` — `lib.rs:1205`
- `highlight_range(ptr) -> TextRange` — `lib.rs:1015`

Symbols and references:
- `symbol_at(file, offset) -> Option<SymbolId>` — `lib.rs:1181`
- `symbols_in_file(file) -> &[Symbol]` — `lib.rs:1195`
- `references_to(SymbolId) -> impl Iterator<Item = SyntaxPtr>` — `lib.rs:1227`
- `references_to_in_file(...)` — `lib.rs:1236`
- `references_to_external(ExternalKey)` / `_in_file` — `lib.rs:1251, 1262`

Scopes and calls:
- `scope_tree(block: SyntaxPtr) -> Option<&ScopeTree>` — `lib.rs:1273` (keyed by the
  body's own `Block`/`TriggerBlock` pointer, not by symbol, because a property's two
  accessors share one symbol but have two bodies — `lib.rs:106-112`)
- `call_sites_in_range(...)` — `lib.rs:1286`
- `enclosing_callable(file, offset) -> Option<SymbolId>` — `lib.rs:1315`

Binding state:
- `is_bound(file)` — `lib.rs:1349`; `is_fully_bound()` — `lib.rs:1362`
- `ensure_bound(file, cache) -> bool` — `lib.rs:1382`
- `with_full_binding<R>(...)` — `lib.rs:1411`

**The key adapter for a tree-walking query:** `resolution()` is keyed by
`SyntaxPtr`, which any walk can construct on the spot —
`SyntaxPtr::new(file, &node)` (`crates/apex-binder/src/ptr.rs:47`) or
`SyntaxPtr::for_token(file, &token)` (`ptr.rs:64`). `SyntaxPtr` exposes `.file()`,
`.kind()`, `.range()`, `.with_range()`, `.to_node(&root) -> Option<SyntaxNode>`
(`ptr.rs:72-104`). Typed variant `AstPtr<N>` with `new`/`range`/`to_node`
(`ptr.rs:122-168`).

### `Symbol` (`crates/apex-binder/src/symbol.rs:181-228`)

```rust
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: SmolStr,              // case preserved; lookup maps are lowercase-keyed
    pub file: FileId,
    pub ptr: SyntaxPtr,             // whole declaration node
    pub name_range: TextRange,      // identifier token only
    pub container: Option<SymbolId>,
    pub type_ref: Option<AstPtr<Type>>,
    pub type_name: Option<SmolStr>,
    pub type_args: Vec<SmolStr>,
    pub modifiers: ModifierSet,
}
```

### Cost of turning on the semantic layer

- `soql` binds nothing; `check`/`fix` bind the **whole project unconditionally**.
  There is no `--root`, no partial bind, and no hook to add one:
  `crates/apexls/src/check.rs:9-19` — "cross-file information (inheritance chains,
  project-wide references for `public` candidates) elsewhere in the bind could be
  incomplete from a partial project subset, and `apex-binder` has no existing hook
  for multi-root/partial binding to build this on top of anyway."
- Measured footprint on the NPSP corpus: peak heap 249 MB, retained 188 MB for
  `BoundProgram::from_files_cached` (`.scratch/apex-memory/map.md:22`). Binding is
  confirmed eager and project-wide for every file including Pass 2.

---

## 7. Existing pattern / matching code

**None exists.** No query DSL, no visitor trait, no structural matcher, no pattern
type anywhere in the workspace. A `git grep` across `crates/` for
`fn visit|fn walk|fn matches_pattern|trait Visitor|struct Matcher|Pattern\b|structural`
returned only prose inside doc comments and test names — zero matching machinery.

Also absent: any `SyntaxKind::from_str` / name-lookup function. `SyntaxKind` derives
`TryFromPrimitive` over `u16` (`crates/apex-syntax/src/syntax_kind.rs:24-26`) but
offers no string→kind mapping, so a pattern language naming node kinds would need
one written.

### The single closest precedent

`crates/apex-binder/examples/unresolved_clusters.rs` — an ad-hoc, deliberately-kept
diagnostic (`cargo run -p apex-binder --release --example unresolved_clusters`,
`unresolved_clusters.rs:21`). It clusters every `Resolution::Unresolved` reference in
the NPSP corpus by a **structural fingerprint** and ranks clusters by frequency.

- Fingerprint = `Vec<SyntaxKind>`: the node's own kind plus up to
  `MAX_ANCESTOR_DEPTH = 4` ancestors (`unresolved_clusters.rs:34`), built by
  `fingerprint(&node)` walking `.parent()` (`unresolved_clusters.rs:56-70`).
- Climbing stops at a boundary kind — `Block | MethodDecl | ConstructorDecl |
  TriggerBlock | PropertyAccessor | ClassBody | InterfaceBody | CompilationUnit`
  (`unresolved_clusters.rs:40-54`) — "since two references that only share 'both live
  somewhere in a method body' aren't meaningfully the same shape."
- Clusters into an `FxHashMap<Vec<SyntaxKind>, (usize, Vec<Example>)>`, ranks by
  count, prints the top 40 with up to a few examples each
  (`unresolved_clusters.rs:100-134`).
- `line_snippet(text, range)` (`unresolved_clusters.rs:138-147`) — the trimmed source
  line containing a range, "enough for a human skimming the report to recognize the
  pattern without opening the file."
- Intent (`unresolved_clusters.rs:7-18`): "a genuine resolver *gap* … tends to
  produce a large, tight cluster of `Unresolved` references sharing one unusual
  structural shape … This turns 'eyeball the corpus until something looks wrong'
  into a ranked worklist."

That ancestor-kind-chain signature is the only shape-matching primitive in the repo,
and it is exactly what a structural matcher would generalize.

### The universal traversal idiom

`root.descendants().filter_map(TypedNode::cast)` — appears ~40+ times across
`crates/apex-binder/tests/*.rs` and once in production
(`crates/apexls/src/soql.rs:181`). There is no shared helper for it; each site
re-writes the chain.

---

## Crate dependency graph

Workspace members: `Cargo.toml:3-15` (11 crates plus `tools/salesforce-doc-scraper`,
`publish = false`, no internal dependencies).

```
apex-lexer      -> (memchr, phf)                              [no internal deps]
apex-discover   -> (ignore, phf, hotpath)                     [no internal deps]
apex-syntax     -> apex-lexer
apex-parser     -> apex-lexer, apex-syntax
apex-printer    -> apex-syntax
apex-metadata   -> apex-discover
apex-stdlib     -> apex-metadata
apex-binder     -> apex-syntax, apex-parser, apex-metadata, apex-stdlib, apex-discover
apexls-server   -> apex-binder, apex-discover, apex-lexer, apex-stdlib, apex-syntax
apexls          -> apex-binder, apex-discover, apex-parser, apex-printer, apex-syntax,
                   apexls-server
```

Manifests: `crates/apex-lexer/Cargo.toml:8-14`, `crates/apex-discover/Cargo.toml:7-10`,
`crates/apex-syntax/Cargo.toml:8-16`, `crates/apex-parser/Cargo.toml:8-10`,
`crates/apex-printer/Cargo.toml:7-11`, `crates/apex-metadata/Cargo.toml:7-11`,
`crates/apex-stdlib/Cargo.toml:7-11`, `crates/apex-binder/Cargo.toml:8-23`,
`crates/apexls-server/Cargo.toml:12-16`, `crates/apexls/Cargo.toml:12-21`.

**`crates/apexls` depends on BOTH `apex-binder` and `apexls-server`.**
`apex-binder.workspace = true` is the first `[dependencies]` entry
(`crates/apexls/Cargo.toml:12`); `apexls-server.workspace = true` is at
`crates/apexls/Cargo.toml:17`. `apexls-server` supplies the fix/diagnostic seams
(`diagnostics_for_file`, `resolve_fixes_for_file`, `LineIndex`, `run_server`);
`apex-binder` supplies `BoundProgram`, `BindCache`, `FileId` used directly in
`check.rs`/`fix.rs`.

`apexls` does **not** depend on `apex-lexer`, `apex-metadata`, `apex-stdlib`, or
`rowan` directly. It carries `clap` (derive), `rayon`, `tokio` (rt only), `mimalloc`
(`crates/apexls/Cargo.toml:18-21`); `mimalloc` is the global allocator
(`crates/apexls/src/main.rs:23-24`).

Release profile (`Cargo.toml:17-42`): `lto = "thin"`, `codegen-units = 1`,
`debug = "line-tables-only"`, `panic` deliberately left at `unwind` because
apexls-server is a long-running LSP process.
