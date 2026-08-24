//! `textDocument/definition` on a `Resolution::SchemaObject` reference
//! (a SOQL `FROM <object>`) -- used to be an unconditional no-op:
//! `Backend::definition` only ever matched `Resolution::Resolved`/
//! `Resolution::Candidates`, so even a positively-identified local
//! custom object fell through to `_ => None`. Confirms it now points at
//! the object's real `.object-meta.xml` file. Follows
//! `hover_definition.rs`'s exact protocol-level harness pattern (spawn
//! the real binary, drive it over real stdio, wait for "rebuild
//! complete" before sending a position-based request).

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
    let _ = std::fs::remove_dir_all(&dir);
    for (file_name, src) in files {
        let path = dir.join(file_name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    dir
}

/// `(line, character)` of the start of `needle`'s first occurrence in
/// `src` -- computed rather than hand-counted (`src` is ASCII-only in
/// every fixture here, so byte offset and UTF-16 character offset
/// coincide) so a fixture's exact text can change without silently
/// desyncing a hardcoded position.
fn position_of(src: &str, needle: &str) -> (u32, u32) {
    let byte_offset = src.find(needle).expect("needle not found in fixture source");
    let before = &src[..byte_offset];
    let line = before.matches('\n').count() as u32;
    let character = match before.rfind('\n') {
        Some(nl) => (byte_offset - nl - 1) as u32,
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
    fn start(main_uri: &Url, main_src: &str, root_uri: &Url) -> Self {
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
                        "uri": main_uri,
                        "languageId": "apex",
                        "version": 1,
                        "text": main_src,
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

#[test]
fn definition_on_a_soql_from_object_points_at_its_object_meta_xml() {
    let query_src =
        "public class Foo {\n    public void run() {\n        List<SObject> rows = [SELECT Id FROM My_Object__c];\n    }\n}\n";
    let object_meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <label>My Object</label>
</CustomObject>"#;

    let dir = write_fixture_dir(
        "schema-goto-def",
        &[
            ("Foo.cls", query_src),
            (
                "objects/My_Object__c/My_Object__c.object-meta.xml",
                object_meta,
            ),
        ],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let object_meta_uri =
        Url::from_file_path(dir.join("objects/My_Object__c/My_Object__c.object-meta.xml"))
            .unwrap();

    let mut session = Session::start(&foo_uri, query_src, &root_uri);

    let (line, character) = position_of(query_src, "My_Object__c");
    let response = session.request(
        2,
        "textDocument/definition",
        serde_json::json!({
            "textDocument": { "uri": foo_uri },
            "position": { "line": line, "character": character },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "definition returned an error: {response:?}"
    );
    let result = &response["result"];
    let uri = result["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a scalar Location, got {response:?}"));
    assert_eq!(uri, object_meta_uri.as_str());

    session.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
