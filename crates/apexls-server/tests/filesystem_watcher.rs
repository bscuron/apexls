//! Protocol-level verification of `Backend::start_watcher`: a file added
//! to disk *without ever being opened in the editor* -- no `didOpen`,
//! no `didChange` -- must still show up in the bind, closing the gap
//! `apex_binder::BoundProgram::from_files_cached`'s doc comment
//! otherwise accepts as an honest limit (`need_fresh_discovery` has no
//! other signal for "a file appeared on disk"). Follows
//! `binder_integration.rs`'s exact pattern (spawn the real binary, drive
//! it over real stdio, watch stderr for "rebuild complete" as the only
//! externally observable "a rebuild happened" signal).

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
        // A server-initiated notification (e.g. textDocument/publishDiagnostics,
        // pushed proactively after every rebuild) has a `method` but no `id` --
        // skip past it rather than mistaking it for the response a caller is
        // actually waiting for.
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

/// Blocks (up to `timeout`) until a "rebuild complete" line arrives on
/// `rx`, or panics -- the same wait `binder_integration.rs` uses to know
/// a background rebuild actually finished.
fn wait_for_rebuild(rx: &mpsc::Receiver<String>, timeout: Duration, context: &str) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) if line.contains("rebuild complete") => return,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("expected a \"rebuild complete\" line on stderr within {timeout:?} ({context})");
}

#[test]
fn a_file_added_on_disk_without_being_opened_is_picked_up_by_the_watcher() {
    let dir = write_fixture_dir(
        "watcher-add",
        &[("Foo.cls", "public class Foo { Integer x; }")],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn apexls-server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let stderr = child.stderr.take().unwrap();

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
    // `initialized` schedules the first rebuild (`Foo` only) and, on
    // success, `initialize` has already started the watcher -- both
    // race against nothing else here, so this first "rebuild complete"
    // is unambiguous.
    wait_for_rebuild(&rx, Duration::from_secs(10), "initial rebuild");

    // The file that should trigger the watcher: written straight to
    // disk, never sent through `textDocument/didOpen`/`didChange` --
    // the exact case a plain document-sync-triggered rebuild can never
    // see, since `overrides` only ever contains open buffers.
    std::fs::write(dir.join("Bar.cls"), "public class Bar { }").unwrap();

    // Debounced (500ms quiet period in `Backend::start_watcher`) plus
    // real OS filesystem-event latency -- generous timeout to stay
    // reliable under CI-like load without weakening what's asserted.
    wait_for_rebuild(&rx, Duration::from_secs(15), "watcher-triggered rebuild");

    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/symbol",
            "params": { "query": "Bar" }
        }),
    );
    let response = recv(&mut stdout);
    let results = response
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        results.iter().any(|s| s.get("name").and_then(|n| n.as_str()) == Some("Bar")),
        "Bar.cls was written directly to disk (never opened) and should be visible after the \
         watcher-triggered rebuild re-walked the directory; workspace/symbol response: {response:?}"
    );

    send(
        &mut stdin,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
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
