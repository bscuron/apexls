//! Regression tests for dynamic-SOQL bind-variable resolution
//! (`crate::resolve::bind_dynamic_soql_binds`): `Database.query`/
//! `countQuery`/`getQueryLocator`'s string argument gets scanned for
//! `:identifier` bind variables, which are then resolved against the
//! call site's local/parameter scope so goto-definition/find-references
//! work on them and `apex_binder::dead_code` no longer flags the bound
//! variable as unused.

use apex_binder::{BoundProgram, Resolution, SymbolKind, SyntaxPtr};

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

fn local_symbol_id(program: &BoundProgram, file: apex_binder::FileId, name: &str) -> apex_binder::SymbolId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.file == file && s.kind == SymbolKind::LocalVar && s.name == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("local `{name}` should have been collected"))
}

/// Every `Resolution` recorded anywhere inside `file`'s syntax tree at a
/// range that lies within a `StringLiteral`/`MultilineStringLiteral`
/// token -- the only way to observe a dynamic-SOQL bind's own recorded
/// resolution, since it has no real AST node of its own to look up via
/// the usual node-cast-then-SyntaxPtr::new pattern every other test file
/// in this suite uses. Scans every string token in the file and asks
/// `resolution_at` for each byte offset inside it; small and simple
/// rather than fast, which is fine for a test fixture.
fn bind_resolutions_in(program: &BoundProgram, file: apex_binder::FileId) -> Vec<Resolution> {
    let root = program.syntax(file);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in root.descendants_with_tokens().filter_map(|e| e.into_token()) {
        if !matches!(
            token.kind(),
            apex_syntax::SyntaxKind::StringLiteral | apex_syntax::SyntaxKind::MultilineStringLiteral
        ) {
            continue;
        }
        let range = token.text_range();
        let mut offset = range.start();
        while offset < range.end() {
            if let Some(res) = program.resolution_at(file, offset) {
                let ptr = SyntaxPtr::for_token(file, &token);
                if seen.insert((ptr, offset)) {
                    out.push(res.clone());
                }
            }
            offset += rowan::TextSize::from(1);
        }
    }
    out
}

#[test]
fn a_bind_variable_resolves_to_the_local_it_names() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            String q = 'SELECT Id FROM Account WHERE Name = :nameVar'; \
            Database.query(q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-basic", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = local_symbol_id(&program, file, "nameVar");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.contains(&Resolution::Resolved(name_var_id)),
        "expected :nameVar to resolve to the local it names: {resolutions:?}"
    );
}

#[test]
fn a_bind_variable_resolves_through_string_concatenation() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            String q = 'SELECT Id FROM Account ' + 'WHERE Name = :nameVar'; \
            Database.query(q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-concat", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = local_symbol_id(&program, file, "nameVar");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.contains(&Resolution::Resolved(name_var_id)),
        "a bind in the second half of a concatenated query string should still resolve: {resolutions:?}"
    );
}

#[test]
fn a_bind_variable_resolves_through_a_reassignment_before_the_call() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            String q; \
            q = 'SELECT Id FROM Account WHERE Name = :nameVar'; \
            Database.query(q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-reassign", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = local_symbol_id(&program, file, "nameVar");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.contains(&Resolution::Resolved(name_var_id)),
        "a plain reassignment (q = '...') before the call should still be traced: {resolutions:?}"
    );
}

#[test]
fn a_direct_literal_argument_resolves_with_no_variable_indirection() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            Database.query('SELECT Id FROM Account WHERE Name = :nameVar'); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-direct-literal", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = local_symbol_id(&program, file, "nameVar");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.contains(&Resolution::Resolved(name_var_id)),
        "a literal passed directly to Database.query should resolve its own binds: {resolutions:?}"
    );
}

#[test]
fn a_bind_naming_an_unresolvable_identifier_is_silently_skipped() {
    let src = "public class Foo { \
        public void run() { \
            String q = 'SELECT Id FROM Account WHERE Name = :totallyMadeUp'; \
            Database.query(q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-unknown-bind", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.is_empty(),
        "a bind naming nothing resolvable must not be recorded at all (never a guess): {resolutions:?}"
    );
}

/// A colon inside a string that doesn't look like SOQL at all (a URL)
/// must never be treated as a bind, even if the trailing text happens to
/// look like a real identifier that's genuinely in scope -- the
/// `looks_like_soql` guard exists precisely to avoid this.
#[test]
fn a_colon_in_a_non_soql_string_is_never_treated_as_a_bind() {
    let src = "public class Foo { \
        public void run() { \
            String url = 'http://host:port'; \
            Database.query(url); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-non-soql-string", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.is_empty(),
        "a non-SOQL-shaped string must never be scanned for binds: {resolutions:?}"
    );
}

/// `Database.queryWithBinds`'s bind names are keys in an explicit
/// `Map<String, Object>` argument, not names resolved against lexical
/// scope at all -- treating them the same way `query`'s plain-string
/// binds are treated would be a real correctness bug, not just an
/// over-broad heuristic, so this must never fire for it.
#[test]
fn query_with_binds_is_never_treated_as_a_lexically_scoped_bind() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            String q = 'SELECT Id FROM Account WHERE Name = :nameVar'; \
            Database.queryWithBinds(q, new Map<String, Object>{'nameVar' => nameVar}, AccessLevel.USER_MODE); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-with-binds", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.is_empty(),
        "queryWithBinds must not trigger lexical-scope bind resolution: {resolutions:?}"
    );
}

/// A field's own declared initializer is traced too, not just a local's
/// -- `private static final String Q = '...';` is a common real pattern.
#[test]
fn a_bind_resolves_through_a_field_declared_query_string() {
    let src = "public class Foo { \
        private static final String Q = 'SELECT Id FROM Account WHERE Name = :nameVar'; \
        public void run(String nameVar) { \
            Database.query(Q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-field-source", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = program
        .symbols
        .iter()
        .find(|(_, s)| s.file == file && s.kind == SymbolKind::Parameter && s.name == "nameVar")
        .map(|(id, _)| id)
        .expect("nameVar parameter should have been collected");
    let resolutions = bind_resolutions_in(&program, file);
    assert!(
        resolutions.contains(&Resolution::Resolved(name_var_id)),
        "a bind inside a field-declared query string should still resolve: {resolutions:?}"
    );
}

/// `goto-definition`'s actual entry point (`resolution_at`), not just the
/// lower-level per-token scan `bind_resolutions_in` uses -- proves a
/// click landing *inside* the string literal's text at the bind's own
/// position resolves, exactly what `capabilities::definition` needs.
#[test]
fn resolution_at_finds_a_bind_from_a_click_inside_the_string_literal() {
    let src = "public class Foo { \
        public void run() { \
            String nameVar = 'Acme'; \
            String q = 'SELECT Id FROM Account WHERE Name = :nameVar'; \
            Database.query(q); \
        } \
    }";
    let dir = write_fixture_dir("dynsoql-resolution-at", &[("Foo.cls", src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let name_var_id = local_symbol_id(&program, file, "nameVar");
    let offset = src.rfind(":nameVar").unwrap() + 2; // land inside "nameVar", not on the colon
    let resolution = program.resolution_at(file, rowan::TextSize::from(offset as u32));
    assert_eq!(resolution.cloned(), Some(Resolution::Resolved(name_var_id)));
}
