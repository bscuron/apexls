//! Real bug found via a live-reported repro against the real NPSP corpus,
//! fixed the same day: renaming a method with cross-file call sites, then
//! sending a couple of body-only edits to the two touched, *open* files
//! (an editor's own follow-up saves/reformats -- matching exactly what
//! was observed live) crashed a background rebuild with a `SymbolTable::get`
//! index-out-of-bounds panic, which -- combined with `wait_for_rebuild`'s
//! (`main.rs`) new wait-for-the-covering-rebuild behavior -- turned every
//! later `hover`/`documentHighlight`/etc. request into a permanent hang
//! instead of the previous "stale but responsive" degradation.
//!
//! Root cause: `SymbolTable::rebuild_indices` (`crates/apex-binder/src/symbol_table.rs`)
//! indexed *every* entry in a file's `by_file` slice into `members_of`/
//! `members_by_name`, including local variables `append_file_symbols`
//! had appended on a *previous* rebind -- but `rebuild_indices` itself
//! only reruns when a declaration changes project-wide, while
//! `append_file_symbols` truncates and re-appends a file's locals on
//! *every* body-only rebind independent of that. A `SymbolId` for a
//! local, captured into `members_of` during one rebuild, could end up
//! pointing past the end of `by_file[file]` once a later, index-rebuild-
//! free body-only rebind truncated that file's locals down further --
//! exactly what `SymbolTable::params`/the `this()`/`super()`/`new`
//! constructor lookups (all of which iterate `members_of(container)` and
//! immediately call `SymbolTable::get` on every entry) then panicked on.
//! Fixed by only indexing a file's *declared* symbols
//! (`declared_symbols_of_file`, i.e. `local < declared_len(file)`) into
//! those two indices -- locals were never meant to be looked up through
//! them in the first place (lexical-scope lookup via `crate::scope::ScopeTree`
//! already handles locals entirely separately, per `append_file_symbols`'s
//! own doc comment).
//!
//! This test spawns the real binary against the real NPSP corpus (the
//! scale that exposed the bug -- not reliably reproducible against a
//! small synthetic fixture), renames `TDTM_ProcessControl.toggleTriggerState`
//! (a real, cross-file-referenced method), then sends the exact edit
//! shape observed live: the two touched, opened files resent twice each
//! (matching an editor applying the rename locally and then reformatting/
//! resaving). It asserts no "panicked" line ever appears on stderr and
//! that a request sent afterward actually gets a real, non-hung response
//! -- both properties that failed before the fix (the panic, then the
//! permanent hang once every later request started waiting on a
//! `bound_version` that could no longer advance).
//!
//! Every file this touches outside the two opened buffers gets restored
//! to its original content when the test ends (success, failure, or
//! panic) via `RestoreOnDrop` -- this writes to files inside the real,
//! shared `tests/corpus/npsp` checkout, not a disposable fixture
//! directory, so leaving them mutated would corrupt every other test's
//! view of the corpus.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

/// Applies a `WorkspaceEdit`-style `TextEdit` array (single-line ranges
/// only -- every real edit this test exercises is) to `text`.
fn apply_edits(text: &str, edits: &[serde_json::Value]) -> String {
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| {
        let start = &e["range"]["start"];
        (start["line"].as_u64().unwrap(), start["character"].as_u64().unwrap())
    });
    edits.reverse();
    for edit in edits {
        let start_line = edit["range"]["start"]["line"].as_u64().unwrap() as usize;
        let start_char = edit["range"]["start"]["character"].as_u64().unwrap() as usize;
        let end_line = edit["range"]["end"]["line"].as_u64().unwrap() as usize;
        let end_char = edit["range"]["end"]["character"].as_u64().unwrap() as usize;
        let new_text = edit["newText"].as_str().unwrap();
        assert_eq!(start_line, end_line, "multi-line edit not handled by this test");
        let line = &lines[start_line];
        let mut new_line = String::new();
        new_line.push_str(&line[..start_char.min(line.len())]);
        new_line.push_str(new_text);
        new_line.push_str(&line[end_char.min(line.len())..]);
        lines[start_line] = new_line;
    }
    lines.join("\n")
}

/// Restores every `(path, original_content)` pair on drop, regardless of
/// how the test ends -- see this file's module doc comment for why that
/// matters against the shared corpus checkout.
struct RestoreOnDrop {
    originals: Vec<(PathBuf, String)>,
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        for (path, original) in &self.originals {
            let _ = std::fs::write(path, original);
        }
    }
}

