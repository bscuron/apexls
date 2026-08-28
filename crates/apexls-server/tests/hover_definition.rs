//! Protocol-level verification of `BACKLOG.md` §3's `textDocument/hover`
//! and `textDocument/definition` (the bind's first real consumers),
//! plus the four "buildable now, no new binder work needed" capabilities
//! that came right after: `textDocument/documentSymbol`, `workspace/symbol`,
//! `textDocument/foldingRange`, `textDocument/selectionRange`. Follows
//! `binder_integration.rs`'s exact pattern (spawn the real binary, drive
//! it over real stdio, wait for the background rebuild's "rebuild
//! complete" stderr line before sending a position-based request --
//! there's no other externally observable "the bind is ready" signal
//! yet).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use lsp_types::Url;

fn send(stdin: &mut impl Write, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stdin.flush().unwrap();
}

fn recv(stdout: &mut impl BufRead) -> serde_json::Value {
    loop {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let n = stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "server closed stdout before sending a full response");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                content_length = Some(value.parse::<usize>().unwrap());
            }
        }
        let content_length = content_length.expect("response had no Content-Length header");
        let mut buf = vec![0u8; content_length];
        stdout.read_exact(&mut buf).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        // A server-initiated notification (e.g. textDocument/publishDiagnostics,
        // pushed proactively after every rebuild) has a `method` but no `id` --
        // skip past it rather than mistaking it for the response a caller is
        // actually waiting for.
        if value.get("id").is_some() {
            return value;
        }
    }
}

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apexls-server-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

/// `public class Foo {` / `    public Integer value;` / `    public void run() {` /
/// `        Integer y = value;` / `    }` / `}` -- kept as an explicit
/// multi-line string (not a one-liner) so this test's hand-picked
/// line/character positions stay easy to verify by inspection.
const FOO_SRC: &str = "public class Foo {\n    public Integer value;\n    public void run() {\n        Integer y = value;\n    }\n}\n";

struct Session {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    rebuild_rx: mpsc::Receiver<String>,
}

impl Session {
    fn start(foo_uri: &Url, root_uri: &Url) -> Self {
        Self::start_with_text(foo_uri, root_uri, FOO_SRC)
    }

    fn start_with_text(foo_uri: &Url, root_uri: &Url, text: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn apexls-server");

        let mut stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();

        let (tx, rebuild_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        send(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": null,
                    "rootUri": null,
                    "capabilities": {},
                    "workspaceFolders": [{ "uri": root_uri, "name": "fixture" }],
                }
            }),
        );
        let mut stdout = stdout;
        let response = recv(&mut stdout);
        assert!(
            response.get("error").is_none(),
            "initialize returned an error: {response:?}"
        );

        send(
            &mut stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        );
        send(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": foo_uri,
                        "languageId": "apex",
                        "version": 1,
                        "text": text,
                    }
                }
            }),
        );

        let mut session = Session {
            child,
            stdin,
            stdout,
            rebuild_rx,
        };
        session.wait_for_rebuild();
        session
    }

    fn wait_for_rebuild(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut saw_rebuild = false;
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self
                .rebuild_rx
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                Ok(line) if line.contains("rebuild complete") => {
                    saw_rebuild = true;
                    break;
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(
            saw_rebuild,
            "expected a \"rebuild complete\" line on stderr within 10s"
        );
    }

    fn request(&mut self, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        recv(&mut self.stdout)
    }

    fn shutdown(mut self) {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null }),
        );
        let response = recv(&mut self.stdout);
        assert!(
            response.get("error").is_none(),
            "shutdown returned an error: {response:?}"
        );
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
        );
        let status = self.child.wait().expect("failed to wait on apexls-server");
        assert!(
            status.success(),
            "apexls-server did not exit cleanly after exit: {status:?}"
        );
    }
}

/// Line 3, character 22 -- inside the *reference* to `value` in
/// `Integer y = value;` (see `FOO_SRC`'s doc comment for the exact
/// layout: 8-space indent, "Integer y = " is 12 chars, landing squarely
/// inside "value").
const VALUE_REFERENCE_POSITION: (u32, u32) = (3, 22);

