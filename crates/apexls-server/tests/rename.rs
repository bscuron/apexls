//! Protocol-level verification of `textDocument/rename` and
//! `textDocument/prepareRename`. Follows `references_highlight.rs`'s
//! exact `Session` harness (spawn the real binary, drive it over real
//! stdio, wait for the background rebuild's "rebuild complete" stderr
//! line before sending a position-based request) -- rename reuses exactly
//! the same `references_to`/`highlight_range` data `references` does, so
//! this file focuses on what's new: multi-file `WorkspaceEdit` shape, the
//! class-rename constructor special case, and the refusal cases (an
//! ambiguous overload, an override-chain method, an invalid new name, a
//! colliding new name) that must come back as a `ResponseError`, never a
//! silently empty or wrong edit.

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

fn position_of(src: &str, needle: &str) -> (u32, u32) {
    let byte_offset = src
        .find(needle)
        .expect("needle not found in fixture source");
    let before = &src[..byte_offset];
    let line = before.matches('\n').count() as u32;
    let character = match before.rfind('\n') {
        Some(last_newline) => (byte_offset - last_newline - 1) as u32,
        None => byte_offset as u32,
    };
    (line, character)
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
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
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
                        "uri": open_uri,
                        "languageId": "apex",
                        "version": 1,
                        "text": open_text,
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

/// Applies a `WorkspaceEdit.changes[uri]` array (LSP line/character
/// `Range`s, all against `src`'s *original* text) to `src`, exactly like
/// a spec-compliant client would -- sorted by start position descending
/// so an earlier edit's range is never invalidated by a later one on the
/// same line. Used to assert the *exact* resulting text, not just an
/// edit's `newText`/count -- the gap that let a real bug (a `TextEdit`
/// end offset one character too wide, silently eating the identifier's
/// trailing space and merging tokens) go undetected.
fn apply_edits(src: &str, edits: &[serde_json::Value]) -> String {
    let mut ranges: Vec<(u32, u32, u32, u32, String)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap() as u32,
                e["range"]["start"]["character"].as_u64().unwrap() as u32,
                e["range"]["end"]["line"].as_u64().unwrap() as u32,
                e["range"]["end"]["character"].as_u64().unwrap() as u32,
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    ranges.sort_by_key(|a| std::cmp::Reverse((a.0, a.1)));
    let mut lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
    for (sl, sc, el, ec, new_text) in ranges {
        assert_eq!(sl, el, "apply_edits only supports single-line edits");
        let line_str = lines[sl as usize].clone();
        let mut new_line = String::new();
        new_line.push_str(&line_str[..sc as usize]);
        new_line.push_str(&new_text);
        new_line.push_str(&line_str[ec as usize..]);
        lines[sl as usize] = new_line;
    }
    lines.join("\n")
}

fn rename_request(session: &mut Session, uri: &Url, src: &str, needle: &str, new_name: &str) -> serde_json::Value {
    let (line, character) = position_of(src, needle);
    session.request(
        2,
        "textDocument/rename",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name,
        }),
    )
}

