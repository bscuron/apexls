//! Protocol-level verification of `BACKLOG.md`'s `textDocument/completion`
//! entry -- `capabilities::completion` end to end through the real
//! server binary. Follows `signature_help.rs`'s exact `Session` harness
//! pattern (spawn the real binary, drive it over real stdio, wait for
//! the background rebuild's "rebuild complete" stderr line before
//! sending a position-based request).

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
    /// The `initialize` response's own `result.capabilities`, kept
    /// around for the one test that checks capability advertisement --
    /// every other test only cares that the server started cleanly.
    init_capabilities: serde_json::Value,
}

impl Session {
    fn start_with_files(root_uri: &Url, files: &[(&Url, &str)]) -> Self {
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
        let init_capabilities = response["result"]["capabilities"].clone();

        send(
            &mut stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        );
        for (uri, text) in files {
            send(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "languageId": "apex",
                            "version": 1,
                            "text": text,
                        }
                    }
                }),
            );
        }

        let mut session = Session {
            child,
            stdin,
            stdout,
            rebuild_rx,
            init_capabilities,
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

    fn completion(&mut self, id: i64, uri: &Url, line: u32, character: u32) -> serde_json::Value {
        let response = self.request(
            id,
            "textDocument/completion",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        assert!(
            response.get("error").is_none(),
            "completion returned an error: {response:?}"
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

/// `items` from a `CompletionList`/`CompletionItem[]` response body --
/// `lsp_types::CompletionResponse` serializes as either shape depending
/// on whether the server returns a bare list or the `{ isIncomplete,
/// items }` wrapper; `capabilities::completion` always uses the latter.
fn items(response: &serde_json::Value) -> &Vec<serde_json::Value> {
    response["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a CompletionList, got {response:?}"))
}

fn labels(response: &serde_json::Value) -> Vec<&str> {
    items(response)
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect()
}

#[test]
fn initialize_advertises_completion_with_a_dot_trigger_character() {
    let dir = write_fixture_dir("completion-capability", &[]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let session = Session::start_with_files(&root_uri, &[]);

    let provider = &session.init_capabilities["completionProvider"];
    assert!(
        !provider.is_null(),
        "expected completionProvider to be advertised: {:?}",
        session.init_capabilities
    );
    assert_eq!(provider["triggerCharacters"], serde_json::json!(["."]));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `public class Foo {` / `    public Integer bar;` /
/// `    public void run() {` / `        Foo other = new Foo();` /
/// `        other.` / `    }` / `}` -- cursor at the very end of `other.`,
/// a real dangling-dot mid-edit position.
const DANGLING_DOT_SRC: &str = "public class Foo {\n    public Integer bar;\n    public void run() {\n        Foo other = new Foo();\n        other.\n    }\n}\n";

#[test]
fn a_dangling_dot_offers_the_receivers_members() {
    let dir = write_fixture_dir("completion-dangling-dot", &[("Foo.cls", DANGLING_DOT_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_files(&root_uri, &[(&foo_uri, DANGLING_DOT_SRC)]);

    // Line 4, character 14: right after `other.`.
    let response = session.completion(2, &foo_uri, 4, 14);
    let labels = labels(&response);
    assert!(
        labels.contains(&"bar"),
        "expected `bar` among the receiver's members: {labels:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same fixture, cursor placed at a fresh bare-identifier position
/// (inside `run`'s body, nothing typed yet) -- should see locals/params,
/// the enclosing type's own members, other project types, and keywords.
const BARE_IDENTIFIER_SRC: &str = "public class Foo {\n    public Integer bar;\n    public void run(Integer x) {\n        \n    }\n}\n";

#[test]
fn a_fresh_bare_identifier_position_offers_locals_members_types_and_keywords() {
    let dir = write_fixture_dir(
        "completion-bare-identifier",
        &[("Foo.cls", BARE_IDENTIFIER_SRC), ("Other.cls", "public class Other { }")],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let other_uri = Url::from_file_path(dir.join("Other.cls")).unwrap();
    let mut session = Session::start_with_files(
        &root_uri,
        &[(&foo_uri, BARE_IDENTIFIER_SRC), (&other_uri, "public class Other { }")],
    );

    // Line 3, character 8: the blank line inside `run`'s body.
    let response = session.completion(2, &foo_uri, 3, 8);
    let labels = labels(&response);
    assert!(labels.contains(&"x"), "expected the parameter `x`: {labels:?}");
    assert!(labels.contains(&"bar"), "expected the enclosing type's own field: {labels:?}");
    assert!(labels.contains(&"Other"), "expected another project type: {labels:?}");
    assert!(labels.contains(&"if"), "expected a general keyword: {labels:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
