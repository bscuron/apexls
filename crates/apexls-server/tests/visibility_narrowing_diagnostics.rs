//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! visibility narrowing. Follows `type_mismatch_diagnostics.rs`'s exact
//! `Session` harness. The underlying candidate-selection algorithm
//! (which member narrows to what, and every exemption) is already
//! thoroughly unit-tested in `apex-binder`'s own `visibility_narrowing`
//! module -- this file only verifies the LSP-facing contract: message
//! wording, severity, absence of a tag, and that the diagnostic actually
//! reaches a real client over the wire.

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
        let path = dir.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
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

fn narrowing_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"].as_str().unwrap_or_default().contains("could be"))
        .collect()
}

fn run_fixture(name: &str, files: &[(&str, &str)]) -> Session {
    let dir = write_fixture_dir(name, files);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let (concrete_name, concrete_text) = files.last().unwrap();
    let concrete_uri = Url::from_file_path(dir.join(concrete_name)).unwrap();
    Session::start(&root_uri, &concrete_uri, concrete_text)
}

const NARROWABLE_PUBLIC_METHOD_SRC: &str =
    "public class Foo {\n    public void helper() { }\n    public void run() { helper(); }\n}\n";

#[test]
fn a_public_method_used_only_within_its_own_class_is_reported_with_the_narrowest_target() {
    let mut session = run_fixture("narrowing-public-same-class", &[("Foo.cls", NARROWABLE_PUBLIC_METHOD_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = narrowing_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one narrowing diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(
        diagnostic["message"],
        serde_json::json!("Method 'helper' is declared 'public' but could be 'private'"),
        "unexpected message: {diagnostic:?}"
    );
    assert_eq!(diagnostic["severity"], serde_json::json!(2), "expected WARNING severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    assert!(
        diagnostic.get("tags").is_none() || diagnostic["tags"].as_array().unwrap().is_empty(),
        "expected no DiagnosticTag (unlike dead_code_diagnostics's UNNECESSARY tag): {diagnostic:?}"
    );
    session.shutdown();
}

const NARROWABLE_PUBLIC_FIELD_SRC: &str =
    "public class Foo {\n    public Integer x;\n    public void run() { x = 1; }\n}\n";

#[test]
fn a_public_field_used_only_within_its_own_class_is_reported() {
    let mut session = run_fixture("narrowing-public-field", &[("Foo.cls", NARROWABLE_PUBLIC_FIELD_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = narrowing_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one narrowing diagnostic: {diagnostics:?}");
    assert_eq!(
        diagnostics[0]["message"],
        serde_json::json!("Field 'x' is declared 'public' but could be 'private'")
    );
    session.shutdown();
}

const UNUSED_PUBLIC_METHOD_SRC: &str = "public class Foo {\n    public void helper() { }\n}\n";

#[test]
fn a_zero_reference_public_method_is_not_reported_defers_to_dead_code() {
    let mut session = run_fixture("narrowing-zero-reference", &[("Foo.cls", UNUSED_PUBLIC_METHOD_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = narrowing_diagnostics(&notification);
    assert!(
        diagnostics.is_empty(),
        "a zero-reference member is dead_code_diagnostics's territory, not this diagnostic's: {diagnostics:?}"
    );
    session.shutdown();
}

const GENUINELY_PUBLIC_METHOD_SRC: &str = "public class Foo {\n    public void helper() { }\n}\n";

#[test]
fn a_public_method_used_from_an_unrelated_class_is_not_reported() {
    let caller = "public class Caller {\n    public void go() { new Foo().helper(); }\n}\n";
    let mut session = run_fixture(
        "narrowing-genuinely-public",
        &[("Caller.cls", caller), ("Foo.cls", GENUINELY_PUBLIC_METHOD_SRC)],
    );
    let notification = session.next_diagnostics();
    let diagnostics = narrowing_diagnostics(&notification);
    assert!(
        diagnostics.is_empty(),
        "referenced from an unrelated class -- must stay public, not reported: {diagnostics:?}"
    );
    session.shutdown();
}
