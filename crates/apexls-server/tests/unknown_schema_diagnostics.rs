//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! `Resolution::UnknownSchema`. Follows `unresolved_reference_diagnostics.rs`'s
//! exact `Session` harness.
//!
//! Scoped specifically to genuine SOQL/SOSL object-or-field references
//! (`ptr.kind() == SyntaxKind::SoqlFieldName`) -- see
//! `apexls_server::capabilities::unknown_schema_diagnostics`'s own doc
//! comment for why the *other* two `UnknownSchema` producers (the
//! `<Object>.fields.<Field>` describe-token hop, an SObject constructor
//! field-init) are deliberately excluded. The negative test below
//! confirms the describe-token hop specifically does *not* misfire.

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

fn unknown_schema_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"].as_str().unwrap_or_default().contains("not a valid"))
        .collect()
}

const BAD_OBJECT_SRC: &str =
    "public class Foo {\n    public void run() {\n        List<SObject> xs = [SELECT Id FROM Totally_Fake_Object__c];\n    }\n}\n";

/// A bad `FROM` object cascades into a second diagnostic for the `SELECT`
/// field checked against it (`Id` can't be found on an object that
/// doesn't exist either) -- the same "one root-cause typo, flagged twice,
/// redundant-looking but not incorrect" precedent
/// `unresolved_reference_diagnostics.rs`'s `QUALIFIED_TYPE_SRC` test
/// already establishes for `Resolution::Unresolved`.
#[test]
fn a_bad_soql_from_object_is_reported_as_an_error() {
    let dir = write_fixture_dir("unknown-schema-object", &[("Foo.cls", BAD_OBJECT_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, BAD_OBJECT_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unknown_schema_diagnostics(&notification);
    assert_eq!(
        diagnostics.len(),
        2,
        "expected the bad object's own diagnostic plus the cascading one for 'Id' checked against it: {diagnostics:?}"
    );
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
        assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    }
    assert!(
        diagnostics
            .iter()
            .any(|d| d["message"].as_str().unwrap().contains("'Totally_Fake_Object__c' is not a valid object")),
        "expected a diagnostic naming the bad object itself: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d["message"].as_str().unwrap().contains("'Id' is not a valid field on object 'Totally_Fake_Object__c'")),
        "expected the cascading diagnostic for the SELECT field checked against the bad object: {diagnostics:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const BAD_FIELD_SRC: &str =
    "public class Foo {\n    public void run() {\n        List<Account> xs = [SELECT Totally_Fake_Field__c FROM Account];\n    }\n}\n";

#[test]
fn a_bad_soql_select_field_on_a_known_object_is_reported_as_an_error() {
    let dir = write_fixture_dir("unknown-schema-field", &[("Foo.cls", BAD_FIELD_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, BAD_FIELD_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unknown_schema_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unknown-schema diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
    let message = diagnostic["message"].as_str().unwrap();
    assert!(
        message.contains("Totally_Fake_Field__c") && message.contains("Account"),
        "expected the message to name both the bad field and its object: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `didChange` that fixes the bad object name clears the diagnostic on
/// the next `publishDiagnostics`, matching every other diagnostic source's
/// existing convention (`syntax_error_diagnostics.rs`,
/// `unresolved_reference_diagnostics.rs`).
#[test]
fn fixing_a_bad_soql_from_object_clears_the_diagnostic() {
    let dir = write_fixture_dir("unknown-schema-fix", &[("Foo.cls", BAD_OBJECT_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, BAD_OBJECT_SRC);
    let notification = session.next_diagnostics();
    assert_eq!(unknown_schema_diagnostics(&notification).len(), 2);

    let fixed_src = "public class Foo {\n    public void run() {\n        List<SObject> xs = [SELECT Id FROM Account];\n    }\n}\n";
    session.did_change(&foo_uri, 2, fixed_src);
    let notification = session.next_diagnostics();
    let diagnostics = unknown_schema_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected the diagnostic to clear once the object name is fixed: {diagnostics:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `<Object>.fields.<FieldName>` describe-token shorthand is real,
/// compiler-magic Apex syntax, not a genuine field access -- it must never
/// be reported as an unknown-schema error even though it resolves via the
/// exact same `Resolution::UnknownSchema` variant internally (see
/// `unknown_schema_diagnostics`'s own doc comment).
const DESCRIBE_TOKEN_SRC: &str =
    "public class Foo {\n    public void run() {\n        Schema.SObjectField f = Account.fields.Name;\n    }\n}\n";

#[test]
fn the_fields_describe_token_shorthand_is_not_reported_as_unknown_schema() {
    let dir = write_fixture_dir("unknown-schema-describe-token", &[("Foo.cls", DESCRIBE_TOKEN_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, DESCRIBE_TOKEN_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unknown_schema_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unknown-schema diagnostics for the describe-token shorthand: {diagnostics:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