#[test]
fn rename_updates_the_declaration_and_every_cross_file_reference() {
    const BASE_SRC: &str = "public class Base {\n    public void greet() { }\n}\n";
    const CALLER_A_SRC: &str =
        "public class CallerA {\n    public void run() { new Base().greet(); }\n}\n";
    const CALLER_B_SRC: &str =
        "public class CallerB {\n    public void run() { new Base().greet(); }\n}\n";
    let dir = write_fixture_dir(
        "rename-cross-file",
        &[
            ("Base.cls", BASE_SRC),
            ("CallerA.cls", CALLER_A_SRC),
            ("CallerB.cls", CALLER_B_SRC),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let base_uri = Url::from_file_path(dir.join("Base.cls")).unwrap();
    let caller_a_uri = Url::from_file_path(dir.join("CallerA.cls")).unwrap();
    let caller_b_uri = Url::from_file_path(dir.join("CallerB.cls")).unwrap();

    let mut session = Session::start(&root_uri, &base_uri, BASE_SRC);

    let response = rename_request(&mut session, &base_uri, BASE_SRC, "greet", "salute");
    assert!(
        response.get("error").is_none(),
        "rename returned an error: {response:?}"
    );
    let changes = response["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("expected a WorkspaceEdit.changes map, got {response:?}"));
    assert_eq!(
        changes.len(),
        3,
        "expected edits in all three files: {changes:?}"
    );
    for (uri, expect_len) in [(&base_uri, 1), (&caller_a_uri, 1), (&caller_b_uri, 1)] {
        let edits = changes
            .get(uri.as_str())
            .unwrap_or_else(|| panic!("expected an edit in {uri}: {changes:?}"))
            .as_array()
            .unwrap();
        assert_eq!(edits.len(), expect_len, "edit count in {uri}: {edits:?}");
        assert_eq!(edits[0]["newText"], "salute");
    }
    assert_eq!(
        apply_edits(BASE_SRC, changes[base_uri.as_str()].as_array().unwrap()),
        "public class Base {\n    public void salute() { }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_A_SRC, changes[caller_a_uri.as_str()].as_array().unwrap()),
        "public class CallerA {\n    public void run() { new Base().salute(); }\n}\n"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renaming_a_class_also_renames_its_own_constructor() {
    const SRC: &str = "public class Widget {\n    \
         public Widget() { }\n    \
         public Widget(Integer x) { }\n\
     }\n";
    const CALLER_SRC: &str = "public class Caller {\n    \
         public void run() { Widget w = new Widget(); }\n\
     }\n";
    let dir = write_fixture_dir(
        "rename-class-ctor",
        &[("Widget.cls", SRC), ("Caller.cls", CALLER_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();
    let caller_uri = Url::from_file_path(dir.join("Caller.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);

    // Land on the class's own declared name, `public class Widget {`.
    let response = rename_request(&mut session, &widget_uri, SRC, "Widget {", "Gadget");
    assert!(
        response.get("error").is_none(),
        "rename returned an error: {response:?}"
    );
    let changes = response["result"]["changes"].as_object().unwrap();

    let widget_edits = changes.get(widget_uri.as_str()).unwrap().as_array().unwrap();
    // The class's own name, plus both constructors' names -- 3 sites, all in Widget.cls.
    assert_eq!(
        widget_edits.len(),
        3,
        "expected the class name plus both constructor names to be renamed: {widget_edits:?}"
    );
    assert!(widget_edits.iter().all(|e| e["newText"] == "Gadget"));

    let caller_edits = changes.get(caller_uri.as_str()).unwrap().as_array().unwrap();
    // `Widget w = new Widget();` -- the type reference and the `new` call.
    assert_eq!(caller_edits.len(), 2, "expected both usages in Caller.cls: {caller_edits:?}");

    assert_eq!(
        apply_edits(SRC, widget_edits),
        "public class Gadget {\n    public Gadget() { }\n    public Gadget(Integer x) { }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_SRC, caller_edits),
        "public class Caller {\n    public void run() { Gadget w = new Gadget(); }\n}\n"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Direct regression test for a real user report: renaming a local
/// variable to a *short* name left the file corrupted (`Integer x= 0;`
/// instead of `Integer x = 0;`) because the computed `TextEdit` range was
/// one character too wide, eating the space right after the old
/// (longer) identifier -- a bug invisible to a test that only checks
/// `newText`/edit count, since neither notices an extra swallowed
/// character. This test's assertion catches exactly that.
#[test]
fn renaming_a_local_variable_to_a_short_name_does_not_corrupt_the_file() {
    const SRC: &str = "public class Widget {\n    public void run() {\n        \
         Integer accountRecordTypeId = 0;\n        \
         accountRecordTypeId = accountRecordTypeId + 1;\n    }\n}\n";
    const EXPECTED: &str = "public class Widget {\n    public void run() {\n        \
         Integer x = 0;\n        \
         x = x + 1;\n    }\n}\n";
    let dir = write_fixture_dir("rename-short-name", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);

    let response = rename_request(&mut session, &widget_uri, SRC, "accountRecordTypeId = 0", "x");
    assert!(
        response.get("error").is_none(),
        "rename returned an error: {response:?}"
    );
    let edits = response["result"]["changes"][widget_uri.as_str()]
        .as_array()
        .unwrap_or_else(|| panic!("expected edits, got {response:?}"));
    assert_eq!(edits.len(), 3, "declaration + both usages: {edits:?}");
    assert_eq!(apply_edits(SRC, edits), EXPECTED);

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prepare_rename_refuses_a_method_that_overrides_a_base_class_method() {
    const BASE_SRC: &str = "public virtual class Base {\n    public virtual void greet() { }\n}\n";
    const DERIVED_SRC: &str =
        "public class Derived extends Base {\n    public override void greet() { }\n}\n";
    let dir = write_fixture_dir(
        "rename-override-refused",
        &[("Base.cls", BASE_SRC), ("Derived.cls", DERIVED_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let derived_uri = Url::from_file_path(dir.join("Derived.cls")).unwrap();

    let mut session = Session::start(&root_uri, &derived_uri, DERIVED_SRC);

    let (line, character) = position_of(DERIVED_SRC, "greet");
    let response = session.request(
        2,
        "textDocument/prepareRename",
        serde_json::json!({
            "textDocument": { "uri": derived_uri },
            "position": { "line": line, "character": character },
        }),
    );
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("expected prepareRename to refuse an override method: {response:?}"));
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("override"),
        "expected the refusal message to explain the override-chain risk: {message}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_refuses_an_invalid_new_identifier() {
    const SRC: &str = "public class Widget {\n    public Integer count;\n}\n";
    let dir = write_fixture_dir("rename-invalid-name", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);

    let response = rename_request(&mut session, &widget_uri, SRC, "count", "not a valid name");
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("expected rename to refuse an invalid identifier: {response:?}"));
    assert!(error["message"].as_str().unwrap().contains("valid Apex identifier"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real-corpus smoke test, matching this project's own convention of
/// running whole-NPSP-corpus assertions as normal (not `#[ignore]`d)
/// tests: `isValidField` is a plain `public static` method (no `virtual`/
/// `override` anywhere in its declaring class) with real cross-file call
/// sites, so this exercises `check_method_eligible`'s project-wide
/// override scan and `rename_edits`'s multi-file edit generation against
/// real, messy source -- not just a small hand-written fixture -- without
/// asserting an exact reference count that would go stale if NPSP's
/// upstream source changes.
#[test]
fn rename_a_real_npsp_method_touches_every_real_call_site_without_erroring() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let util_describe_path = root.join("force-app/main/default/classes/UTIL_Describe.cls");
    let root_uri = Url::from_file_path(&root).unwrap();
    let util_describe_uri = Url::from_file_path(&util_describe_path).unwrap();
    let src = std::fs::read_to_string(&util_describe_path).unwrap();

    let mut session = Session::start(&root_uri, &util_describe_uri, &src);

    let response = rename_request(&mut session, &util_describe_uri, &src, "isValidField", "isFieldValid");
    assert!(
        response.get("error").is_none(),
        "rename on a real NPSP method returned an error: {response:?}"
    );
    let changes = response["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("expected a WorkspaceEdit.changes map, got {response:?}"));
    assert!(
        changes.len() >= 5,
        "expected the rename to touch several real files across the NPSP corpus, got {}: {changes:?}",
        changes.len()
    );
    for edits in changes.values() {
        for edit in edits.as_array().unwrap() {
            assert_eq!(edit["newText"], "isFieldValid");
        }
    }

    session.shutdown();
}

#[test]
fn rename_refuses_a_name_that_collides_with_an_existing_member() {
    const SRC: &str =
        "public class Widget {\n    public Integer count;\n    public Integer total;\n}\n";
    let dir = write_fixture_dir("rename-collision", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);

    let response = rename_request(&mut session, &widget_uri, SRC, "count", "total");
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("expected rename to refuse a colliding name: {response:?}"));
    assert!(error["message"].as_str().unwrap().contains("collide"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