#[test]
fn hover_on_a_field_reference_shows_its_declared_type() {
    let dir = write_fixture_dir("hover", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": { "line": VALUE_REFERENCE_POSITION.0, "character": VALUE_REFERENCE_POSITION.1 },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "hover returned an error: {response:?}"
    );
    let contents = response["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hover contents.value, got {response:?}"));
    assert!(
        contents.contains("value"),
        "hover text should mention `value`: {contents}"
    );
    assert!(
        contents.contains("Integer"),
        "hover text should mention the declared type `Integer`: {contents}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` /
/// `        Boolean b = String.isBlank('x');` / `    }` / `}` -- "isBlank"
/// spans characters 27-33 on line 2, so character 30 lands squarely
/// inside it.
const STDLIB_CALL_SRC: &str =
    "public class Foo {\n    public void run() {\n        Boolean b = String.isBlank('x');\n    }\n}\n";
const STDLIB_CALL_POSITION: (u32, u32) = (2, 30);

/// End-to-end confirmation that `apex_stdlib`'s bundled standard-library
/// schema (wired into `crate::resolve`'s `Ty::System` arms, then
/// `capabilities::describe_stdlib_member`) actually reaches a real
/// `textDocument/hover` response -- not just the underlying
/// `Resolution::StdlibMember` outcome `standard_library_resolution.rs`
/// (in `apex-binder`) already covers.
#[test]
fn hover_on_a_real_stdlib_method_call_shows_its_signature_and_description() {
    let dir = write_fixture_dir("hover-stdlib", &[("Foo.cls", STDLIB_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_CALL_SRC);

    let response = session.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": { "line": STDLIB_CALL_POSITION.0, "character": STDLIB_CALL_POSITION.1 },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "hover returned an error: {response:?}"
    );
    let contents = response["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hover contents.value, got {response:?}"));
    assert!(
        contents.contains("isBlank"),
        "hover text should mention `isBlank`: {contents}"
    );
    assert!(
        contents.contains("Boolean"),
        "hover text should mention the real return type `Boolean`: {contents}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` /
/// `        System.debug(LoggingLevel.INFO, 'hi');` / `    }` / `}` --
/// "debug" spans characters 15-19 on line 2, so character 17 lands
/// inside it. `System.debug` is a real, confirmed 2-way overload
/// (`debug(Object)` and `debug(LoggingLevel, Object)`) -- a 2-argument
/// call should narrow hover to just the 2-arg overload's own signature
/// and description, not show both indiscriminately.
const STDLIB_OVERLOADED_CALL_SRC: &str =
    "public class Foo {\n    public void run() {\n        System.debug(LoggingLevel.INFO, 'hi');\n    }\n}\n";
const STDLIB_OVERLOADED_CALL_POSITION: (u32, u32) = (2, 17);

#[test]
fn hover_on_an_overloaded_stdlib_call_narrows_to_the_matching_arity() {
    let dir = write_fixture_dir("hover-stdlib-overload", &[("Foo.cls", STDLIB_OVERLOADED_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_OVERLOADED_CALL_SRC);

    let response = session.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": { "line": STDLIB_OVERLOADED_CALL_POSITION.0, "character": STDLIB_OVERLOADED_CALL_POSITION.1 },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "hover returned an error: {response:?}"
    );
    let contents = response["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hover contents.value, got {response:?}"));

    // Narrowed to the 2-arg overload only: its own signature (with the
    // `LoggingLevel` parameter) and its own description ("...with the
    // specified log level.") should appear, but the 1-arg overload's
    // signature/description should not.
    assert!(
        contents.contains("LoggingLevel"),
        "hover should show the 2-arg overload's LoggingLevel parameter: {contents}"
    );
    assert!(
        !contents.contains("LoggingLevel Enum"),
        "the scraped \"LoggingLevel Enum\" cross-reference artifact should be stripped to just \
         \"LoggingLevel\": {contents}"
    );
    assert!(
        contents.contains("specified log level"),
        "hover should show the 2-arg overload's own description: {contents}"
    );
    assert!(
        !contents.contains("debug(Object"),
        "hover should NOT also show the 1-arg overload's signature once arity narrows to one \
         match: {contents}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    private List<SObject> objectsToInsert = new List<SObject>();` /
/// `    public void run(List<SObject> sObjects) {` /
/// `        objectsToInsert.addAll(sObjects);` / `    }` / `}` -- character
/// 26 on line 3 lands inside "addAll". `List.addAll` is a real, confirmed
/// *same-arity* overload pair (`addAll(List)`/`addAll(Set)`, both one
/// parameter) -- arity alone can't tell them apart the way `System.debug`'s
/// differently-arity pair above can, so hovering a `List`-typed receiver's
/// call must narrow by the receiver's real argument *type* to just the
/// `List` overload, not show both (List/Set aren't implicitly
/// convertible, so showing the `Set` one too was actively misleading).
const STDLIB_SAME_ARITY_OVERLOAD_CALL_SRC: &str = "public class Foo {\n    private List<SObject> objectsToInsert = new List<SObject>();\n    public void run(List<SObject> sObjects) {\n        objectsToInsert.addAll(sObjects);\n    }\n}\n";
const STDLIB_SAME_ARITY_OVERLOAD_CALL_POSITION: (u32, u32) = (3, 26);

#[test]
fn hover_on_a_same_arity_overloaded_stdlib_call_narrows_by_argument_type() {
    let dir = write_fixture_dir(
        "hover-stdlib-same-arity-overload",
        &[("Foo.cls", STDLIB_SAME_ARITY_OVERLOAD_CALL_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_SAME_ARITY_OVERLOAD_CALL_SRC);

    let response = session.request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": {
                "line": STDLIB_SAME_ARITY_OVERLOAD_CALL_POSITION.0,
                "character": STDLIB_SAME_ARITY_OVERLOAD_CALL_POSITION.1,
            },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "hover returned an error: {response:?}"
    );
    let contents = response["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected hover contents.value, got {response:?}"));

    assert!(
        contents.contains("addAll(List"),
        "hover should show the List overload's own signature: {contents}"
    );
    assert!(
        !contents.contains("addAll(Set"),
        "hover should NOT also show the Set overload once the argument's real List type narrows \
         it out: {contents}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_on_a_field_reference_points_back_at_its_declaration() {
    let dir = write_fixture_dir("definition", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "textDocument/definition",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": { "line": VALUE_REFERENCE_POSITION.0, "character": VALUE_REFERENCE_POSITION.1 },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "definition returned an error: {response:?}"
    );
    let result = &response["result"];
    // A single resolved candidate -> `GotoDefinitionResponse::Scalar`, a
    // bare `Location` object (not wrapped in an array).
    let uri = result["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a scalar Location, got {response:?}"));
    assert_eq!(uri, foo_uri.as_str());
    // Line 1 is `    public Integer value;` -- the field's own
    // declaration line.
    assert_eq!(result["range"]["start"]["line"], 1);

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn document_symbol_returns_the_class_nesting_its_field_and_method() {
    let dir = write_fixture_dir("document-symbol", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({ "textDocument": { "uri": foo_uri } }),
    );
    assert!(
        response.get("error").is_none(),
        "documentSymbol returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a symbol array, got {response:?}"));
    assert_eq!(result.len(), 1, "expected one top-level symbol (Foo)");
    let foo = &result[0];
    assert_eq!(foo["name"], "Foo");
    let children = foo["children"]
        .as_array()
        .unwrap_or_else(|| panic!("expected Foo to have children, got {foo:?}"));
    let names: Vec<&str> = children
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"value"), "children should include `value`: {names:?}");
    assert!(names.contains(&"run"), "children should include `run`: {names:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A doc comment ends up nested *inside* its declaration's own CST node
/// (leading trivia on the node's first real child -- see
/// `apex_syntax::ast::decl::HasDocComment`'s doc comment), so the node's
/// raw range starts at the comment, not the declaration. Jumping to a
/// `documentSymbol` entry used to land there instead of on the method
/// itself.
#[test]
fn document_symbol_range_starts_after_a_leading_doc_comment_not_at_it() {
    let src = "public class Foo {\n    /**\n     * Does the thing.\n     */\n    public void run() {\n    }\n}\n";
    let dir = write_fixture_dir("document-symbol-doc-comment", &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start_with_text(&foo_uri, &root_uri, src);

    let response = session.request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({ "textDocument": { "uri": foo_uri } }),
    );
    assert!(
        response.get("error").is_none(),
        "documentSymbol returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a symbol array, got {response:?}"));
    let run = result[0]["children"]
        .as_array()
        .and_then(|children| children.iter().find(|c| c["name"] == "run"))
        .unwrap_or_else(|| panic!("expected a `run` child symbol, got {result:?}"));
    // Line 4 is `    public void run() {` -- the method's own
    // declaration line, not line 1 where the doc comment starts.
    assert_eq!(
        run["range"]["start"]["line"], 4,
        "range should start at `run`'s own declaration, not its doc comment: {run:?}"
    );
    assert_eq!(run["selectionRange"]["start"]["line"], 4);

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_symbol_finds_a_method_by_substring() {
    let dir = write_fixture_dir("workspace-symbol", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "workspace/symbol",
        serde_json::json!({ "query": "ru" }),
    );
    assert!(
        response.get("error").is_none(),
        "workspace/symbol returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a symbol array, got {response:?}"));
    assert!(
        result.iter().any(|s| s["name"] == "run"),
        "expected `run` among workspace/symbol results for query \"ru\": {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folding_range_covers_the_class_body_and_the_method_body() {
    let dir = write_fixture_dir("folding-range", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "textDocument/foldingRange",
        serde_json::json!({ "textDocument": { "uri": foo_uri } }),
    );
    assert!(
        response.get("error").is_none(),
        "foldingRange returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a folding range array, got {response:?}"));
    // The class body (lines 0-5) and `run`'s block (lines 2-4) are both
    // multi-line braced regions -- both should fold.
    assert!(
        result.iter().any(|r| r["startLine"] == 0),
        "expected a folding range starting at the class body's opening brace: {result:?}"
    );
    assert!(
        result.iter().any(|r| r["startLine"] == 2),
        "expected a folding range starting at `run`'s opening brace: {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selection_range_expands_from_the_reference_outward() {
    let dir = write_fixture_dir("selection-range", &[("Foo.cls", FOO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&foo_uri, &root_uri);

    let response = session.request(
        2,
        "textDocument/selectionRange",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "positions": [
                { "line": VALUE_REFERENCE_POSITION.0, "character": VALUE_REFERENCE_POSITION.1 },
            ],
        }),
    );
    assert!(
        response.get("error").is_none(),
        "selectionRange returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a selection range array, got {response:?}"));
    assert_eq!(result.len(), 1, "expected one selection range per requested position");
    // The innermost range (`value`, the NameExpr) must itself expand to
    // at least one real parent (the statement/block/method/class
    // ancestry) -- a chain of length 1 would mean nothing but the token
    // itself was ever returned.
    assert!(
        result[0]["parent"].is_object(),
        "expected the innermost selection range to have a parent: {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
