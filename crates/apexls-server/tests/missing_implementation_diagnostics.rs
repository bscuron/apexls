//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! missing interface/abstract-method implementations. Follows
//! `unresolved_reference_diagnostics.rs`'s exact `Session` harness.
//! Scoped to project-local interfaces/abstract classes only -- see
//! `apexls_server::capabilities::missing_implementation_diagnostics`'s
//! own doc comment for why a standard-library interface (`Comparable`
//! etc.) is deliberately never flagged.

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

fn missing_impl_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"].as_str().unwrap_or_default().contains("does not implement"))
        .collect()
}

/// Real Apex allows exactly one top-level type per file -- every fixture
/// here matches that (an interface/abstract-class file plus a separate
/// concrete-class file), opening the *concrete* class's own file (always
/// last in `files`) as the tested document.
fn run_fixture(name: &str, files: &[(&str, &str)]) -> Session {
    let dir = write_fixture_dir(name, files);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let (concrete_name, concrete_text) = files.last().unwrap();
    let concrete_uri = Url::from_file_path(dir.join(concrete_name)).unwrap();
    Session::start(&root_uri, &concrete_uri, concrete_text)
}

const GREETER_INTERFACE_SRC: &str = "public interface Greeter {\n    String greet();\n}\n";

#[test]
fn a_class_missing_a_required_interface_method_is_reported_as_an_error() {
    let mut session = run_fixture(
        "missing-impl-interface",
        &[("Greeter.cls", GREETER_INTERFACE_SRC), ("Foo.cls", "public class Foo implements Greeter {\n}\n")],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one missing-implementation diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    let message = diagnostic["message"].as_str().unwrap();
    assert!(message.contains("greet") && message.contains("Greeter"), "expected the message to name the method and interface: {message:?}");
    session.shutdown();
}

#[test]
fn a_class_that_implements_the_interface_method_without_override_is_not_reported() {
    let mut session = run_fixture(
        "missing-impl-interface-satisfied",
        &[
            ("Greeter.cls", GREETER_INTERFACE_SRC),
            ("Foo.cls", "public class Foo implements Greeter {\n    public String greet() {\n        return 'hi';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics: {diagnostics:?}");
    session.shutdown();
}

const BASE_ABSTRACT_CLASS_SRC: &str = "public abstract class Base {\n    public abstract String run();\n}\n";

#[test]
fn a_class_missing_a_required_abstract_method_is_reported_as_an_error() {
    let mut session = run_fixture(
        "missing-impl-abstract",
        &[("Base.cls", BASE_ABSTRACT_CLASS_SRC), ("Foo.cls", "public class Foo extends Base {\n}\n")],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one missing-implementation diagnostic: {diagnostics:?}");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(message.contains("run") && message.contains("Base"), "expected the message to name the method and abstract class: {message:?}");
    session.shutdown();
}

/// A same-name/same-arity method with no `override` keyword satisfies an
/// *interface* requirement but must NOT satisfy an *abstract-class*
/// requirement -- the settled asymmetry from ticket 07.
#[test]
fn a_same_signature_method_with_no_override_keyword_does_not_satisfy_an_abstract_class_requirement() {
    let mut session = run_fixture(
        "missing-impl-abstract-no-override",
        &[
            ("Base.cls", BASE_ABSTRACT_CLASS_SRC),
            ("Foo.cls", "public class Foo extends Base {\n    public String run() {\n        return 'hi';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert_eq!(
        diagnostics.len(),
        1,
        "expected still-missing: a same-signature method needs `override` to satisfy an abstract-class requirement: {diagnostics:?}"
    );
    session.shutdown();
}

#[test]
fn an_overriding_method_satisfies_an_abstract_class_requirement() {
    let mut session = run_fixture(
        "missing-impl-abstract-override",
        &[
            ("Base.cls", BASE_ABSTRACT_CLASS_SRC),
            ("Foo.cls", "public class Foo extends Base {\n    public override String run() {\n        return 'hi';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// An intermediate abstract subclass may legitimately implement only
/// *some* of the required methods -- it must never be flagged itself
/// (it's abstract), and the final concrete class only needs to cover
/// what's still missing.
#[test]
fn a_concrete_class_satisfied_via_an_intermediate_abstract_subclass_is_not_reported() {
    let mut session = run_fixture(
        "missing-impl-partial-via-abstract-subclass",
        &[
            ("Worker.cls", "public interface Worker {\n    String start();\n    String finish();\n}\n"),
            ("PartialWorker.cls", "public abstract class PartialWorker implements Worker {\n    public String start() {\n        return 'started';\n    }\n}\n"),
            ("Foo.cls", "public class Foo extends PartialWorker {\n    public String finish() {\n        return 'done';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// Two interfaces requiring the identical (name, arity) signature must
/// be satisfied by one method, not flagged as two separate requirements.
#[test]
fn two_interfaces_requiring_the_same_signature_are_satisfied_by_one_method() {
    let mut session = run_fixture(
        "missing-impl-multi-interface-dedup",
        &[
            ("Alpha.cls", "public interface Alpha {\n    String run();\n}\n"),
            ("Beta.cls", "public interface Beta {\n    String run();\n}\n"),
            ("Foo.cls", "public class Foo implements Alpha, Beta {\n    public String run() {\n        return 'ok';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics (one method satisfies both): {diagnostics:?}");
    session.shutdown();
}

/// A class implementing a standard-library interface must never be
/// flagged -- `Comparable` is silently invisible to `inherited_chain`
/// today (out of this diagnostic's deliberately project-local-only
/// scope), so flagging it would be a false positive against a case this
/// scope explicitly excludes.
#[test]
fn a_class_implementing_a_stdlib_interface_is_never_flagged() {
    let mut session = run_fixture(
        "missing-impl-stdlib-interface",
        &[("Foo.cls", "public class Foo implements Comparable {\n    public Integer compareTo(Object other) {\n        return 0;\n    }\n}\n")],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics for a stdlib interface: {diagnostics:?}");
    session.shutdown();
}

/// An abstract method declared with **no** explicit visibility modifier
/// can be overridden with no `override` keyword and still satisfies the
/// requirement -- confirmed against a real org (see
/// `has_required_override`'s own doc comment): `abstract String run();`
/// (no `public`/`protected`/`global`) is a different, looser case than
/// `public abstract String run();`, real NPSP shape
/// (`fflib_SObjectSelector.cls`'s `getSObjectType`/`getSObjectFieldList`).
const BASE_ABSTRACT_CLASS_NO_VISIBILITY_SRC: &str = "public abstract class Base {\n    abstract String run();\n}\n";

#[test]
fn an_abstract_method_with_no_explicit_visibility_is_satisfied_without_override() {
    let mut session = run_fixture(
        "missing-impl-abstract-no-visibility",
        &[
            ("Base.cls", BASE_ABSTRACT_CLASS_NO_VISIBILITY_SRC),
            ("Foo.cls", "public class Foo extends Base {\n    public String run() {\n        return 'hi';\n    }\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(
        diagnostics.is_empty(),
        "expected no missing-implementation diagnostics: an unmodified abstract declaration doesn't require `override` to satisfy: {diagnostics:?}"
    );
    session.shutdown();
}

#[test]
fn an_abstract_class_that_omits_required_methods_is_not_flagged_itself() {
    let mut session = run_fixture(
        "missing-impl-abstract-class-itself",
        &[
            ("Worker.cls", "public interface Worker {\n    String run();\n}\n"),
            ("Foo.cls", "public abstract class Foo implements Worker {\n}\n"),
        ],
    );
    let notification = session.next_diagnostics();
    let diagnostics = missing_impl_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no missing-implementation diagnostics on the abstract class itself: {diagnostics:?}");
    session.shutdown();
}
