//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! `Resolution::Unresolved` references. Follows `syntax_error_diagnostics.rs`'s
//! exact `Session` harness.
//!
//! The feature deliberately surfaces *every* `Unresolved` reference, not a
//! filtered subset -- a reference matching one of the specific, identified
//! shapes where this binder's own resolution has no fallback at all (a
//! catch-clause exception type, a `super(...)` call whose target type
//! couldn't be resolved, one segment of a namespace-qualified stdlib type,
//! ...) gets `WARNING` with a message explaining why, while everything
//! else gets `ERROR` as the higher-confidence default. See
//! `apexls_server::capabilities::classify_unresolved`'s own doc comment
//! for the full reasoning.

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

/// A diagnostic from `unresolved_reference_diagnostics` specifically,
/// distinguished from `syntax_error_diagnostics`/`dead_code_diagnostics`'s
/// own entries by message shape (`"unresolved reference to"` for the
/// `WARNING`/structural case, `"cannot resolve reference"` for the
/// `ERROR`/default case -- see `classify_unresolved`'s doc comment).
fn unresolved_reference_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| {
            let message = d["message"].as_str().unwrap_or_default();
            message.contains("unresolved reference to") || message.contains("cannot resolve reference")
        })
        .collect()
}

const TYPO_SRC: &str =
    "public class Foo {\n    public void run() {\n        Integer x = undefinedVariable;\n    }\n}\n";

