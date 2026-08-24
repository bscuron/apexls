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
    serde_json::from_slice(&buf).unwrap()
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
                        "text": FOO_SRC,
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
