//! Protocol-level verification of `textDocument/diagnostic`, the
//! pull-model counterpart to `textDocument/publishDiagnostics` covered by
//! `syntax_error_diagnostics.rs`. Follows that file's exact `Session`
//! harness, swapping `next_diagnostics` (which waits on the server-pushed
//! notification) for a `pull_diagnostics` request/response round-trip.

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
    next_id: i64,
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
            next_id: 2,
        };
        let response = recv(&mut session.stdout);
        assert!(response.get("error").is_none(), "initialize returned an error: {response:?}");
        assert!(
            response["result"]["capabilities"]["diagnosticProvider"].is_object(),
            "expected initialize to advertise diagnosticProvider: {response:?}"
        );

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

    /// Sends a `textDocument/diagnostic` request and returns its
    /// `RelatedFullDocumentDiagnosticReport` result -- every response this
    /// server ever sends is `kind: "full"` (see `Backend::document_diagnostic`'s
    /// own doc comment: no `previous_result_id`/`Unchanged` support yet).
    fn pull_diagnostics(&mut self, uri: &Url) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut self.stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "textDocument/diagnostic",
                "params": { "textDocument": { "uri": uri } },
            }),
        );
        let response = recv(&mut self.stdout);
        assert_eq!(response["id"], serde_json::json!(id));
        assert!(response.get("error").is_none(), "textDocument/diagnostic returned an error: {response:?}");
        assert_eq!(response["result"]["kind"], serde_json::json!("full"), "expected a full report: {response:?}");
        response["result"].clone()
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
/// missing its closing paren, the same fixture `syntax_error_diagnostics.rs`
/// uses for the push path.
const MISSING_CLOSE_PAREN_SRC: &str =
    "public class Foo {\n    public void run() {\n        foo(1, 2;\n    }\n}\n";
const FIXED_SRC: &str = "public class Foo {\n    public void run() {\n        foo(1, 2);\n    }\n}\n";

#[test]
fn pulling_diagnostics_returns_the_same_syntax_error_the_push_path_reports() {
    let dir = write_fixture_dir("document-diagnostic-pull", &[("Foo.cls", MISSING_CLOSE_PAREN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, MISSING_CLOSE_PAREN_SRC);

    let report = session.pull_diagnostics(&foo_uri);
    let diagnostics = report["items"].as_array().unwrap();
    // Same non-exhaustive filter `syntax_error_diagnostics.rs` uses: `foo`
    // is also undeclared, a legitimate, unrelated ERROR from
    // `unresolved_reference_diagnostics`.
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["severity"] == serde_json::json!(1) && d["message"].as_str().unwrap_or_default().starts_with("expected"))
        .collect();
    assert_eq!(errors.len(), 1, "expected exactly one syntax-error diagnostic: {diagnostics:?}");
    let message = errors[0]["message"].as_str().unwrap();
    assert!(
        message.contains("expected") && message.contains("RParen"),
        "expected a parser 'expected ..., found ...' style message: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixing_the_error_and_pulling_again_no_longer_reports_it() {
    let dir = write_fixture_dir("document-diagnostic-pull-fix", &[("Foo.cls", MISSING_CLOSE_PAREN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, MISSING_CLOSE_PAREN_SRC);

    let has_syntax_error = |report: &serde_json::Value| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == serde_json::json!(1) && d["message"].as_str().unwrap_or_default().starts_with("expected"))
    };

    assert!(has_syntax_error(&session.pull_diagnostics(&foo_uri)));

    session.did_change(&foo_uri, 2, FIXED_SRC);

    assert!(
        !has_syntax_error(&session.pull_diagnostics(&foo_uri)),
        "expected the syntax-error diagnostic to be gone after fixing and re-pulling"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pulling_diagnostics_for_an_unknown_file_returns_an_empty_full_report() {
    let dir = write_fixture_dir("document-diagnostic-pull-unknown", &[("Foo.cls", FIXED_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let unknown_uri = Url::from_file_path(dir.join("DoesNotExist.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, FIXED_SRC);

    let report = session.pull_diagnostics(&unknown_uri);
    assert_eq!(report["items"].as_array().unwrap().len(), 0, "expected no diagnostics for an unknown file: {report:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
