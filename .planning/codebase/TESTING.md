# Testing Patterns

**Analysis Date:** 2026-09-01

## Test Framework

**Runner:**
- Built-in Rust test harness (no external framework)
- Config: No `Cargo.toml` test section overrides; uses default `[[test]]` behavior
- Runs via `cargo test --workspace` in CI

**Assertion Library:**
- Standard `assert!`, `assert_eq!`, `assert_ne!` macros
- Custom assertion messages: `assert!(condition, "message with {} context", var)`
- No external crate dependency (assert_matches, proptest, etc.)

**Run Commands:**
```bash
cargo test --workspace              # Run all tests across all crates
cargo test -p apex-binder          # Run tests for specific crate
cargo test --lib                   # Library tests only (exclude integration tests)
cargo llvm-cov --workspace         # Generate coverage report (CI-only tool: cargo-llvm-cov)
```

## Test File Organization

**Location:** `crates/*/tests/` directories (integration tests)
- Parser tests: `crates/apex-parser/tests/*.rs` (10+ files)
- Binder tests: `crates/apex-binder/tests/*.rs` (30+ files, most comprehensive suite)
- Lexer tests: `crates/apex-lexer/tests/roundtrip.rs`
- Discover tests: `crates/apex-discover/tests/*.rs`
- Server tests: `crates/apexls-server/tests/*.rs`

**Note:** No `src/**/*.rs#[cfg(test)]` unit tests observed in exploration; all tests are integration tests in `tests/` directories. This keeps test code separate from library code.

**Naming:**
- Test files describe the feature/invariant they cover: `scope_resolution_smoke.rs`, `completion.rs`, `declaration_grammar_coverage.rs`, `sobject_constructor_field_init.rs`, `generics_resolution.rs`
- Each file focuses on one area of functionality, not one test function
- Within a file, `#[test]` functions named descriptively: `fn locals_and_params_are_all_visible_across_nested_scopes()`, `fn a_local_declared_after_the_cursor_is_not_yet_visible()`, `fn enclosing_types_direct_members_are_offered_bare()`

**Structure:**
```
crates/apex-binder/tests/
├── completion.rs                 # 200+ lines: fixture-based completion tests
├── scope_resolution_smoke.rs     # 150+ lines: corpus-level invariant validation
├── declaration_type_resolution.rs
├── ...30 other .rs files...
└── resolution_regression_baseline.rs
```

## Test Structure

**Suite Organization:**
- Each integration test file is independent (no shared test utilities across files)
- Fixtures created inline or via helper functions
- Real-world corpus tests (`NPSP`) used where available: `scope_resolution_smoke.rs`, `resolution_regression_baseline.rs`

**Common Pattern:**
```rust
// Fixture creation
fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

// Cursor marker helper (from completion.rs)
fn split_cursor(src: &str) -> (String, u32) {
    let idx = src.find('|').expect("fixture must contain a `|` cursor marker");
    let mut out = String::with_capacity(src.len() - 1);
    out.push_str(&src[..idx]);
    out.push_str(&src[idx + 1..]);
    (out, idx as u32)
}

#[test]
fn test_name_describes_behavior() {
    let (src, offset) = split_cursor("source with |cursor marker here");
    let dir = write_fixture_dir("test-name", &[("File.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();
    
    // assertions
    assert!(program.symbols.iter().any(|(_, s)| s.name == "expected"));
}
```

**Example from `completion.rs` (lines 45-85):**
```rust
#[test]
fn locals_and_params_are_all_visible_across_nested_scopes() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run(Integer x) { \
                 Integer y = 0; \
                 if (true) { Integer z = 0; |} \
             } \
         }",
    );
    let dir = write_fixture_dir("completion-locals-nested", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let locals: Vec<_> = ctx
        .candidates
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                CompletionCandidateKind::Local | CompletionCandidateKind::Parameter
            )
        })
        .map(|c| c.label.as_str())
        .collect();

    assert!(
        locals.contains(&"x"),
        "param `x` should be visible: {locals:?}"
    );
    assert!(
        locals.contains(&"y"),
        "outer local `y` should be visible: {locals:?}"
    );
    assert!(
        locals.contains(&"z"),
        "innermost local `z` should be visible: {locals:?}"
    );
}
```

**Patterns:**
- **Setup:** Fixture creation (temp dir with Apex source files), marker-based offset parsing
- **Teardown:** `std::fs::remove_dir_all(&dir).ok()` (ignore errors if already cleaned)
- **Assertion:** Multi-line assertions with context in failure messages: `assert!(condition, "message: {details:?}")`
- **Collection filtering:** `.iter().filter(...).map(...).collect()` for extracting relevant results

## Mocking

**Framework:** No mocking framework (no Mockito, no `mock!` macros)

**Patterns:**
- Direct fixture creation with real file I/O (temp directories)
- Corpus-based testing (`tests/corpus/npsp/` submodule): real Salesforce code used as test input
- No trait mocking; tests work with concrete types

**What to Mock:** Nothing in current codebase
- File I/O replaced with temp dirs (not mocked via trait)
- All external services (Salesforce schema) provided via data files, not network calls during tests

**What NOT to Mock:**
- Parser output: tests parse real Apex and check the tree
- Binder resolution: tests bind whole projects and inspect symbol tables
- Even "slow" operations (corpus parsing) are tested directly, not stubbed

