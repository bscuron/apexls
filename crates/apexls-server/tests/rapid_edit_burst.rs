//! Guards against a real bug, reported live against the actual NPSP
//! corpus (~1,044 files): a rapid burst of `didChange` notifications (an
//! LSP client can fire one per keystroke) used to spawn one independent,
//! un-debounced `spawn_blocking` rebuild per edit, with `overrides`
//! captured *before* acquiring `bind.cache`'s lock -- so an older edit's
//! rebuild could land on the shared cache *after* a newer edit's already
//! had, leaving `apex_binder::BindCache` internally inconsistent. Typing
//! a temporarily-invalid statement (a bare identifier with no `()`/`;`)
//! mid-edit triggered a `SymbolTable::get` index-out-of-bounds panic on a
//! background rebuild thread, which poisoned `bind.cache`'s `Mutex` --
//! permanently breaking every future rebuild (and so every hover/
//! definition/documentHighlight response) for the rest of the session,
//! since a poisoned `std::sync::Mutex` never recovers on its own.
//!
//! **Honest caveat**: this exact race is timing-dependent and wasn't
//! reliably reproducible against a small/synthetic project in isolation
//! (tried directly, including a genuinely concurrent multi-thread stress
//! harness at the `apex-binder` level, and this test's own burst against
//! the pre-fix code, both without a single repro in dozens of runs) --
//! it very likely needs the real corpus's scale (~1,044 files, several
//! hundred ms of real cold-bind work) for the timing window to reliably
//! open. This test is therefore not a proven "fails before the fix,
//! passes after" regression test; it's a smoke/stress test replaying the
//! same edit shape (a valid call, a blank line, a bare identifier with no
//! `()`/`;`, the same with `()` added, then the final valid duplicate
//! call) as fast as the test process can write to stdin, against a
//! project padded out with several hundred filler classes for a more
//! realistic rebuild latency. It asserts the properties that must hold
//! regardless of whether it happens to hit the exact race this run: no
//! "panicked" line ever appears on stderr, and the server is still fully
//! responsive afterward (a `textDocument/definition` request resolves
//! correctly, not `null`).
//!
//! The fix went through two designs. The first kept one `spawn_blocking`
//! task per edit but debounced it and captured `overrides` only after
//! acquiring `bind.cache`'s lock, so calls could never regress relative
//! to each other regardless of acquisition order. The current design
//! (`main.rs`'s `spawn_rebuild_worker`) replaces that with a single
//! persistent worker task woken by a `tokio::sync::Notify` -- with
//! exactly one task ever calling `from_files_cached`, sequentially,
//! there is no second rebuild left to race against in the first place,
//! no debounce delay before an idle worker picks up a single edit, and
//! no timing constant to tune. The real confidence in the fix comes from
//! that structural argument, not from this test managing to hit the
//! race.

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
    // A tiny (2-3 file) fixture rebinds fast enough that every rebuild
    // finishes well before the next `didChange` in a burst is even sent,
    // never actually overlapping -- no race window to exercise. Padding
    // the project out with several hundred trivial filler classes gives
    // each rebuild's `apex_discover::discover`/stat/parse pass enough
    // real work (a few ms, matching the scale that exposed the real bug
    // on the ~1,044-file NPSP corpus) that a burst of edits sent with no
    // delay between them has a genuine chance of landing while a
    // previous rebuild is still in flight.
    for i in 0..600 {
        std::fs::write(
            dir.join(format!("Filler{i}.cls")),
            format!("public class Filler{i} {{\n    public void noop() {{ }}\n}}\n"),
        )
        .unwrap();
    }
    dir
}

fn did_change(stdin: &mut impl Write, uri: &Url, version: i64, text: &str) {
    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }),
    );
}

#[test]
fn a_rapid_burst_of_edits_including_a_transiently_invalid_one_never_panics_or_wedges_the_server() {
    let foo_src = "public class Foo {\n    public void bar() {\n        helper();\n    }\n\n    private void helper() { }\n}\n";
    let caller_src =
        "public class Caller {\n    public void go() {\n        Foo f = new Foo();\n        f.bar();\n    }\n}\n";
    let dir = write_fixture_dir("burst", &[("Foo.cls", foo_src), ("Caller.cls", caller_src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let caller_uri = Url::from_file_path(dir.join("Caller.cls")).unwrap();

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
                "capabilities": { "general": { "positionEncodings": ["utf-8"] } },
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
                "textDocument": { "uri": foo_uri, "languageId": "apex", "version": 0, "text": foo_src }
            }
        }),
    );
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": caller_uri, "languageId": "apex", "version": 0, "text": caller_src }
            }
        }),
    );

    // The exact shape of the real repro: a valid call, then a blank
    // tab-indented line, then a bare identifier (invalid -- no `()`, no
    // `;`), then the identifier with parens but still no `;`, then
    // finally a complete, valid duplicate call. Sent back-to-back with no
    // delay, as fast as this process can write to stdin -- matching an
    // LSP client that fires one `didChange` per keystroke.
    let steps = [
        "public class Foo {\n    public void bar() {\n        helper();\n\t\n    }\n\n    private void helper() { }\n}\n",
        "public class Foo {\n    public void bar() {\n        helper();\n\thelper\n    }\n\n    private void helper() { }\n}\n",
        "public class Foo {\n    public void bar() {\n        helper();\n\thelper()\n    }\n\n    private void helper() { }\n}\n",
        "public class Foo {\n    public void bar() {\n        helper();\n        helper();\n    }\n\n    private void helper() { }\n}\n",
    ];
    for (i, text) in steps.iter().enumerate() {
        did_change(&mut stdin, &foo_uri, (i + 1) as i64, text);
    }

    // Drain stderr for a few seconds, watching for a rebuild settling and
    // (the actual thing this test guards against) any panic.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_panic: Option<String> = None;
    let mut saw_rebuild_after_burst = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) => {
                if line.to_lowercase().contains("panicked") {
                    saw_panic = Some(line);
                    break;
                }
                if line.contains("rebuild complete") {
                    saw_rebuild_after_burst = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if saw_rebuild_after_burst {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saw_panic.is_none(),
        "server logged a panic during the rapid edit burst: {saw_panic:?}"
    );
    assert!(
        saw_rebuild_after_burst,
        "expected at least one \"rebuild complete\" line after the edit burst settled"
    );

    // The real assertion: the server must still be able to answer a
    // request correctly afterward, not permanently return null because
    // `bind.cache`'s Mutex got poisoned by an earlier panic.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": caller_uri },
                "position": { "line": 3, "character": 11 }
            }
        }),
    );
    let response = recv(&mut stdout);
    assert!(
        response.get("error").is_none(),
        "definition returned an error: {response:?}"
    );
    let result_is_null = response.get("result").is_none_or(|r| r.is_null());
    assert!(
        !result_is_null,
        "definition on Caller.cls's f.bar() returned null -- server likely wedged: {response:?}"
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
