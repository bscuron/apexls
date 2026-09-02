//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! duplicate/conflicting modifiers. Follows `unresolved_reference_diagnostics.rs`'s
//! exact `Session` harness. All three fixture shapes and their exact
//! wording are taken directly from the Wayfinder `apex-diagnostics` map's
//! ticket 02, which confirmed each one against a real Salesforce org.

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

fn modifier_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| {
            let message = d["message"].as_str().unwrap_or_default();
            message.starts_with("Duplicate modifier:")
                || message == "Declarations can only have one scope"
                || message == "static methods cannot be abstract"
        })
        .collect()
}

const DUPLICATE_METHOD_MODIFIER_SRC: &str =
    "public class Foo {\n    private private void run() {\n    }\n}\n";

#[test]
fn a_duplicate_modifier_on_a_method_is_reported_as_an_error() {
    let dir = write_fixture_dir("modifier-duplicate-method", &[("Foo.cls", DUPLICATE_METHOD_MODIFIER_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, DUPLICATE_METHOD_MODIFIER_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one modifier diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    assert_eq!(diagnostic["message"], serde_json::json!("Duplicate modifier: private"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const DUPLICATE_FIELD_MODIFIER_SRC: &str = "public class Foo {\n    private private Integer x;\n}\n";

#[test]
fn a_duplicate_modifier_on_a_field_is_reported_as_an_error() {
    let dir = write_fixture_dir("modifier-duplicate-field", &[("Foo.cls", DUPLICATE_FIELD_MODIFIER_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, DUPLICATE_FIELD_MODIFIER_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one modifier diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0]["message"], serde_json::json!("Duplicate modifier: private"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const CONFLICTING_VISIBILITY_SRC: &str = "public class Foo {\n    private public void run() {\n    }\n}\n";

#[test]
fn conflicting_visibility_modifiers_are_reported_as_an_error() {
    let dir = write_fixture_dir("modifier-conflicting-visibility", &[("Foo.cls", CONFLICTING_VISIBILITY_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, CONFLICTING_VISIBILITY_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one modifier diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0]["message"], serde_json::json!("Declarations can only have one scope"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const DUPLICATE_STATIC_SRC: &str = "public class Foo {\n    public static static void run() {\n    }\n}\n";

#[test]
fn a_duplicate_static_modifier_is_reported_as_an_error() {
    let dir = write_fixture_dir("modifier-duplicate-static", &[("Foo.cls", DUPLICATE_STATIC_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, DUPLICATE_STATIC_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one modifier diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0]["message"], serde_json::json!("Duplicate modifier: static"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const STATIC_ABSTRACT_CONFLICT_SRC: &str =
    "public abstract class Foo {\n    public static abstract void run();\n}\n";

#[test]
fn a_static_abstract_method_is_reported_as_an_error() {
    let dir = write_fixture_dir("modifier-static-abstract", &[("Foo.cls", STATIC_ABSTRACT_CONFLICT_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, STATIC_ABSTRACT_CONFLICT_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one modifier diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0]["message"], serde_json::json!("static methods cannot be abstract"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const CLEAN_SRC: &str = "public class Foo {\n    public static void run() {\n    }\n\n    private Integer x;\n\n    private final Integer y = 1;\n}\n";

#[test]
fn a_normal_declaration_with_no_conflicting_modifiers_has_no_modifier_diagnostics() {
    let dir = write_fixture_dir("modifier-clean", &[("Foo.cls", CLEAN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, CLEAN_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = modifier_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no modifier diagnostics on clean code: {diagnostics:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
