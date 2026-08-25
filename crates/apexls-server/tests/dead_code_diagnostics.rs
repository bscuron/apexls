//! Protocol-level verification of the dead-code diagnostics + delete
//! quick-fix pair: an unused private method gets a
//! `textDocument/publishDiagnostics` `WARNING` (tagged `UNNECESSARY`)
//! pushed proactively after the rebuild that reflects it -- not lazily,
//! behind a request, the way every other capability works -- and
//! `textDocument/codeAction` over that range returns a quick-fix whose
//! `WorkspaceEdit` deletes it cleanly. Follows `rename.rs`'s `Session`
//! harness; the one addition is `recv_notification`, since this is the
//! first test file that needs to observe a server-initiated notification
//! rather than skip past it while waiting for a response.

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

/// A response has an `id`; a server-initiated notification (e.g.
/// `textDocument/publishDiagnostics`, pushed after every rebuild) has a
/// `method` but no `id` -- skip past any notification while waiting for
/// the response a particular request actually wants.
fn recv(stdout: &mut impl BufRead) -> serde_json::Value {
    loop {
        let value = read_frame(stdout);
        if value.get("id").is_some() {
            return value;
        }
    }
}

/// The inverse of `recv`: waits for the next notification named `method`,
/// skipping past any *response* frames encountered along the way (there
/// shouldn't be any outstanding requests when this is called, but being
/// tolerant costs nothing).
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

fn position_of(src: &str, needle: &str) -> (u32, u32) {
    let byte_offset = src.find(needle).expect("needle not found in fixture source");
    let before = &src[..byte_offset];
    let line = before.matches('\n').count() as u32;
    let character = match before.rfind('\n') {
        Some(last_newline) => (byte_offset - last_newline - 1) as u32,
        None => byte_offset as u32,
    };
    (line, character)
}

/// Applies one LSP `TextEdit`-shaped JSON value (`range`/`newText`) to
/// `src`, converting line/character positions to byte offsets via `src`'s
/// own line boundaries -- unlike `rename.rs`'s `apply_edits`, this
/// supports a multi-line range, which a whole-declaration deletion
/// (start of one line through the start of the next) always is.
fn apply_single_edit(src: &str, edit: &serde_json::Value) -> String {
    let mut line_starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let to_offset = |line: u64, character: u64| line_starts[line as usize] + character as usize;
    let start = to_offset(
        edit["range"]["start"]["line"].as_u64().unwrap(),
        edit["range"]["start"]["character"].as_u64().unwrap(),
    );
    let end = to_offset(
        edit["range"]["end"]["line"].as_u64().unwrap(),
        edit["range"]["end"]["character"].as_u64().unwrap(),
    );
    let new_text = edit["newText"].as_str().unwrap();
    format!("{}{}{}", &src[..start], new_text, &src[end..])
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

    fn request(&mut self, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        recv(&mut self.stdout)
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

#[test]
fn unused_private_method_gets_a_diagnostic_and_a_working_quickfix() {
    let src = "public class Foo {\n    private void helper() { }\n    public void run() { }\n}\n";
    let dir = write_fixture_dir("dead-code-diagnostics", &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut session = Session::start(&root_uri, &foo_uri, src);

    // The diagnostic push happens inside the rebuild worker itself, right
    // after the "rebuild complete" line `wait_for_rebuild` already waited
    // for -- `next_diagnostics` just needs to read it off stdout.
    let notification = session.next_diagnostics();
    assert_eq!(notification["params"]["uri"], serde_json::json!(foo_uri));
    let diagnostics = notification["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "expected exactly one dead-code diagnostic: {diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(2), "expected WARNING severity");
    assert_eq!(diagnostic["tags"], serde_json::json!([1]), "expected the UNNECESSARY tag");
    assert!(
        diagnostic["message"].as_str().unwrap().contains("helper"),
        "expected the message to name the dead method: {diagnostic:?}"
    );

    let (line, character) = position_of(src, "helper");
    let response = session.request(
        2,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character },
            },
            "context": { "diagnostics": diagnostics },
        }),
    );
    assert!(response.get("error").is_none(), "codeAction returned an error: {response:?}");
    let actions = response["result"].as_array().expect("expected a code action array");
    assert_eq!(actions.len(), 1, "expected exactly one quick-fix: {actions:?}");
    let action = &actions[0];
    assert_eq!(action["kind"], serde_json::json!("quickfix"));
    assert!(
        action["title"].as_str().unwrap().contains("helper"),
        "expected the title to name the dead method: {action:?}"
    );

    let edits = action["edit"]["changes"][foo_uri.as_str()]
        .as_array()
        .expect("expected exactly one file's worth of edits");
    assert_eq!(edits.len(), 1);
    let after = apply_single_edit(src, &edits[0]);
    assert_eq!(
        after,
        "public class Foo {\n    public void run() { }\n}\n",
        "applying the quick-fix should leave the file with only the live method"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