#[test]
fn rename_across_files_survives_body_only_edits_to_the_touched_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("npsp")
        .canonicalize()
        .expect("real NPSP corpus checkout (tests/corpus/npsp) not present");
    let root_uri = Url::from_file_path(&root).unwrap();

    let pc_path = root.join("force-app/tdtm/classes/TDTM_ProcessControl.cls");
    let api_path = root.join("force-app/tdtm/classes/TDTM_Config_API.cls");
    let pc_uri = Url::from_file_path(&pc_path).unwrap();
    let api_uri = Url::from_file_path(&api_path).unwrap();
    let pc_src = std::fs::read_to_string(&pc_path).unwrap();
    let api_src = std::fs::read_to_string(&api_path).unwrap();

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

    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "processId": null, "rootUri": null,
            "capabilities": { "general": { "positionEncodings": ["utf-8"] } },
            "workspaceFolders": [{ "uri": root_uri, "name": "npsp" }],
        }
    }));
    recv(&mut stdout);
    send(&mut stdin, &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": pc_uri, "languageId": "apex", "version": 0, "text": pc_src } }
    }));
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": api_uri, "languageId": "apex", "version": 0, "text": api_src } }
    }));

    let decl_line = pc_src
        .lines()
        .position(|l| l.contains("public static void toggleTriggerState("))
        .expect("TDTM_ProcessControl.cls should still declare toggleTriggerState");
    let decl_col = pc_src.lines().nth(decl_line).unwrap().find("toggleTriggerState").unwrap();

    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": pc_uri },
            "position": { "line": decl_line, "character": decl_col },
            "newName": "toggleTriggerStateRenamed"
        }
    }));
    let rename_response = recv(&mut stdout);
    let changes = rename_response["result"]["changes"]
        .as_object()
        .expect("rename of a real, unambiguous NPSP method should succeed")
        .clone();

    let pc_edits = changes.get(pc_uri.as_str()).cloned().unwrap_or_default();
    let api_edits = changes.get(api_uri.as_str()).cloned().unwrap_or_default();
    let new_pc_src = apply_edits(&pc_src, pc_edits.as_array().unwrap());
    let new_api_src = apply_edits(&api_src, api_edits.as_array().unwrap());

    // Every other touched file gets the rename applied directly on disk
    // (an unopened buffer, exactly like a real editor writing files it
    // doesn't have open) -- and restored on drop no matter what happens
    // below.
    let mut originals = Vec::new();
    for (uri, edits) in &changes {
        if uri == pc_uri.as_str() || uri == api_uri.as_str() {
            continue;
        }
        let path = Url::parse(uri).unwrap().to_file_path().unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        let new_text = apply_edits(&original, edits.as_array().unwrap());
        std::fs::write(&path, new_text).unwrap();
        originals.push((path, original));
    }
    let _restore = RestoreOnDrop { originals };

    // The exact edit shape observed live: each touched, open file resent
    // twice in a row (an editor applying the rename, then immediately
    // reformatting/resaving the same content plus a trailing newline).
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": { "textDocument": { "uri": pc_uri, "version": 1 }, "contentChanges": [{ "text": new_pc_src }] }
    }));
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": { "textDocument": { "uri": api_uri, "version": 1 }, "contentChanges": [{ "text": new_api_src }] }
    }));
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": { "textDocument": { "uri": pc_uri, "version": 2 }, "contentChanges": [{ "text": format!("{new_pc_src}\n") }] }
    }));
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": { "textDocument": { "uri": api_uri, "version": 2 }, "contentChanges": [{ "text": format!("{new_api_src}\n") }] }
    }));

    // Drain stderr for a few seconds watching for a panic, exactly like
    // `rapid_edit_burst.rs`.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_panic: Option<String> = None;
    let mut saw_rebuild_after_edits = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) => {
                if line.to_lowercase().contains("panicked") {
                    saw_panic = Some(line);
                    break;
                }
                if line.contains("rebuild complete") {
                    saw_rebuild_after_edits = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if saw_rebuild_after_edits {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saw_panic.is_none(),
        "server logged a panic after the rename + body-only edit sequence: {saw_panic:?}"
    );

    // The real assertion: a request sent afterward must get a real
    // response, not hang forever -- this is exactly what `wait_for_rebuild`
    // (`main.rs`) turned into a permanent hang before the
    // `SymbolTable::rebuild_indices` fix, since a panicked rebuild used to
    // leave `bound_version` stuck below every later request's target
    // version forever.
    send(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/hover",
        "params": { "textDocument": { "uri": pc_uri }, "position": { "line": decl_line, "character": decl_col } }
    }));
    let hover_response = recv(&mut stdout);
    assert!(
        hover_response.get("error").is_none(),
        "hover returned an error after the edit sequence: {hover_response:?}"
    );
    let hover_text = hover_response["result"]["contents"]["value"]
        .as_str()
        .expect("hover should return real contents, not null, after the edit sequence");
    assert!(
        hover_text.contains("toggleTriggerStateRenamed"),
        "expected hover to show the renamed method, got: {hover_text:?}"
    );

    send(&mut stdin, &serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": null }));
    let response = recv(&mut stdout);
    assert!(response.get("error").is_none(), "shutdown returned an error: {response:?}");
    send(&mut stdin, &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
    let status = child.wait().expect("failed to wait on apexls-server");
    assert!(status.success(), "apexls-server did not exit cleanly: {status:?}");
}
