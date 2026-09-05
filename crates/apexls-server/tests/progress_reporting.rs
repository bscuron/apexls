//! Protocol-level verification of `$/progress` reporting around a
//! rebuild: a client that declares `window.workDoneProgress` support at
//! `initialize` time must see a `window/workDoneProgress/create` request
//! followed by a `begin`/`end` `$/progress` notification pair for the
//! same token, bracketing the rebuild `initialized` schedules -- closing
//! the "a request can block on `wait_for_rebuild` with zero client-visible
//! feedback" gap `.scratch/apex-lsp-gaps/research.md` flagged. Follows
//! `binder_integration.rs`'s exact real-stdio harness pattern, extended
//! with a receiver that also answers the server-initiated
//! `window/workDoneProgress/create` request (a real client must, or the
//! server's own progress task would hang waiting for that response).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use lsp_types::Url;

fn send(stdin: &mut impl Write, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stdin.flush().unwrap();
}

/// Reads one `Content-Length`-framed JSON-RPC message, whatever it is --
/// unlike other tests' `recv`, this doesn't skip notifications, since
/// this test needs to observe the `$/progress` notifications themselves.
fn recv_any(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout before sending a full message");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.parse::<usize>().unwrap());
        }
    }
    let content_length = content_length.expect("message had no Content-Length header");
    let mut buf = vec![0u8; content_length];
    stdout.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apexls-server-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

#[test]
fn a_rebuild_reports_progress_when_the_client_supports_it() {
    let dir = write_fixture_dir("progress", &[("Foo.cls", "public class Foo { }")]);
    let root_uri = Url::from_file_path(&dir).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn apexls-server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                "capabilities": { "window": { "workDoneProgress": true } },
                "workspaceFolders": [{ "uri": root_uri, "name": "fixture" }],
            }
        }),
    );
    // Read messages until the `initialize` response (id 1) arrives --
    // nothing else should be sent before it, but this doesn't assume
    // that, matching `recv_any`'s "observe everything" contract.
    loop {
        let msg = recv_any(&mut stdout);
        if msg.get("id") == Some(&serde_json::json!(1)) {
            assert!(msg.get("error").is_none(), "initialize returned an error: {msg:?}");
            break;
        }
    }

    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // `initialized` unconditionally schedules one rebuild -- drive
    // messages until this test has seen the full create -> begin -> end
    // cycle for one token, answering the server's `create` request the
    // way a real client must (or the server's detached progress task,
    // which awaits that response, would hang forever).
    let mut created_token: Option<serde_json::Value> = None;
    let mut began_token: Option<serde_json::Value> = None;
    let mut ended_token: Option<serde_json::Value> = None;
    for _ in 0..500 {
        if ended_token.is_some() {
            break;
        }
        let msg = recv_any(&mut stdout);
        if msg.get("method") == Some(&serde_json::json!("window/workDoneProgress/create")) {
            let token = msg["params"]["token"].clone();
            send(
                &mut stdin,
                &serde_json::json!({ "jsonrpc": "2.0", "id": msg["id"], "result": null }),
            );
            created_token = Some(token);
        } else if msg.get("method") == Some(&serde_json::json!("$/progress")) {
            let kind = msg["params"]["value"]["kind"].as_str().unwrap_or_default();
            let token = msg["params"]["token"].clone();
            match kind {
                "begin" => began_token = Some(token),
                "end" => ended_token = Some(token),
                other => panic!("unexpected $/progress kind {other:?}: {msg:?}"),
            }
        }
    }

    let created_token = created_token.expect("expected a window/workDoneProgress/create request");
    let began_token = began_token.expect("expected a $/progress begin notification");
    let ended_token = ended_token.expect("expected a $/progress end notification within 500 messages");
    assert_eq!(created_token, began_token, "begin should report against the created token");
    assert_eq!(created_token, ended_token, "end should report against the same created token");

    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null }),
    );
    loop {
        let msg = recv_any(&mut stdout);
        if msg.get("id") == Some(&serde_json::json!(2)) {
            assert!(msg.get("error").is_none(), "shutdown returned an error: {msg:?}");
            break;
        }
    }
    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );

    let status = child.wait().expect("failed to wait on apexls-server");
    assert!(status.success(), "apexls-server did not exit cleanly after exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rebuild_reports_no_progress_when_the_client_does_not_support_it() {
    let dir = write_fixture_dir("no-progress", &[("Foo.cls", "public class Foo { }")]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn apexls-server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                // No `window.workDoneProgress` capability at all -- the
                // server must never send progress notifications this
                // client never opted into.
                "capabilities": {},
                "workspaceFolders": [{ "uri": root_uri, "name": "fixture" }],
            }
        }),
    );
    loop {
        let msg = recv_any(&mut stdout);
        if msg.get("id") == Some(&serde_json::json!(1)) {
            assert!(msg.get("error").is_none(), "initialize returned an error: {msg:?}");
            break;
        }
    }

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
                    "text": "public class Foo { }"
                }
            }
        }),
    );

    // Drive both scheduled rebuilds (`initialized`'s own, then `didOpen`'s)
    // to completion via `textDocument/publishDiagnostics` -- the one
    // notification every rebuild always sends for an open document --
    // then assert neither ever sent progress.
    let mut publishes_seen = 0;
    for _ in 0..500 {
        if publishes_seen >= 1 {
            break;
        }
        let msg = recv_any(&mut stdout);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default();
        assert_ne!(method, "window/workDoneProgress/create", "unsupported client should never see a create request: {msg:?}");
        assert_ne!(method, "$/progress", "unsupported client should never see a $/progress notification: {msg:?}");
        if method == "textDocument/publishDiagnostics" {
            publishes_seen += 1;
        }
    }
    assert_eq!(publishes_seen, 1, "expected at least one publishDiagnostics within 500 messages");

    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null }),
    );
    loop {
        let msg = recv_any(&mut stdout);
        if msg.get("id") == Some(&serde_json::json!(2)) {
            assert!(msg.get("error").is_none(), "shutdown returned an error: {msg:?}");
            break;
        }
    }
    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );

    let status = child.wait().expect("failed to wait on apexls-server");
    assert!(status.success(), "apexls-server did not exit cleanly after exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
