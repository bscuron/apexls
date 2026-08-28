//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! real syntax errors -- rust-analyzer-style red-squiggle diagnostics
//! from `apex_parser::ParseError`, not just the pre-existing dead-code
//! `WARNING`s. Follows `dead_code_diagnostics.rs`'s exact `Session`
//! harness (including its `recv_notification`/`next_diagnostics` helpers
//! for observing the server-initiated push rather than a request/response
//! pair).

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

fn read_frame(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout before sending a full frame");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.parse::<usize>().unwrap());
        }
    }
    let content_length = content_length.expect("frame had no Content-Length header");
    let mut buf = vec![0u8; content_length];
    stdout.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

fn recv(stdout: &mut impl BufRead) -> serde_json::Value {
    loop {
        let value = read_frame(stdout);
        if value.get("id").is_some() {
            return value;
        }
    }
}

fn recv_notification(stdout: &mut impl BufRead, method: &str) -> serde_json::Value {
    loop {
        let value = read_frame(stdout);
        if value.get("id").is_none() && value.get("method").and_then(|m| m.as_str()) == Some(method) {
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

struct Session {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    rebuild_rx: mpsc::Receiver<String>,
}

impl Session {
    fn start(root_uri: &Url, open_uri: &Url, open_text: &str) -> Self {
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

        let mut session = Session {
            child,
            stdin,
            stdout,
            rebuild_rx,
        };
        let response = recv(&mut session.stdout);
        assert!(response.get("error").is_none(), "initialize returned an error: {response:?}");

        send(
            &mut session.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        );
        send(
            &mut session.stdin,
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
        session.wait_for_rebuild();
        session
    }

    fn wait_for_rebuild(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut saw_rebuild = false;
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self.rebuild_rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Ok(line) if line.contains("rebuild complete") => {
                    saw_rebuild = true;
                    break;
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_rebuild, "expected a \"rebuild complete\" line on stderr within 10s");
    }

    fn did_change(&mut self, uri: &Url, version: i64, new_text: &str) {
        send(
            &mut self.stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": new_text }],
                }
            }),
        );
        self.wait_for_rebuild();
    }

    fn next_diagnostics(&mut self) -> serde_json::Value {
        recv_notification(&mut self.stdout, "textDocument/publishDiagnostics")
    }

    fn shutdown(mut self) {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null }),
        );
        let response = recv(&mut self.stdout);
        assert!(response.get("error").is_none(), "shutdown returned an error: {response:?}");
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
        );
        let status = self.child.wait().expect("failed to wait on apexls-server");
        assert!(status.success(), "apexls-server did not exit cleanly after exit: {status:?}");
    }
}

/// `public class Foo { public void run() { foo(1, 2; } }` -- a call
/// missing its closing paren, a real, common typo-shaped syntax error.
const MISSING_CLOSE_PAREN_SRC: &str =
    "public class Foo {\n    public void run() {\n        foo(1, 2;\n    }\n}\n";
const FIXED_SRC: &str = "public class Foo {\n    public void run() {\n        foo(1, 2);\n    }\n}\n";

