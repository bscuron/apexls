//! Protocol-level verification of the three parameter-list `textDocument/codeAction`s
//! (`capabilities::parameter_reorder_actions`): "Rotate parameters left",
//! "Rotate parameters right", and "Remove parameter '<name>'". Follows
//! `rename.rs`'s exact `Session` harness (spawn the real binary, drive it
//! over real stdio, wait for the background rebuild's "rebuild complete"
//! stderr line before sending a position-based request) -- this feature
//! reuses the same `references_to`-driven multi-file `WorkspaceEdit`
//! shape rename does, so this file focuses on what's new: rewriting a
//! declaration's `FormalParamList` and every call site's `ArgList` in
//! lockstep, and the eligibility gate (v1 only offers these actions for a
//! non-virtual, non-overloaded, non-interface method).

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

/// Applies every edit in `edits` (one file's worth) to `src`, sorted by
/// descending start offset so earlier edits' ranges stay valid as later
/// (in source order) ones are applied first -- unlike `rename.rs`'s own
/// `apply_edits`, this works off absolute byte offsets (via `src`'s own
/// line boundaries) rather than requiring every edit to be single-line,
/// since a rewritten parameter/argument list can span more columns than a
/// single identifier.
fn apply_edits(src: &str, edits: &[serde_json::Value]) -> String {
    let mut line_starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let to_offset = |line: u64, character: u64| line_starts[line as usize] + character as usize;
    let mut ranges: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            let start = to_offset(
                e["range"]["start"]["line"].as_u64().unwrap(),
                e["range"]["start"]["character"].as_u64().unwrap(),
            );
            let end = to_offset(
                e["range"]["end"]["line"].as_u64().unwrap(),
                e["range"]["end"]["character"].as_u64().unwrap(),
            );
            (start, end, e["newText"].as_str().unwrap().to_string())
        })
        .collect();
    ranges.sort_by_key(|&(start, ..)| std::cmp::Reverse(start));
    let mut result = src.to_string();
    for (start, end, new_text) in ranges {
        result = format!("{}{}{}", &result[..start], new_text, &result[end..]);
    }
    result
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

fn code_action_request(session: &mut Session, uri: &Url, src: &str, needle: &str) -> serde_json::Value {
    let (line, character) = position_of(src, needle);
    session.request(
        2,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character },
            },
            "context": { "diagnostics": [] },
        }),
    )
}

fn action_named<'a>(actions: &'a [serde_json::Value], title: &str) -> &'a serde_json::Value {
    actions
        .iter()
        .find(|a| a["title"] == serde_json::json!(title))
        .unwrap_or_else(|| panic!("expected an action titled {title:?} among {actions:?}"))
}

