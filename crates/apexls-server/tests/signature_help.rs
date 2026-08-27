//! Protocol-level verification of `BACKLOG.md` §3's `textDocument/signatureHelp`
//! -- `capabilities::signature_help`'s argument-position tracking layered on
//! top of `narrow_by_overload`'s existing candidate set. Follows
//! `hover_definition.rs`'s exact pattern (spawn the real binary, drive it
//! over real stdio, wait for the background rebuild's "rebuild complete"
//! stderr line before sending a position-based request).

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

    fn signature_help(&mut self, id: i64, uri: &Url, line: u32, character: u32) -> serde_json::Value {
        let response = self.request(
            id,
            "textDocument/signatureHelp",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        assert!(
            response.get("error").is_none(),
            "signatureHelp returned an error: {response:?}"
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
}

/// `public class Foo {` / `    public void run() {` /
/// `        combine(1, 2);` / `    }` /
/// `    public Integer combine(Integer a, Integer b) { return a + b; }` / `}`
/// -- line 2, character 16 lands on the `1` (the first argument),
/// character 19 lands on the `2` (the second, right after `, `).
const TWO_PARAM_CALL_SRC: &str = "public class Foo {\n    public void run() {\n        combine(1, 2);\n    }\n    public Integer combine(Integer a, Integer b) { return a + b; }\n}\n";

#[test]
fn signature_help_reports_the_active_parameter_by_argument_position() {
    let dir = write_fixture_dir("sighelp-position", &[("Foo.cls", TWO_PARAM_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, TWO_PARAM_CALL_SRC);

    let on_first_arg = session.signature_help(2, &foo_uri, 2, 16);
    let signatures = on_first_arg["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("expected signatures, got {on_first_arg:?}"));
    assert_eq!(signatures.len(), 1);
    assert!(signatures[0]["label"].as_str().unwrap().contains("combine"));
    assert_eq!(on_first_arg["result"]["activeParameter"], 0);

    let on_second_arg = session.signature_help(3, &foo_uri, 2, 19);
    assert_eq!(on_second_arg["result"]["activeParameter"], 1);

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same fixture, cursor placed inside `combine(1, )` -- a dangling
/// trailing comma with nothing typed after it yet, the shape a real
/// editor sends mid-keystroke right after `,` fires signature help
/// again. Only one real argument node exists in the tree (`1`), but the
/// comma itself should still advance `activeParameter` past it.
const DANGLING_TRAILING_COMMA_SRC: &str = "public class Foo {\n    public void run() {\n        combine(1, );\n    }\n    public Integer combine(Integer a, Integer b) { return a + b; }\n}\n";

#[test]
fn signature_help_advances_past_a_dangling_trailing_comma() {
    let dir = write_fixture_dir("sighelp-trailing-comma", &[("Foo.cls", DANGLING_TRAILING_COMMA_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, DANGLING_TRAILING_COMMA_SRC);

    // Line 2, character 18: the space right after `, ` and before `)`.
    let response = session.signature_help(2, &foo_uri, 2, 18);
    assert_eq!(
        response["result"]["activeParameter"], 1,
        "a dangling trailing comma should still advance to the next parameter slot: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` / `        bar(1);` /
/// `    }` / `    public void bar(Integer x) { }` /
/// `    public void bar(Integer x, Integer y) { }` / `}` -- both `bar`
/// overloads should be listed, with the 1-arg one active since exactly
/// one argument is typed.
const OVERLOADED_CALL_SRC: &str = "public class Foo {\n    public void run() {\n        bar(1);\n    }\n    public void bar(Integer x) { }\n    public void bar(Integer x, Integer y) { }\n}\n";

#[test]
fn signature_help_lists_every_overload_and_activates_the_matching_one() {
    let dir = write_fixture_dir("sighelp-overload", &[("Foo.cls", OVERLOADED_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, OVERLOADED_CALL_SRC);

    // Line 2, character 12: inside the `1` argument.
    let response = session.signature_help(2, &foo_uri, 2, 12);
    let signatures = response["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("expected signatures, got {response:?}"));
    assert_eq!(signatures.len(), 2, "expected both `bar` overloads: {response:?}");

    let active = response["result"]["activeSignature"].as_u64().unwrap() as usize;
    let active_params = signatures[active]["parameters"].as_array().unwrap();
    assert_eq!(
        active_params.len(),
        1,
        "the 1-arg overload should be active for a 1-argument call: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public Foo(Integer x) { }` /
/// `    public Foo(Integer x, String y) { }` / `    public void run() {` /
/// `        new Foo(1);` / `    }` / `}` -- constructor overloads go
/// through the same path as method overloads (`SymbolTable::members_of`
/// instead of `lookup_member`).
const CONSTRUCTOR_CALL_SRC: &str = "public class Foo {\n    public Foo(Integer x) { }\n    public Foo(Integer x, String y) { }\n    public void run() {\n        new Foo(1);\n    }\n}\n";

#[test]
fn signature_help_works_for_a_constructor_call() {
    let dir = write_fixture_dir("sighelp-ctor", &[("Foo.cls", CONSTRUCTOR_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, CONSTRUCTOR_CALL_SRC);

    // Line 4, character 16: inside the `1` argument of `new Foo(1)`.
    let response = session.signature_help(2, &foo_uri, 4, 16);
    let signatures = response["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("expected signatures, got {response:?}"));
    assert_eq!(signatures.len(), 2, "expected both `Foo` constructor overloads: {response:?}");
    assert_eq!(response["result"]["activeParameter"], 0);

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public void run() {` /
/// `        Boolean b = String.isBlank('x');` / `    }` / `}` -- the
/// bundled stdlib schema path (`Resolution::StdlibMember`), not a
/// `SymbolId`-backed one.
const STDLIB_CALL_SRC: &str = "public class Foo {\n    public void run() {\n        Boolean b = String.isBlank('x');\n    }\n}\n";

#[test]
fn signature_help_works_for_a_stdlib_method_call() {
    let dir = write_fixture_dir("sighelp-stdlib", &[("Foo.cls", STDLIB_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, STDLIB_CALL_SRC);

    // Line 2, character 36: inside the `'x'` argument.
    let response = session.signature_help(2, &foo_uri, 2, 36);
    let signatures = response["result"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("expected signatures, got {response:?}"));
    assert!(!signatures.is_empty(), "expected at least one isBlank signature: {response:?}");
    assert!(
        signatures[0]["label"].as_str().unwrap().contains("isBlank"),
        "expected the signature label to mention isBlank: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// No enclosing `ArgList` at all (cursor sits on a plain statement) --
/// must return `None`, not panic or fabricate a signature.
const NO_CALL_SRC: &str = "public class Foo {\n    public void run() {\n        Integer x = 1;\n    }\n}\n";

#[test]
fn signature_help_returns_none_outside_any_call() {
    let dir = write_fixture_dir("sighelp-none", &[("Foo.cls", NO_CALL_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, NO_CALL_SRC);

    let response = session.signature_help(2, &foo_uri, 2, 20);
    assert!(
        response["result"].is_null(),
        "expected no signature help outside a call: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