#[test]
fn a_real_syntax_error_gets_an_error_severity_diagnostic() {
    let dir = write_fixture_dir("syntax-error-diagnostics", &[("Foo.cls", MISSING_CLOSE_PAREN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, MISSING_CLOSE_PAREN_SRC);

    let notification = session.next_diagnostics();
    assert_eq!(notification["params"]["uri"], serde_json::json!(foo_uri));
    let diagnostics = notification["params"]["diagnostics"].as_array().unwrap();
    // Filtered to ERROR severity, not asserted as the *only* diagnostic:
    // `run`'s own body is unrelated to whether it has a real caller, and
    // this fixture (deliberately minimal, no second file) leaves it
    // legitimately dead-code-flagged too -- irrelevant to what this test
    // actually checks.
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["severity"] == serde_json::json!(1))
        .collect();
    assert_eq!(errors.len(), 1, "expected exactly one ERROR-severity diagnostic: {diagnostics:?}");
    let diagnostic = errors[0];
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    let message = diagnostic["message"].as_str().unwrap();
    assert!(
        message.contains("expected") && message.contains("RParen"),
        "expected a parser 'expected ..., found ...' style message: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixing_the_error_clears_the_diagnostic_on_the_next_publish() {
    let dir = write_fixture_dir("syntax-error-clears", &[("Foo.cls", MISSING_CLOSE_PAREN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, MISSING_CLOSE_PAREN_SRC);

    let has_error = |v: &serde_json::Value| {
        v["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == serde_json::json!(1))
    };

    let first = session.next_diagnostics();
    assert!(has_error(&first), "expected the initial broken file to have an ERROR diagnostic: {first:?}");

    session.did_change(&foo_uri, 2, FIXED_SRC);
    let second = session.next_diagnostics();
    assert_eq!(second["params"]["uri"], serde_json::json!(foo_uri));
    assert!(
        !has_error(&second),
        "fixing the syntax error should clear its ERROR diagnostic, not leave a stale squiggle: {second:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The exact shape a real user reported: a missing semicolon after a
/// call, immediately followed by another statement on the next line --
/// `Parser::expect`'s failure used to be positioned at the start of the
/// *next* statement (`this` on line 3), which reads as "this next
/// statement is wrong" even though it isn't; the missing semicolon
/// belongs at the end of line 2, right after the call it's missing from.
const MISSING_SEMICOLON_BEFORE_NEXT_STATEMENT_SRC: &str = "public class Foo {\n    public void run() {\n        this.populateAvailableFields()\n        this.populateSoftCredits();\n    }\n}\n";

#[test]
fn a_missing_semicolon_is_reported_at_the_end_of_the_statement_missing_it_not_the_next_one() {
    let dir = write_fixture_dir(
        "syntax-error-gap-position",
        &[("Foo.cls", MISSING_SEMICOLON_BEFORE_NEXT_STATEMENT_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, MISSING_SEMICOLON_BEFORE_NEXT_STATEMENT_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = notification["params"]["diagnostics"].as_array().unwrap();
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["severity"] == serde_json::json!(1))
        .collect();
    assert_eq!(errors.len(), 1, "expected exactly one ERROR-severity diagnostic: {diagnostics:?}");
    let range = &errors[0]["range"];
    assert_eq!(
        range["start"]["line"], serde_json::json!(2),
        "expected the diagnostic on line 2 (end of `populateAvailableFields()`), not line 3 \
         (start of the next statement): {range:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real syntax error *and* a genuinely dead private method in the same
/// file -- confirms `publish_diagnostics` actually merges both sources
/// into one notification rather than one silently replacing the other
/// (`textDocument/publishDiagnostics` replaces a client's whole
/// diagnostic set for a URI on every notification, so sending two
/// separate notifications for the same file would be a real bug).
const SYNTAX_ERROR_AND_DEAD_CODE_SRC: &str =
    "public class Foo {\n    private void deadHelper() { }\n    public void run() { foo(1, 2; }\n}\n";

#[test]
fn a_syntax_error_and_a_dead_symbol_both_appear_in_the_same_publish() {
    let dir = write_fixture_dir(
        "syntax-error-and-dead-code",
        &[("Foo.cls", SYNTAX_ERROR_AND_DEAD_CODE_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, SYNTAX_ERROR_AND_DEAD_CODE_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = notification["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics.len() >= 2,
        "expected both the syntax error and at least one dead-code warning in one publish: {diagnostics:?}"
    );

    let severities: Vec<i64> = diagnostics.iter().map(|d| d["severity"].as_i64().unwrap()).collect();
    assert!(severities.contains(&1), "expected an ERROR-severity entry: {diagnostics:?}");
    assert!(severities.contains(&2), "expected a WARNING-severity entry: {diagnostics:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