#[test]
fn rotate_left_right_and_remove_rewrite_the_declaration_and_every_call_site() {
    const WIDGET_SRC: &str = "public class Widget {\n    \
         public void configure(String a, String b, String c) {\n        \
             System.debug(a + b + c);\n    \
         }\n\
         }\n";
    const CALLER_A_SRC: &str =
        "public class CallerA {\n    public void run() { new Widget().configure('x', 'y', 'z'); }\n}\n";
    const CALLER_B_SRC: &str =
        "public class CallerB {\n    public void run() { new Widget().configure('p', 'q', 'r'); }\n}\n";
    let dir = write_fixture_dir(
        "param-reorder-basic",
        &[
            ("Widget.cls", WIDGET_SRC),
            ("CallerA.cls", CALLER_A_SRC),
            ("CallerB.cls", CALLER_B_SRC),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();
    let caller_a_uri = Url::from_file_path(dir.join("CallerA.cls")).unwrap();
    let caller_b_uri = Url::from_file_path(dir.join("CallerB.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, WIDGET_SRC);

    let response = code_action_request(&mut session, &widget_uri, WIDGET_SRC, "String b");
    assert!(response.get("error").is_none(), "codeAction returned an error: {response:?}");
    let actions = response["result"].as_array().expect("expected a code action array");
    assert_eq!(actions.len(), 3, "expected all three actions for a 3-param method: {actions:?}");

    let rotate_left = action_named(actions, "Rotate parameters left");
    assert_eq!(rotate_left["kind"], serde_json::json!("refactor.rewrite"));
    let changes = rotate_left["edit"]["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 3, "expected an edit in all three files: {changes:?}");
    assert_eq!(
        apply_edits(WIDGET_SRC, changes[widget_uri.as_str()].as_array().unwrap()),
        "public class Widget {\n    public void configure(String b, String c, String a) {\n        \
         System.debug(a + b + c);\n    }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_A_SRC, changes[caller_a_uri.as_str()].as_array().unwrap()),
        "public class CallerA {\n    public void run() { new Widget().configure('y', 'z', 'x'); }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_B_SRC, changes[caller_b_uri.as_str()].as_array().unwrap()),
        "public class CallerB {\n    public void run() { new Widget().configure('q', 'r', 'p'); }\n}\n"
    );

    let rotate_right = action_named(actions, "Rotate parameters right");
    let changes = rotate_right["edit"]["changes"].as_object().unwrap();
    assert_eq!(
        apply_edits(WIDGET_SRC, changes[widget_uri.as_str()].as_array().unwrap()),
        "public class Widget {\n    public void configure(String c, String a, String b) {\n        \
         System.debug(a + b + c);\n    }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_A_SRC, changes[caller_a_uri.as_str()].as_array().unwrap()),
        "public class CallerA {\n    public void run() { new Widget().configure('z', 'x', 'y'); }\n}\n"
    );

    let remove = action_named(actions, "Remove parameter 'b'");
    let changes = remove["edit"]["changes"].as_object().unwrap();
    assert_eq!(
        apply_edits(WIDGET_SRC, changes[widget_uri.as_str()].as_array().unwrap()),
        "public class Widget {\n    public void configure(String a, String c) {\n        \
         System.debug(a + b + c);\n    }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_A_SRC, changes[caller_a_uri.as_str()].as_array().unwrap()),
        "public class CallerA {\n    public void run() { new Widget().configure('x', 'z'); }\n}\n"
    );
    assert_eq!(
        apply_edits(CALLER_B_SRC, changes[caller_b_uri.as_str()].as_array().unwrap()),
        "public class CallerB {\n    public void run() { new Widget().configure('p', 'r'); }\n}\n"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_single_parameter_method_only_offers_remove() {
    const SRC: &str = "public class Widget {\n    public void configure(String a) { }\n}\n";
    let dir = write_fixture_dir("param-reorder-single", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);
    let response = code_action_request(&mut session, &widget_uri, SRC, "String a");
    let actions = response["result"].as_array().expect("expected a code action array");
    assert_eq!(actions.len(), 1, "expected only Remove for a single-parameter method: {actions:?}");
    assert_eq!(actions[0]["title"], serde_json::json!("Remove parameter 'a'"));

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_overloaded_method_offers_no_parameter_actions() {
    const SRC: &str = "public class Widget {\n    \
         public void configure(String a, String b) { }\n    \
         public void configure(String a) { }\n\
         }\n";
    let dir = write_fixture_dir("param-reorder-overload", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);
    let response = code_action_request(&mut session, &widget_uri, SRC, "String a, String b");
    let actions = response["result"].as_array();
    assert!(
        actions.is_none_or(|a| a.is_empty()),
        "expected no parameter actions for an overloaded method: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_interface_methods_parameters_offer_no_actions() {
    const SRC: &str = "public interface Widget {\n    void configure(String a, String b);\n}\n";
    let dir = write_fixture_dir("param-reorder-interface", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);
    let response = code_action_request(&mut session, &widget_uri, SRC, "String a");
    let actions = response["result"].as_array();
    assert!(
        actions.is_none_or(|a| a.is_empty()),
        "expected no parameter actions for an interface method: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_override_methods_parameters_offer_no_actions() {
    // `Derived.configure`'s own params (`x`/`y`) are named differently
    // than `Base.configure`'s (`a`/`b`) purely so the two `String x`-shaped
    // needles below are unambiguous in the fixture source -- Apex overload/
    // override matching only cares about parameter *types*, never names.
    const SRC: &str = "public virtual class Base {\n    \
         public virtual void configure(String a, String b) { }\n\
         }\n\
         public class Derived extends Base {\n    \
         public override void configure(String x, String y) { }\n\
         }\n";
    let dir = write_fixture_dir("param-reorder-override", &[("Widget.cls", SRC)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let widget_uri = Url::from_file_path(dir.join("Widget.cls")).unwrap();

    let mut session = Session::start(&root_uri, &widget_uri, SRC);
    // The *override* itself (`Derived.configure`) is what's under the
    // cursor here -- `method_override_chain_reason` refuses it directly
    // via `is_override`, distinct from `Base.configure`, which would be
    // refused via the separate "overridden by a subclass" check instead.
    let response = code_action_request(&mut session, &widget_uri, SRC, "String x");
    let actions = response["result"].as_array();
    assert!(
        actions.is_none_or(|a| a.is_empty()),
        "expected no parameter actions for an override method: {response:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
