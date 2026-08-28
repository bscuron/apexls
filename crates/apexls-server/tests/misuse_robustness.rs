//! Protocol-level robustness tests: the server must never panic, hang, or
//! answer wrongly when it's driven in ways a well-behaved editor never
//! would but a misconfigured/edge-case one might -- opened with no
//! project at all, pointed at a workspace root that isn't (or doesn't
//! even exist as) a real Apex project, asked about a file whose extension
//! or content doesn't match what it claims to be, or queried with a
//! position/URI that doesn't correspond to anything real. Every one of
//! these must come back as a clean, empty/`null` LSP response -- never a
//! JSON-RPC error, and never a dead or hung process. Follows
//! `signature_help.rs`'s exact `Session` harness pattern, generalized
//! (`Session::start`) to support omitting the workspace root entirely and
//! opening files that may or may not exist on disk.

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
    /// `root_uri: None` means the client offers neither `workspaceFolders`
    /// nor (deprecated) `rootUri` at all -- real "used outside of any
    /// project" single-file mode, per `Backend::initialize`'s own root-
    /// resolution fallback chain.
    fn start(root_uri: Option<&Url>, files: &[(&Url, &str)]) -> Self {
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

        let workspace_folders = root_uri
            .map(|uri| serde_json::json!([{ "uri": uri, "name": "fixture" }]))
            .unwrap_or(serde_json::Value::Null);
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
                    "workspaceFolders": workspace_folders,
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
        };
        // Only a resolved root ever spawns the background rebuild worker
        // (`Backend::initialized`) -- with no root at all, `bind.program`
        // stays permanently `None` and every request already returns
        // synchronously, so there is no "rebuild complete" line to ever
        // wait for.
        if root_uri.is_some() {
            session.wait_for_rebuild();
        }
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

    fn hover(&mut self, id: i64, uri: &Url, line: u32, character: u32) -> serde_json::Value {
        self.request(
            id,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    }

    fn completion(&mut self, id: i64, uri: &Url, line: u32, character: u32) -> serde_json::Value {
        self.request(
            id,
            "textDocument/completion",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    }

    fn document_symbol(&mut self, id: i64, uri: &Url) -> serde_json::Value {
        self.request(
            id,
            "textDocument/documentSymbol",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        )
    }

    fn request(&mut self, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        recv(&mut self.stdout)
    }

    /// Confirms the process is still alive and answering requests --
    /// the real point of most tests in this file: not just "this one
    /// request returned `null`," but "the server didn't die or wedge as
    /// a result." Sends a request against a small independent buffer and
    /// asserts it still gets a clean (if possibly `null`) response.
    fn assert_still_alive(&mut self, id: i64) {
        let probe_uri = Url::parse("file:///apexls-alive-probe/Probe.cls").unwrap();
        let response = self.hover(id, &probe_uri, 0, 0);
        assert!(
            response.get("error").is_none(),
            "server answered with an error instead of a clean response after the misuse case: {response:?}"
        );
    }

    fn shutdown(mut self) {
        send(
            &mut self.stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 999, "method": "shutdown", "params": null }),
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

const VALID_CLASS_SRC: &str =
    "public class Foo {\n    public Integer bar;\n    public void run() { Integer x = bar; }\n}\n";

/// No `workspaceFolders`, no (deprecated) `rootUri` -- the client never
/// tells the server what project it's in at all. `Backend::initialize`'s
/// own doc comment already calls this out as a real, supported mode
/// (`self.root` stays `None`), not just an oversight -- this pins that
/// every capability actually degrades to a clean `null` rather than
/// erroring, hanging (there is no rebuild to wait for), or panicking.
#[test]
fn no_workspace_folder_or_root_uri_returns_null_gracefully() {
    let foo_uri = Url::parse("file:///apexls-no-root/Foo.cls").unwrap();
    let mut session = Session::start(None, &[(&foo_uri, VALID_CLASS_SRC)]);

    let hover = session.hover(2, &foo_uri, 1, 20);
    assert!(hover.get("error").is_none(), "hover errored: {hover:?}");
    assert!(hover["result"].is_null(), "expected null hover with no project: {hover:?}");

    let completion = session.completion(3, &foo_uri, 2, 10);
    assert!(completion.get("error").is_none(), "completion errored: {completion:?}");
    assert!(
        completion["result"].is_null(),
        "expected null completion with no project: {completion:?}"
    );

    session.shutdown();
}

/// `rootUri`/`workspaceFolders` name a directory that was never created
/// on disk at all -- a stale or mistyped workspace, or one deleted out
/// from under an already-running server. `apex_discover::discover`'s own
/// doc comment says unreadable entries are silently skipped rather than
/// failing the walk; this confirms that holds for the root itself too,
/// not just subdirectories -- the server should still finish a (trivially
/// empty) rebuild and keep answering requests cleanly.
#[test]
fn a_nonexistent_workspace_root_does_not_crash_or_hang() {
    let missing_root = std::env::temp_dir().join(format!(
        "apexls-server-nonexistent-root-{}",
        std::process::id()
    ));
    let root_uri = Url::from_file_path(&missing_root).unwrap();
    let mut session = Session::start(Some(&root_uri), &[]);

    session.assert_still_alive(2);
    session.shutdown();
}

/// The workspace root is a real directory, but not an Apex project at
/// all (no `.cls`/`.trigger` files, just an unrelated file) -- confirms
/// this doesn't prevent a *separately opened*, on-disk, real `.cls` file
/// under that same root from binding and resolving completely normally.
#[test]
fn a_project_root_with_no_apex_files_still_binds_a_real_file_opened_in_it() {
    let dir = write_fixture_dir(
        "misuse-empty-project",
        &[("readme.txt", "not an apex project"), ("Foo.cls", VALID_CLASS_SRC)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start(Some(&root_uri), &[(&foo_uri, VALID_CLASS_SRC)]);

    // `bar` at line 2, inside `Integer x = bar;`.
    let hover = session.hover(2, &foo_uri, 2, 32);
    assert!(hover.get("error").is_none(), "hover errored: {hover:?}");
    assert!(
        !hover["result"].is_null(),
        "a real file under a non-project root should still hover normally: {hover:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file with a non-Apex extension, opened alongside a real `.cls` file
/// in the same project -- `apex_discover` only ever considers
/// `.cls`/`.trigger` extensions, so this file is never a bind candidate
/// no matter what's in it. Confirms requesting anything about it returns
/// `null` rather than erroring, and that its presence doesn't poison the
/// bind for the real file sitting right next to it.
#[test]
fn a_non_apex_extension_file_returns_null_and_does_not_affect_other_files() {
    let dir = write_fixture_dir(
        "misuse-wrong-extension",
        &[
            ("Foo.cls", VALID_CLASS_SRC),
            ("Main.java", "public class Main { public static void main(String[] a) {} }"),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let java_uri = Url::from_file_path(dir.join("Main.java")).unwrap();
    let mut session = Session::start(
        Some(&root_uri),
        &[
            (&foo_uri, VALID_CLASS_SRC),
            (&java_uri, "public class Main { public static void main(String[] a) {} }"),
        ],
    );

    let java_hover = session.hover(2, &java_uri, 0, 15);
    assert!(java_hover.get("error").is_none(), "hover on the .java file errored: {java_hover:?}");
    assert!(
        java_hover["result"].is_null(),
        "a non-Apex-extension file should never be bound: {java_hover:?}"
    );

    let foo_hover = session.hover(3, &foo_uri, 2, 32);
    assert!(foo_hover.get("error").is_none(), "hover on Foo.cls errored: {foo_hover:?}");
    assert!(
        !foo_hover["result"].is_null(),
        "the real .cls file should be unaffected by the unrelated .java file: {foo_hover:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.cls` file whose content isn't Apex at all (or any recognizable
/// language) -- `apex-parser`'s own module doc comment guarantees no
/// panic on malformed input, only a best-effort tree plus recorded
/// errors; this exercises that guarantee through the whole real server,
/// not just the parser crate in isolation, and confirms a garbage file
/// doesn't take down binding for a real, valid file alongside it.
#[test]
fn a_cls_file_with_completely_garbage_content_does_not_crash_the_server() {
    let garbage = "{{{ this is not apex at all !!! 0xDEADBEEF \0\0\0 <<<>>> ]][[ ";
    let dir = write_fixture_dir(
        "misuse-garbage-content",
        &[("Foo.cls", VALID_CLASS_SRC), ("Garbage.cls", garbage)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let garbage_uri = Url::from_file_path(dir.join("Garbage.cls")).unwrap();
    let mut session = Session::start(
        Some(&root_uri),
        &[(&foo_uri, VALID_CLASS_SRC), (&garbage_uri, garbage)],
    );

    // Every position in the garbage file, not just one -- a parser bug
    // is more likely to surface as an out-of-bounds/off-by-one panic at
    // a specific offset than a blanket failure.
    for character in 0..garbage.len() as u32 {
        let response = session.hover(2, &garbage_uri, 0, character);
        assert!(
            response.get("error").is_none(),
            "hover at character {character} of garbage content errored: {response:?}"
        );
    }
    let completion = session.completion(3, &garbage_uri, 0, 5);
    assert!(completion.get("error").is_none(), "completion on garbage content errored: {completion:?}");
    let doc_symbol = session.document_symbol(4, &garbage_uri);
    assert!(doc_symbol.get("error").is_none(), "documentSymbol on garbage content errored: {doc_symbol:?}");

    let foo_hover = session.hover(5, &foo_uri, 2, 32);
    assert!(foo_hover.get("error").is_none(), "hover on Foo.cls errored: {foo_hover:?}");
    assert!(
        !foo_hover["result"].is_null(),
        "a real valid file should still resolve normally alongside a garbage one: {foo_hover:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.cls` file containing `trigger`-shaped content, and a `.trigger`
/// file containing `class`-shaped content -- `apex-binder` dispatches
/// which grammar entry point to parse with purely by file extension
/// (`crates/apex-binder/src/lib.rs`), so a real mismatch here just means
/// parsing the wrong grammar against the content, not a crash.
#[test]
fn extension_content_mismatch_does_not_crash() {
    let backwards_trigger = "trigger FooTrigger on Account (before insert) { }\n";
    let backwards_class = "public class NotReallyATrigger { public void run() {} }\n";
    let dir = write_fixture_dir(
        "misuse-extension-mismatch",
        &[
            ("Foo.cls", VALID_CLASS_SRC),
            ("BackwardsClass.cls", backwards_trigger),
            ("BackwardsTrigger.trigger", backwards_class),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let backwards_class_uri = Url::from_file_path(dir.join("BackwardsClass.cls")).unwrap();
    let backwards_trigger_uri = Url::from_file_path(dir.join("BackwardsTrigger.trigger")).unwrap();
    let mut session = Session::start(
        Some(&root_uri),
        &[
            (&foo_uri, VALID_CLASS_SRC),
            (&backwards_class_uri, backwards_trigger),
            (&backwards_trigger_uri, backwards_class),
        ],
    );

    for uri in [&backwards_class_uri, &backwards_trigger_uri] {
        let response = session.hover(2, uri, 0, 5);
        assert!(response.get("error").is_none(), "hover on a mismatched file errored: {response:?}");
    }

    session.assert_still_alive(3);
    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A position far past the end of a small, valid file -- `LineIndex`
/// must clamp/reject this to `None` rather than indexing past the real
/// text.
#[test]
fn a_position_far_past_the_end_of_a_file_returns_null_not_a_panic() {
    let dir = write_fixture_dir("misuse-out-of-bounds-position", &[("Foo.cls", VALID_CLASS_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start(Some(&root_uri), &[(&foo_uri, VALID_CLASS_SRC)]);

    let hover = session.hover(2, &foo_uri, 99_999, 99_999);
    assert!(hover.get("error").is_none(), "hover past EOF errored: {hover:?}");
    assert!(hover["result"].is_null(), "expected null hover past EOF: {hover:?}");

    let completion = session.completion(3, &foo_uri, 99_999, 99_999);
    assert!(completion.get("error").is_none(), "completion past EOF errored: {completion:?}");
    assert!(
        completion["result"].is_null(),
        "expected null completion past EOF: {completion:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A request against a URI that was never opened and doesn't exist on
/// disk -- `file_id` must return `None` rather than the handler assuming
/// the file is somehow present.
#[test]
fn a_request_for_an_unknown_uri_returns_null() {
    let dir = write_fixture_dir("misuse-unknown-uri", &[("Foo.cls", VALID_CLASS_SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let unknown_uri = Url::from_file_path(dir.join("DoesNotExist.cls")).unwrap();
    let mut session = Session::start(Some(&root_uri), &[(&foo_uri, VALID_CLASS_SRC)]);

    let hover = session.hover(2, &unknown_uri, 0, 0);
    assert!(hover.get("error").is_none(), "hover on an unknown uri errored: {hover:?}");
    assert!(hover["result"].is_null(), "expected null hover for an unknown uri: {hover:?}");

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