## Fixtures and Factories

**Test Data:**
- Inline strings (multi-line): `"public class Foo { public Integer x; }"` embedded in test function
- Fixture directory pattern: create temp dir, write files, run analysis, delete dir
- Cursor marker pattern: `|` in source marks the position for LSP queries like completion/goto-def

**Example pattern from `completion.rs`:**
```rust
fn split_cursor(src: &str) -> (String, u32) {
    let idx = src.find('|').expect("fixture must contain a `|` cursor marker");
    let mut out = String::with_capacity(src.len() - 1);
    out.push_str(&src[..idx]);
    out.push_str(&src[idx + 1..]);
    (out, idx as u32)
}
```

**Location:**
- Helper functions defined at module level in each test file (not shared across files)
- Corpus root typically `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")`
- Real NPSP corpus must be checked out via git submodule for corpus-based tests to run

**Examples:**
- `crates/apex-binder/tests/completion.rs`: `write_fixture_dir()`, `split_cursor()`, `file_for_class()`, `labels_of()` helpers
- `crates/apex-lexer/tests/roundtrip.rs`: `collect_apex_files()` recursive directory walk
- `crates/apex-parser/tests/declaration_grammar_coverage.rs`: `assert_round_trips()` helper validates lossless parsing

## Coverage

**Requirements:** None explicitly enforced per crate
- Coverage report generated in CI via `cargo llvm-cov --workspace --ignore-run-fail --html`
- Report uploaded as artifact to GitHub Actions
- No coverage gate (% minimum not enforced)

**View Coverage:**
```bash
cargo llvm-cov --workspace --ignore-run-fail --html
# Opens: target/llvm-cov/html/index.html in browser
```

**Strategy observed:**
- Parser: round-trip + grammar coverage for every declaration/statement form
- Binder: invariant tests on real corpus (scope resolution, reference counts, overload resolution)
- No unit-test coverage obsession; focus on meaningful invariants

## Test Types

**Unit Tests:**
- Rare (no `#[cfg(test)] mod tests` blocks observed)
- Focus is on integration tests

**Integration Tests:**
- **Scope**: Whole project or multi-file scenarios
- **Approach**: Fixture creation → operation → invariant check
- **Duration**: Most run in <1s; corpus tests (~NPSP) take 10-30s depending on machine
- Examples: `completion.rs`, `scope_resolution_smoke.rs`, `declaration_grammar_coverage.rs`

**Corpus Tests:**
- Real-world test data: NPSP Salesforce package (`tests/corpus/npsp/` submodule)
- Used for whole-program invariant validation: "every real NPSP file binds and resolves a meaningful share of references"
- Metrics: counts of resolution types (Resolved, Candidates, SchemaObject, UnknownSchema, etc.)
- Baselines: target floor counts updated when grammar/binder changes justify it

**E2E Tests:**
- LSP server protocol tests: `crates/apexls-server/tests/*.rs`
- Real JSON-RPC messages, not just library function calls
- End-to-end: source code → LSP request → response parsing

## Common Patterns

**Async Testing:**
- No async tests observed in codebase (Apex language is synchronous, single-threaded model)
- Binder uses `rayon` for parallelism internally, but test layer is synchronous

**Error Testing:**
```rust
#[test]
fn malformed_declarations_report_the_expected_error() {
    let cases: &[(&str, &str)] = &[
        ("@Foo(x=bogus) public class Bar { }", "expected a literal value"),
        ("public class Foo extends { }", "expected a type after 'extends'"),
        // ...more cases...
    ];
    for (src, expected_message) in cases {
        let parse = parse_compilation_unit(src);
        assert!(
            parse.errors.iter().any(|e| e.message.contains(expected_message)),
            "{src:?}: expected an error containing {expected_message:?}, got {:?}",
            parse.errors
        );
    }
}
```

**Corpus Validation:**
```rust
#[test]
fn every_real_npsp_file_binds_and_resolves_a_meaningful_share_of_references() {
    let root = corpus_root();
    assert!(root.exists(), "no NPSP corpus found...");
    let program = BoundProgram::from_files(&root);
    
    let mut counts = Counts::default();
    for (_, resolution) in program.all_resolutions() {
        match resolution {
            Resolution::Resolved(_) => counts.resolved += 1,
            // ...match all variants...
        }
    }
    
    assert!(counts.resolved > 100_000, "expected >100,000 Resolved references...");
    // ...more bounds...
}
```

**Round-trip Testing (Lexer/Parser):**
```rust
#[test]
fn tokenizing_npsp_round_trips_exactly() {
    let root = corpus_root();
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else { continue; };
        let rebuilt: String = apex_lexer::tokenize(&source)
            .into_iter()
            .map(|t| t.text(&source))
            .collect();
        assert_eq!(rebuilt, source, "did not round-trip");
    }
}
```

## Test Coverage Gaps

**Known gaps (not tested, low priority for v1):**
- SOQL/SOSL completion and resolution (by design, v1 scope excludes this)
- Hover documentation formatting on non-ASCII characters (edge case)
- Very large files (stack safety is tested, but not performance profiles)
- Incremental re-binding edge cases (cache invalidation tested, but not all permutation sequences)

---

*Testing analysis: 2026-09-01*