#[test]
fn a_genuine_typo_is_reported_as_an_error() {
    let dir = write_fixture_dir("unresolved-typo", &[("Foo.cls", TYPO_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, TYPO_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unresolved_reference_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unresolved-reference diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    let message = diagnostic["message"].as_str().unwrap();
    assert!(
        message.contains("undefinedVariable"),
        "expected the message to name the bad identifier: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

const CATCH_CLAUSE_SRC: &str = "public class Foo {\n    public void run() {\n        try {\n        } catch (DmlException e) {\n        }\n    }\n}\n";

#[test]
fn a_catch_clause_exception_type_is_reported_as_a_warning_not_an_error() {
    let dir = write_fixture_dir("unresolved-catch-clause", &[("Foo.cls", CATCH_CLAUSE_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, CATCH_CLAUSE_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unresolved_reference_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unresolved-reference diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(
        diagnostic["severity"],
        serde_json::json!(2),
        "a catch-clause exception type is a known binder limitation (project-local-only \
         lookup, no stdlib fallback), not necessarily a real bug -- expected WARNING: {diagnostic:?}"
    );
    let message = diagnostic["message"].as_str().unwrap();
    assert!(
        message.contains("DmlException") && message.contains("apexls limitation"),
        "expected the message to name the type and explain it's a tool limitation: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `MyException`'s own supertype (`Exception`) still has no
/// `SymbolTable`-level fallback for `super`/`super(...)` resolution
/// specifically (`inherit::resolve_inheritance`'s own doc comment: "an
/// unresolvable supertype... is simply dropped" -- true regardless of
/// whether the name is also a real, modeled `apex_stdlib` class), so
/// `super(msg)`'s own container type is unknown at bind time -- the same
/// root cause as the bare-`SuperExpr` case, reached through `CallExpr`
/// instead. The `extends Exception` clause's own `Type`-kind reference,
/// by contrast, resolves cleanly (`Exception` is a real, populated
/// `apex_stdlib::standard_classes()` entry -- see that function's own doc
/// comment for the scraper gap this fixed), so only the `super(msg)` call
/// itself produces a diagnostic here.
const SUPER_CALL_SRC: &str =
    "public class MyException extends Exception {\n    public MyException(String msg) {\n        super(msg);\n    }\n}\n";

#[test]
fn a_super_call_on_an_unresolvable_supertype_is_reported_as_a_warning_not_an_error() {
    let dir = write_fixture_dir("unresolved-super-call", &[("MyException.cls", SUPER_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let file_uri = Url::from_file_path(dir.join("MyException.cls")).unwrap();

    let mut session = Session::start(&root_uri, &file_uri, SUPER_CALL_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unresolved_reference_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected only the super(...) call's own diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(2), "expected WARNING: {diagnostic:?}");
    let message = diagnostic["message"].as_str().unwrap();
    assert!(
        message.contains("apexls limitation"),
        "expected the message to explain this is a tool limitation, not invalid code: {message:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `System.String s = 'hi';` -- a namespace-qualified stdlib type
/// reference, two dotted segments. Neither `System` (columns 8-14) nor
/// `String` (columns 15-21) has a `Resolution` of its own beyond
/// `resolve::record_qualified_segments`'s per-segment entry, which is
/// always `Unresolved` by construction for both segments (see that
/// function's doc comment) -- even though the *whole* `System.String`
/// reference (the enclosing `Type` node) resolves just fine as a real
/// `StdlibMember`, confirmed directly against `BoundProgram` while
/// developing this test. Not asserted here via a hover request, though:
/// `BoundProgram::resolution_at`'s own doc comment says a per-token
/// resolution, when one is recorded, is always checked *before* climbing
/// to any enclosing node's -- so hovering either segment surfaces that
/// segment's own `Unresolved` entry, never the whole node's real
/// resolution, for exactly the same structural reason this diagnostic
/// fires at all. `System` in particular is tokenized as its own
/// recognized keyword (`SyntaxKind::System`), not a plain
/// `SyntaxKind::Identifier` -- `classify_unresolved` deliberately
/// classifies this shape by excluding the known node kinds rather than
/// matching a specific token kind, for exactly this reason.
const QUALIFIED_TYPE_SRC: &str = "public class Foo {\n    public void run() {\n        System.String s = 'hi';\n    }\n}\n";

#[test]
fn a_namespace_qualified_type_segment_is_reported_as_a_warning_not_an_error() {
    let dir = write_fixture_dir("unresolved-qualified-type", &[("Foo.cls", QUALIFIED_TYPE_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, QUALIFIED_TYPE_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unresolved_reference_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 2, "expected both the 'System' and 'String' segments: {diagnostics:?}");
    assert!(
        diagnostics.iter().all(|d| d["severity"] == serde_json::json!(2)),
        "expected both segments to be WARNING, not ERROR: {diagnostics:?}"
    );
    assert!(
        diagnostics.iter().all(|d| d["message"].as_str().unwrap().contains("apexls limitation")),
        "expected both messages to explain this is a tool limitation: {diagnostics:?}"
    );
    let system = diagnostics
        .iter()
        .find(|d| d["range"]["start"]["character"] == serde_json::json!(8))
        .unwrap_or_else(|| panic!("expected an entry starting at 'System': {diagnostics:?}"));
    assert_eq!(system["range"]["end"]["character"], serde_json::json!(14), "expected the range to cover just 'System': {system:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A genuine typo *and* a catch-clause exception type in the same file --
/// confirms the classifier runs per-reference within one merged
/// `publishDiagnostics` notification, not just in isolation.
const TYPO_AND_CATCH_CLAUSE_SRC: &str = "public class Foo {\n    public void run() {\n        Integer x = undefinedVariable;\n        try {\n        } catch (DmlException e) {\n        }\n    }\n}\n";

#[test]
fn an_error_and_a_warning_both_appear_in_the_same_publish() {
    let dir = write_fixture_dir("unresolved-mixed", &[("Foo.cls", TYPO_AND_CATCH_CLAUSE_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, TYPO_AND_CATCH_CLAUSE_SRC);

    let notification = session.next_diagnostics();
    let diagnostics = unresolved_reference_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 2, "expected both the typo and the catch clause: {diagnostics:?}");
    let severities: Vec<i64> = diagnostics.iter().map(|d| d["severity"].as_i64().unwrap()).collect();
    assert!(severities.contains(&1), "expected an ERROR-severity entry (the typo): {diagnostics:?}");
    assert!(severities.contains(&2), "expected a WARNING-severity entry (the catch clause): {diagnostics:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real-corpus smoke test, matching this project's own convention of
/// running whole-NPSP-corpus assertions as normal (not `#[ignore]`d)
/// tests (see e.g. `rename.rs`'s own real-corpus test): `fflib_QueryFactory.cls`
/// is real, dense, third-party Apex (SOQL-builder library code, not a
/// hand-crafted fixture) that surfaced several genuine binder gaps during
/// development of this diagnostic -- `apex_stdlib`'s `Exception`/`Iterator`
/// entries, `String.split`'s scraped array return type, a project type's
/// own inherited-but-unresolvable-supertype member lookup, and a
/// namespace-qualified class reference used in expression position
/// (`Schema.SoapType.ID`) -- each a real, general fix (see
/// `apex_stdlib::standard_classes`'s and `crate::resolve`'s own doc
/// comments), not something special-cased for this file. Pinned here as
/// an `ERROR`-severity regression guard: `WARNING`s (mostly namespace-
/// qualified-segment structural noise, `Schema`/`SObjectType`/
/// `ChildRelationship`/... -- see `classify_unresolved`'s own doc
/// comment) are expected and not asserted against, since this file
/// legitimately still has some.
#[test]
fn a_real_npsp_fflib_query_factory_file_has_no_error_severity_diagnostics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let file = root.join("force-app/infrastructure/apex-common/main/classes/fflib_QueryFactory.cls");
    let root_uri = Url::from_file_path(&root).unwrap();
    let file_uri = Url::from_file_path(&file).unwrap();
    let src = std::fs::read_to_string(&file).unwrap();

    let mut session = Session::start(&root_uri, &file_uri, &src);

    let notification = session.next_diagnostics();
    let diagnostics = notification["params"]["diagnostics"].as_array().unwrap();
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["severity"] == serde_json::json!(1))
        .collect();
    assert!(errors.is_empty(), "expected no ERROR-severity diagnostics: {errors:?}");

    session.shutdown();
}
