//! Protocol-level verification of `BACKLOG.md` §3's `textDocument/inlayHint`
//! -- `capabilities::inlay_hints`'s `paramName:` labels at call sites.
//! Follows `hover_definition.rs`'s exact pattern (spawn the real binary,
//! drive it over real stdio, wait for the background rebuild's "rebuild
//! complete" stderr line before sending a request).

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

    fn inlay_hint(
        &mut self,
        id: i64,
        uri: &Url,
        start: (u32, u32),
        end: (u32, u32),
    ) -> serde_json::Value {
        let response = self.request(
            id,
            "textDocument/inlayHint",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start.0, "character": start.1 },
                    "end": { "line": end.0, "character": end.1 },
                },
            }),
        );
        assert!(
            response.get("error").is_none(),
            "inlayHint returned an error: {response:?}"
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

    /// Result entries as `(line, character, label)` triples, for
    /// order-independent assertions.
    fn hints(response: &serde_json::Value) -> Vec<(u64, u64, String)> {
        response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("expected an inlay hint array, got {response:?}"))
            .iter()
            .map(|h| {
                (
                    h["position"]["line"].as_u64().unwrap(),
                    h["position"]["character"].as_u64().unwrap(),
                    h["label"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
}

/// `public class Foo {` / `    public void run() {` /
/// `        greet('hello');` / `    }` /
/// `    public void greet(String message) { }` / `}` -- the string
/// literal argument starts at line 2, character 14.
const SINGLE_ARG_SRC: &str = "public class Foo {\n    public void run() {\n        greet('hello');\n    }\n    public void greet(String message) { }\n}\n";

#[test]
fn inlay_hint_labels_a_call_argument_with_its_parameter_name() {
    let dir = write_fixture_dir("inlay-basic", &[("Foo.cls", SINGLE_ARG_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, SINGLE_ARG_SRC);

    let response = session.inlay_hint(2, &foo_uri, (0, 0), (6, 0));
    let hints = Session::hints(&response);
    assert_eq!(
        hints,
        vec![(2, 14, "message:".to_string())],
        "expected exactly one hint labeling the string literal argument: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` /
/// `        Integer accountId = 1;` /
/// `        foo(accountId, 5);` / `    }` /
/// `    public void foo(Integer accountId, Integer count) { }` / `}` --
/// the first argument (`accountId`) already spells its own parameter's
/// name, so it should be suppressed; the second (`5`) should not.
const SUPPRESSION_SRC: &str = "public class Foo {\n    public void run() {\n        Integer accountId = 1;\n        foo(accountId, 5);\n    }\n    public void foo(Integer accountId, Integer count) { }\n}\n";

#[test]
fn inlay_hint_suppresses_an_argument_that_already_spells_its_parameter_name() {
    let dir = write_fixture_dir("inlay-suppress", &[("Foo.cls", SUPPRESSION_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, SUPPRESSION_SRC);

    let response = session.inlay_hint(2, &foo_uri, (0, 0), (7, 0));
    let hints = Session::hints(&response);
    assert_eq!(
        hints,
        vec![(3, 23, "count:".to_string())],
        "expected only the second argument to get a hint: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `bar(compute())` where `compute` doesn't exist anywhere: both `bar`
/// overloads stay arity-compatible and neither can be ruled out by
/// argument type, so the call resolves to `Resolution::Candidates`
/// (genuinely ambiguous) rather than `Resolved` -- no hint should be
/// guessed for it.
const AMBIGUOUS_CALL_SRC: &str = "public class Foo {\n    public void run() {\n        bar(compute());\n    }\n    public void bar(Integer x) { }\n    public void bar(String x) { }\n}\n";

#[test]
fn inlay_hint_shows_nothing_for_an_ambiguous_overload_call() {
    let dir = write_fixture_dir("inlay-ambiguous", &[("Foo.cls", AMBIGUOUS_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, AMBIGUOUS_CALL_SRC);

    let response = session.inlay_hint(2, &foo_uri, (0, 0), (7, 0));
    let hints = Session::hints(&response);
    assert!(hints.is_empty(), "expected no hints for an ambiguous call: {response:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` /
/// `        Boolean b = String.isBlank('x');` / `    }` / `}` -- the
/// bundled stdlib schema carries each parameter's *name* too (scraped
/// straight off the real signature, e.g. `isBlank(String inputString)`),
/// not just its type, so a stdlib call gets a real hint the same as a
/// project one.
const STDLIB_CALL_SRC: &str =
    "public class Foo {\n    public void run() {\n        Boolean b = String.isBlank('x');\n    }\n}\n";

#[test]
fn inlay_hint_labels_a_stdlib_call_argument_with_its_parameter_name() {
    let dir = write_fixture_dir("inlay-stdlib", &[("Foo.cls", STDLIB_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_CALL_SRC);

    let response = session.inlay_hint(2, &foo_uri, (0, 0), (5, 0));
    let hints = Session::hints(&response);
    assert_eq!(
        hints,
        vec![(2, 35, "inputString:".to_string())],
        "expected a hint labeling the 'x' argument with isBlank's real parameter name: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Database.query` is a real, confirmed 2-way overload
/// (`query(String)` and `query(String, AccessLevel)`) -- a 1-argument
/// call should narrow to the 1-arg overload alone and still get a hint,
/// exactly like the project-method overload case above.
const STDLIB_OVERLOAD_CALL_SRC: &str =
    "public class Foo {\n    public void run() {\n        List<Account> accs = Database.query('SELECT Id FROM Account');\n    }\n}\n";

#[test]
fn inlay_hint_narrows_an_overloaded_stdlib_call_to_the_matching_arity() {
    let dir = write_fixture_dir("inlay-stdlib-overload", &[("Foo.cls", STDLIB_OVERLOAD_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_OVERLOAD_CALL_SRC);

    let response = session.inlay_hint(2, &foo_uri, (0, 0), (5, 0));
    let hints = Session::hints(&response);
    assert_eq!(hints.len(), 1, "expected exactly one hint for the 1-arg overload: {response:?}");
    assert!(
        hints[0].2.ends_with(':'),
        "expected a `paramName:` label, got {:?}",
        hints[0].2
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reuses `SINGLE_ARG_SRC`, but requests a range that only covers line 0
/// -- well short of the call on line 2 -- and confirms nothing comes
/// back for it, then confirms the full-file request still finds it.
/// Real coverage for the range-restriction plumbing itself
/// (`BoundProgram::call_sites_in_range`), not just the label logic.
#[test]
fn inlay_hint_only_returns_hints_inside_the_requested_range() {
    let dir = write_fixture_dir("inlay-range", &[("Foo.cls", SINGLE_ARG_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, SINGLE_ARG_SRC);

    let narrow = session.inlay_hint(2, &foo_uri, (0, 0), (1, 0));
    assert!(
        Session::hints(&narrow).is_empty(),
        "a range not covering the call should return no hints: {narrow:?}"
    );

    let whole_file = session.inlay_hint(3, &foo_uri, (0, 0), (6, 0));
    assert!(
        !Session::hints(&whole_file).is_empty(),
        "the same call should get a hint once the range covers it: {whole_file:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
