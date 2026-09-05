//! Protocol-level verification of `textDocument/codeLens` for the Run
//! Test lens (ticket 02/03,
//! `.scratch/apex-lsp-gaps/issues/02-run-test-lens-decision.md`):
//! `capabilities::run_test_lenses`'s walk over the binder's already-bound
//! `SymbolTable`. Follows `inlay_hints.rs`'s exact real-stdio harness
//! pattern (spawn the real binary, drive it over real stdio, wait for
//! the background rebuild's "rebuild complete" stderr line before
//! sending a request). Doesn't exercise `workspace/executeCommand`
//! itself -- that shells out to the real `sf` CLI against a real
//! authenticated org, unsuitable for an automated test; the JSON
//! parsing it depends on (`summarize_apex_test_output`) is covered by
//! `lib.rs`'s own unit tests instead.

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
    loop {
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
        let value: serde_json::Value = serde_json::from_slice(&buf).unwrap();
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
}

impl Session {
    fn start_with_text(foo_uri: &Url, root_uri: &Url, text: &str) -> Self {
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
        assert!(
            response["result"]["capabilities"]["codeLensProvider"].is_object(),
            "expected a codeLensProvider capability: {response:?}"
        );
        assert_eq!(
            response["result"]["capabilities"]["executeCommandProvider"]["commands"],
            serde_json::json!(["apexls.runTest"]),
            "expected apexls.runTest to be the sole registered command: {response:?}"
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
                        "text": text,
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

    fn code_lens(&mut self, id: i64, uri: &Url) -> serde_json::Value {
        let response = self.request(
            id,
            "textDocument/codeLens",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        );
        assert!(
            response.get("error").is_none(),
            "codeLens returned an error: {response:?}"
        );
        response
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

    /// Each lens as `(line, arguments)`, order-independent.
    fn lenses(response: &serde_json::Value) -> Vec<(u64, Vec<serde_json::Value>)> {
        response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a code lens array, got {response:?}"))
            .iter()
            .map(|lens| {
                let line = lens["range"]["start"]["line"].as_u64().unwrap();
                let command = &lens["command"];
                assert_eq!(command["command"], "apexls.runTest", "unexpected command id: {lens:?}");
                assert_eq!(command["title"], "Run Test", "unexpected lens title: {lens:?}");
                let arguments = command["arguments"].as_array().unwrap().clone();
                (line, arguments)
            })
            .collect()
    }
}

/// `@isTest private class FooTest {` / `    @isTest static void
/// testBar() { } }` -- both the class and its one method are lens
/// targets: class at line 0, method at line 1.
const ISTEST_CLASS_AND_METHOD_SRC: &str =
    "@isTest\nprivate class FooTest {\n    @isTest\n    static void testBar() { }\n}\n";

#[test]
fn lens_appears_on_both_an_istest_class_and_its_istest_method() {
    let dir = write_fixture_dir("codelens-basic", &[("FooTest.cls", ISTEST_CLASS_AND_METHOD_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("FooTest.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, ISTEST_CLASS_AND_METHOD_SRC);

    let response = session.code_lens(2, &foo_uri);
    let mut lenses = Session::lenses(&response);
    lenses.sort_by_key(|(line, _)| *line);
    assert_eq!(
        lenses,
        vec![
            (1, vec![serde_json::json!("FooTest")]),
            (3, vec![serde_json::json!("FooTest"), serde_json::json!("testBar")]),
        ],
        "expected one class-level lens and one method-level lens: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Legacy `testMethod` modifier keyword, no `@isTest` annotation at all
/// -- `ModifierSet::is_testmethod` alone must be enough to place the
/// method-level lens (the class itself isn't `@isTest`-annotated here,
/// so only the method gets a lens).
const LEGACY_TESTMETHOD_SRC: &str =
    "public class Foo {\n    private static testMethod void testSomething() { }\n}\n";

#[test]
fn lens_appears_on_a_legacy_testmethod_without_an_istest_class() {
    let dir = write_fixture_dir("codelens-legacy", &[("Foo.cls", LEGACY_TESTMETHOD_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, LEGACY_TESTMETHOD_SRC);

    let response = session.code_lens(2, &foo_uri);
    let lenses = Session::lenses(&response);
    assert_eq!(
        lenses,
        vec![(1, vec![serde_json::json!("Foo"), serde_json::json!("testSomething")])],
        "expected exactly one method-level lens, no class-level lens: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An entirely ordinary class with no test annotations anywhere --
/// zero lenses, confirming the walk doesn't fire spuriously.
const ORDINARY_CLASS_SRC: &str = "public class Foo {\n    public void doWork() { }\n}\n";

#[test]
fn no_lens_appears_on_an_ordinary_class_or_method() {
    let dir = write_fixture_dir("codelens-none", &[("Foo.cls", ORDINARY_CLASS_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, ORDINARY_CLASS_SRC);

    let response = session.code_lens(2, &foo_uri);
    assert!(
        response["result"].is_null() || response["result"].as_array().unwrap().is_empty(),
        "expected no lenses for an ordinary class: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A method nested inside an inner class, itself inside an `@isTest`
/// outer class -- the lens's `arguments` must name the *outer* (top-level,
/// compiled `ApexClass`-record) class, not the inner one, since that's
/// the only name `sf apex run test --tests` recognizes.
const NESTED_INNER_TEST_METHOD_SRC: &str = "@isTest\nprivate class Outer {\n    private class Inner {\n        @isTest\n        static void testNested() { }\n    }\n}\n";

#[test]
fn nested_test_method_lens_names_the_top_level_class() {
    let dir = write_fixture_dir("codelens-nested", &[("Outer.cls", NESTED_INNER_TEST_METHOD_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Outer.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, NESTED_INNER_TEST_METHOD_SRC);

    let response = session.code_lens(2, &foo_uri);
    let lenses = Session::lenses(&response);
    assert!(
        lenses
            .iter()
            .any(|(_, args)| args == &vec![serde_json::json!("Outer"), serde_json::json!("testNested")]),
        "expected the nested method's lens to name the top-level class \"Outer\": {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
