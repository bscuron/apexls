//! Protocol-level verification of `textDocument/references` and
//! `textDocument/documentHighlight` (`BACKLOG.md` §3). Follows
//! `hover_definition.rs`'s exact pattern (spawn the real binary, drive
//! it over real stdio, wait for the background rebuild's "rebuild
//! complete" stderr line before sending a position-based request).

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

/// `needle`'s (line, character) position in `src`, both 0-based -- lets a
/// test's assertions stay in sync with fixture source without hand-
/// counting characters (same idea as `schema_goto_definition.rs`'s
/// `position_of`). Returns the position of `needle`'s *first* byte;
/// callers pick a `needle` precise enough (e.g. a whole identifier) that
/// this lands unambiguously where intended.
fn position_of(src: &str, needle: &str) -> (u32, u32) {
    let byte_offset = src
        .find(needle)
        .expect("needle not found in fixture source");
    let before = &src[..byte_offset];
    let line = before.matches('\n').count() as u32;
    let character = match before.rfind('\n') {
        Some(last_newline) => (byte_offset - last_newline - 1) as u32,
        None => byte_offset as u32,
    };
    (line, character)
}

struct Session {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    rebuild_rx: mpsc::Receiver<String>,
}

impl Session {
    /// Starts the server against `root_uri` (every `.cls` file already
    /// written to `root_uri`'s directory gets bound, whether or not it's
    /// individually opened) and additionally opens `open_uri`/`open_text`
    /// via `didOpen`, matching how a real editor session begins.
    fn start(root_uri: &Url, open_uri: &Url, open_text: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn apexls-server");

        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
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
                        "uri": open_uri,
                        "languageId": "apex",
                        "version": 1,
                        "text": open_text,
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

const BASE_SRC: &str = "public class Base {\n    public void greet() { }\n}\n";
const CALLER_A_SRC: &str =
    "public class CallerA {\n    public void run() { new Base().greet(); }\n}\n";
const CALLER_B_SRC: &str =
    "public class CallerB {\n    public void run() { new Base().greet(); }\n}\n";

#[test]
fn references_excludes_the_declaration_by_default_but_crosses_files() {
    let dir = write_fixture_dir(
        "references-cross-file",
        &[
            ("Base.cls", BASE_SRC),
            ("CallerA.cls", CALLER_A_SRC),
            ("CallerB.cls", CALLER_B_SRC),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let base_uri = Url::from_file_path(dir.join("Base.cls")).unwrap();

    let mut session = Session::start(&root_uri, &base_uri, BASE_SRC);

    let (line, character) = position_of(BASE_SRC, "greet");
    let response = session.request(
        2,
        "textDocument/references",
        serde_json::json!({
            "textDocument": { "uri": base_uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": false },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "references returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a Location array, got {response:?}"));
    assert_eq!(
        result.len(),
        2,
        "expected exactly 2 references (CallerA + CallerB), declaration excluded: {result:?}"
    );
    let uris: Vec<&str> = result.iter().map(|l| l["uri"].as_str().unwrap()).collect();
    assert!(
        !uris.contains(&base_uri.as_str()),
        "declaration site must not appear when includeDeclaration is false: {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn references_includes_the_declaration_when_requested() {
    let dir = write_fixture_dir(
        "references-include-decl",
        &[
            ("Base.cls", BASE_SRC),
            ("CallerA.cls", CALLER_A_SRC),
            ("CallerB.cls", CALLER_B_SRC),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let base_uri = Url::from_file_path(dir.join("Base.cls")).unwrap();

    let mut session = Session::start(&root_uri, &base_uri, BASE_SRC);

    let (line, character) = position_of(BASE_SRC, "greet");
    let response = session.request(
        2,
        "textDocument/references",
        serde_json::json!({
            "textDocument": { "uri": base_uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true },
        }),
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a Location array, got {response:?}"));
    assert_eq!(
        result.len(),
        3,
        "expected declaration + 2 references when includeDeclaration is true: {result:?}"
    );
    let uris: Vec<&str> = result.iter().map(|l| l["uri"].as_str().unwrap()).collect();
    assert!(
        uris.contains(&base_uri.as_str()),
        "declaration site must appear when includeDeclaration is true: {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn references_reports_only_the_method_name_range_not_the_whole_call() {
    // Regression test: a `MethodCallExpr` reference used to be keyed by
    // (and reported at) its *whole* node range -- target through closing
    // paren -- so `references`/`documentHighlight` on `greet` returned a
    // range spanning all of `new Base().greet()` instead of just the
    // `greet` token.
    let dir = write_fixture_dir(
        "references-method-name-range",
        &[
            ("Base.cls", BASE_SRC),
            ("CallerA.cls", CALLER_A_SRC),
            ("CallerB.cls", CALLER_B_SRC),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let base_uri = Url::from_file_path(dir.join("Base.cls")).unwrap();
    let caller_a_uri = Url::from_file_path(dir.join("CallerA.cls")).unwrap();

    let mut session = Session::start(&root_uri, &base_uri, BASE_SRC);

    let (line, character) = position_of(BASE_SRC, "greet");
    let response = session.request(
        2,
        "textDocument/references",
        serde_json::json!({
            "textDocument": { "uri": base_uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": false },
        }),
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a Location array, got {response:?}"));
    let caller_a_loc = result
        .iter()
        .find(|l| l["uri"].as_str() == Some(caller_a_uri.as_str()))
        .unwrap_or_else(|| panic!("expected a reference in CallerA.cls: {result:?}"));

    let (want_line, want_character) = position_of(CALLER_A_SRC, "greet");
    let start = &caller_a_loc["range"]["start"];
    let end = &caller_a_loc["range"]["end"];
    assert_eq!(
        (start["line"].as_u64(), start["character"].as_u64()),
        (Some(want_line as u64), Some(want_character as u64)),
        "range should start at the `greet` token, not the whole call: {caller_a_loc:?}"
    );
    assert_eq!(
        (end["line"].as_u64(), end["character"].as_u64()),
        (Some(want_line as u64), Some(want_character as u64 + "greet".len() as u64)),
        "range should end at the `greet` token's own end, not the call's closing paren: {caller_a_loc:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn document_highlight_finds_every_occurrence_of_a_local_in_one_file() {
    const SRC: &str = "public class Widget {\n    \
         public void run() {\n        \
         Integer count = 0;\n        \
         count = count + 1;\n    \
     }\n}\n";
    let dir = write_fixture_dir("highlight-local", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);

    // Land on the declaration's own name, `Integer count = 0;`'s `count`.
    let (line, character) = position_of(SRC, "count = 0");
    let response = session.request(
        2,
        "textDocument/documentHighlight",
        serde_json::json!({
            "textDocument": { "uri": widget_uri },
            "position": { "line": line, "character": character },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "documentHighlight returned an error: {response:?}"
    );
    let result = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a DocumentHighlight array, got {response:?}"));
    // Declaration + `count = count + 1;`'s two occurrences of `count`.
    assert_eq!(
        result.len(),
        3,
        "expected the declaration plus both occurrences on the next line: {result:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
