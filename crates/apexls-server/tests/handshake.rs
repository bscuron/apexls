//! Protocol-level integration test: spawns the built `apexls-server`
//! binary as a real subprocess and drives it over stdio with actual
//! `Content-Length`-framed JSON-RPC, exactly as a real LSP client
//! would. This is the only way to test the server loop itself
//! (transport, framing, handshake sequencing) rather than just
//! `LanguageServer` method bodies in isolation.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn send(stdin: &mut impl Write, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stdin.flush().unwrap();
}

/// Reads one `Content-Length`-framed JSON-RPC message from `stdout`.
fn recv(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout before sending a full response");
        let line = line.trim_end();
        if line.is_empty() {
            break; // blank line ends the header block
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

#[test]
fn full_handshake_over_real_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_apexls-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn apexls-server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
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
            }
        }),
    );
    let response = recv(&mut stdout);
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none(),
        "initialize returned an error: {response:?}"
    );
    assert_eq!(
        response["result"]["capabilities"]["textDocumentSync"],
        serde_json::json!(1),
        "expected TextDocumentSyncKind::FULL (1), got: {response:?}"
    );
    assert_eq!(response["result"]["serverInfo"]["name"], "apexls");

    // 2. initialized (notification -- no response expected)
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );

    // 3. didOpen a document, then didChange it (both notifications --
    // just proving the server accepts them without erroring; the
    // in-memory document store isn't observable from outside yet).
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///Foo.cls",
                    "languageId": "apex",
                    "version": 1,
                    "text": "public class Foo { }"
                }
            }
        }),
    );
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": "file:///Foo.cls", "version": 2 },
                "contentChanges": [{ "text": "public class Foo { Integer x; }" }]
            }
        }),
    );

    // A request sent right after those notifications should still get
    // a well-formed response -- proves the server didn't wedge or drop
    // sync after processing document-sync notifications.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let response = recv(&mut stdout);
    assert_eq!(response["id"], 2);
    assert!(
        response.get("error").is_none(),
        "shutdown returned an error: {response:?}"
    );

    // 4. exit -- the server must terminate its process on this.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    let status = child.wait().expect("failed to wait on apexls-server");
    assert!(
        status.success(),
        "apexls-server did not exit cleanly after exit: {status:?}"
    );
}
