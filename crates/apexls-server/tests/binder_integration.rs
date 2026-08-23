//! Verifies `BACKLOG.md` §2 Step 1's `apex-binder` wiring: opening a real
//! fixture project through the real stdio protocol actually triggers a
//! background rebuild that *completes*, not just that the server accepts
//! the document-sync notifications without erroring (`handshake.rs`
//! already covers that). The bind itself isn't observable through any
//! LSP response yet (no capability consumes it -- see `BACKLOG.md` §3),
//! so this test observes it the only way currently possible: the
//! `tracing`-emitted "rebuild complete" line on the server's stderr.

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
fn opening_a_project_triggers_a_background_bind_that_completes() {
    let dir = write_fixture_dir(
        "bind",
        &[
            ("Foo.cls", "public class Foo { Integer x; }"),
            ("Bar.cls", "public class Bar extends Foo { }"),
        ],
    );
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
    let stderr = child.stderr.take().unwrap();

    // Stream stderr on a background thread: `tracing`'s log lines are the
    // only externally observable signal that a rebuild happened at all,
    // since nothing consumes the bind through the protocol yet.
    let (tx, rx) = mpsc::channel::<String>();
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
                    "text": "public class Foo { Integer x; }"
                }
            }
        }),
    );

    // `initialized` alone already schedules one rebuild; `didOpen` above
    // schedules a second. Either (or both) completing satisfies this
    // test -- it's proving a bind happens at all, not asserting exactly
    // how many times, which BACKLOG.md §2 Step 1 explicitly leaves
    // unspecified (no debouncing yet).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_rebuild = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
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
        "expected a \"rebuild complete\" line on stderr within 10s after didOpen"
    );

    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null }),
    );
    let response = recv(&mut stdout);
    assert!(
        response.get("error").is_none(),
        "shutdown returned an error: {response:?}"
    );
    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );

    let status = child.wait().expect("failed to wait on apexls-server");
    assert!(
        status.success(),
        "apexls-server did not exit cleanly after exit: {status:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
