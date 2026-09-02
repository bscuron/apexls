//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! type mismatches. Follows `unresolved_reference_diagnostics.rs`'s
//! exact `Session` harness. Positive tests for the three checkpoints
//! (local-var declaration, `return`, call/`new` argument), plus a
//! negative test for each of ticket 08's six real NPSP patterns a
//! zero-false-positive checker must never flag.

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

fn type_mismatch_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| {
            let m = d["message"].as_str().unwrap_or_default();
            m.contains("cannot assign") || m.contains("cannot return") || m.contains("does not match")
        })
        .collect()
}

fn run_fixture(name: &str, files: &[(&str, &str)]) -> Session {
    let dir = write_fixture_dir(name, files);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let (concrete_name, concrete_text) = files.last().unwrap();
    let concrete_uri = Url::from_file_path(dir.join(concrete_name)).unwrap();
    Session::start(&root_uri, &concrete_uri, concrete_text)
}

// -- positive tests: one per checkpoint --

const BAD_LOCAL_VAR_SRC: &str = "public class Foo {\n    public void run() {\n        Integer x = 'not a number';\n    }\n}\n";

#[test]
fn a_local_variable_initialized_with_an_incompatible_type_is_reported_as_an_error() {
    let mut session = run_fixture("mismatch-local-var", &[("Foo.cls", BAD_LOCAL_VAR_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one type-mismatch diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    session.shutdown();
}

const BAD_RETURN_SRC: &str = "public class Foo {\n    public Integer run() {\n        return 'not a number';\n    }\n}\n";

#[test]
fn a_return_with_an_incompatible_type_is_reported_as_an_error() {
    let mut session = run_fixture("mismatch-return", &[("Foo.cls", BAD_RETURN_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one type-mismatch diagnostic: {diagnostics:?}");
    session.shutdown();
}

const BAD_ARGUMENT_SRC: &str = "public class Foo {\n    public void take(Boolean flag) {\n    }\n    public void run() {\n        take('not a boolean');\n    }\n}\n";

#[test]
fn a_call_argument_with_an_incompatible_type_is_reported_as_an_error() {
    let mut session = run_fixture("mismatch-argument", &[("Foo.cls", BAD_ARGUMENT_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one type-mismatch diagnostic: {diagnostics:?}");
    session.shutdown();
}

const BAD_CONSTRUCTOR_ARGUMENT_SRC: &str =
    "public class Foo {\n    public Foo(Boolean flag) {\n    }\n    public void run() {\n        Foo f = new Foo('not a boolean');\n    }\n}\n";

#[test]
fn a_constructor_argument_with_an_incompatible_type_is_reported_as_an_error() {
    let mut session = run_fixture("mismatch-ctor-argument", &[("Foo.cls", BAD_CONSTRUCTOR_ARGUMENT_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one type-mismatch diagnostic: {diagnostics:?}");
    session.shutdown();
}

// -- negative tests: ticket 08's six real NPSP patterns, none ever flagged --

/// 1. Implicit numeric widening at a call boundary (`TEST_OpportunityBuilder.withAmount(Decimal)`
/// called with a bare integer literal, real NPSP shape).
const NUMERIC_WIDENING_SRC: &str =
    "public class Foo {\n    public void withAmount(Decimal amount) {\n    }\n    public void run() {\n        withAmount(20);\n        Decimal d = 5;\n    }\n}\n";

#[test]
fn implicit_numeric_widening_is_not_reported() {
    let mut session = run_fixture("mismatch-numeric-widening", &[("Foo.cls", NUMERIC_WIDENING_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// 2. `Object`-typed variables/returns receiving concrete values (dynamic
/// SObject field access idiom, real NPSP shape).
const OBJECT_TYPED_DYNAMIC_ACCESS_SRC: &str =
    "public class Foo {\n    public void run(Account acc) {\n        Object value = acc.get('Name');\n    }\n}\n";

#[test]
fn assigning_a_dynamic_value_into_an_object_typed_variable_is_not_reported() {
    let mut session = run_fixture("mismatch-object-typed", &[("Foo.cls", OBJECT_TYPED_DYNAMIC_ACCESS_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// 3. A cast recovering a concrete type out of a dynamic `.get(...)`
/// (real NPSP defensive-unboxing idiom).
const CAST_RECOVERS_CONCRETE_TYPE_SRC: &str =
    "public class Foo {\n    public void run(Account acc) {\n        String name = (String) acc.get('Name');\n    }\n}\n";

#[test]
fn a_cast_recovering_a_concrete_type_is_not_reported() {
    let mut session = run_fixture("mismatch-cast-recovers", &[("Foo.cls", CAST_RECOVERS_CONCRETE_TYPE_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// 4. Collection covariance (`List<Concrete>` into a `List<SObject>`-typed
/// parameter, no cast, real NPSP shape).
const COLLECTION_COVARIANCE_SRC: &str = "public class Foo {\n    public List<Account> accounts() {\n        return new List<Account>();\n    }\n    public void setRecords(List<SObject> records) {\n    }\n    public void run() {\n        setRecords(accounts());\n    }\n}\n";

#[test]
fn list_covariance_into_a_list_of_sobject_is_not_reported() {
    let mut session = run_fixture("mismatch-collection-covariance", &[("Foo.cls", COLLECTION_COVARIANCE_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// 5. `Id`/`String` bidirectional compatibility (real NPSP shape: an
/// implicit assignment of an `Id` into a `String`-typed variable/param).
const ID_STRING_BIDIRECTIONAL_SRC: &str =
    "public class Foo {\n    public void run(Id recordId) {\n        String s = recordId;\n        takesId(s);\n    }\n    public void takesId(Id anId) {\n    }\n}\n";

#[test]
fn id_and_string_bidirectional_compatibility_is_not_reported() {
    let mut session = run_fixture("mismatch-id-string", &[("Foo.cls", ID_STRING_BIDIRECTIONAL_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// 6. SObject-to-`SObject` widening (a concrete object type into a
/// generic `SObject`-typed variable, never the reverse).
const SOBJECT_WIDENING_SRC: &str = "public class Foo {\n    public void run(Account acc) {\n        SObject generic = acc;\n    }\n}\n";

#[test]
fn sobject_to_sobject_widening_is_not_reported() {
    let mut session = run_fixture("mismatch-sobject-widening", &[("Foo.cls", SOBJECT_WIDENING_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics: {diagnostics:?}");
    session.shutdown();
}

const NO_MISMATCH_CLEAN_SRC: &str =
    "public class Foo {\n    public Integer run(Integer x) {\n        Integer y = x;\n        return y;\n    }\n}\n";

#[test]
fn normal_code_with_no_type_mismatches_has_no_diagnostics() {
    let mut session = run_fixture("mismatch-clean", &[("Foo.cls", NO_MISMATCH_CLEAN_SRC)]);
    let notification = session.next_diagnostics();
    let diagnostics = type_mismatch_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no type-mismatch diagnostics on clean code: {diagnostics:?}");
    session.shutdown();
}
