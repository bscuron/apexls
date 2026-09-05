//! Protocol-level verification of `textDocument/semanticTokens/full`+`/range`
//! (ticket 01, `.scratch/apex-lsp-gaps/issues/01-semantic-tokens-implement.md`):
//! `capabilities::collect_tokens`'s walk over the binder's already-resolved
//! `SymbolTable`/`Resolution` data. Follows `inlay_hints.rs`'s exact
//! real-stdio harness pattern (spawn the real binary, drive it over real
//! stdio, wait for the background rebuild's "rebuild complete" stderr line
//! before sending a request), extended with a decoder for the delta-encoded
//! `SemanticTokens.data` array and the legend captured from `initialize`'s
//! own response -- so a token assertion reads as `(line, col, length, type,
//! modifiers)`, not raw `u32`s.

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

/// One decoded token: `(line, start_col, length, type name, modifier names)`.
type Token = (u32, u32, u32, String, Vec<String>);

struct Session {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    rebuild_rx: mpsc::Receiver<String>,
    legend_types: Vec<String>,
    legend_modifiers: Vec<String>,
}

impl Session {
    fn start_with_text(foo_uri: &Url, root_uri: &Url, text: &str) -> Self {
        Self::start_with_text_and_encoding(foo_uri, root_uri, text, None)
    }

    /// `encodings`: the `general.positionEncodings` array to offer at
    /// `initialize`, or `None` to offer nothing (negotiates UTF-16, the
    /// LSP-mandated default -- see `line_index::PositionEncoding::negotiate`).
    fn start_with_text_and_encoding(
        foo_uri: &Url,
        root_uri: &Url,
        text: &str,
        encodings: Option<&[&str]>,
    ) -> Self {
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

        let capabilities = match encodings {
            Some(encodings) => serde_json::json!({ "general": { "positionEncodings": encodings } }),
            None => serde_json::json!({}),
        };
        send(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": null,
                    "rootUri": null,
                    "capabilities": capabilities,
                    "workspaceFolders": [{ "uri": root_uri, "name": "fixture" }],
                }
            }),
        );
        let response = recv(&mut stdout);
        assert!(response.get("error").is_none(), "initialize returned an error: {response:?}");
        let legend = &response["result"]["capabilities"]["semanticTokensProvider"]["legend"];
        let legend_types: Vec<String> = legend["tokenTypes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a semanticTokensProvider legend: {response:?}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let legend_modifiers: Vec<String> = legend["tokenModifiers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

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
                        "text": text,
                    }
                }
            }),
        );

        let mut session = Session {
            child,
            stdin,
            stdout,
            rebuild_rx,
            legend_types,
            legend_modifiers,
        };
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
        send(&mut self.stdin, &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        recv(&mut self.stdout)
    }

    fn semantic_tokens_full(&mut self, id: i64, uri: &Url) -> Vec<Token> {
        let response = self.request(
            id,
            "textDocument/semanticTokens/full",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        );
        assert!(response.get("error").is_none(), "semanticTokens/full returned an error: {response:?}");
        self.decode(&response)
    }

    fn semantic_tokens_range(&mut self, id: i64, uri: &Url, start: (u32, u32), end: (u32, u32)) -> Vec<Token> {
        let response = self.request(
            id,
            "textDocument/semanticTokens/range",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start.0, "character": start.1 },
                    "end": { "line": end.0, "character": end.1 },
                },
            }),
        );
        assert!(response.get("error").is_none(), "semanticTokens/range returned an error: {response:?}");
        self.decode(&response)
    }

    /// Delta-decodes `response["result"]["data"]` into absolute
    /// `(line, col, length, type, modifiers)` tuples per LSP 3.17's
    /// encoding (five `u32`s per token; `delta_line`/`delta_start` are
    /// relative to the previous token's own absolute start, with
    /// `delta_start` resetting to an absolute column whenever
    /// `delta_line != 0`).
    fn decode(&self, response: &serde_json::Value) -> Vec<Token> {
        let data = response["result"]["data"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a SemanticTokens.data array, got {response:?}"));
        assert_eq!(data.len() % 5, 0, "SemanticTokens.data length must be a multiple of 5: {data:?}");
        let mut out = Vec::new();
        let (mut line, mut col) = (0u32, 0u32);
        for chunk in data.chunks_exact(5) {
            let delta_line = chunk[0].as_u64().unwrap() as u32;
            let delta_start = chunk[1].as_u64().unwrap() as u32;
            let length = chunk[2].as_u64().unwrap() as u32;
            let type_idx = chunk[3].as_u64().unwrap() as usize;
            let mod_bits = chunk[4].as_u64().unwrap() as u32;
            if delta_line == 0 {
                col += delta_start;
            } else {
                line += delta_line;
                col = delta_start;
            }
            let type_name = self.legend_types[type_idx].clone();
            let modifiers: Vec<String> = self
                .legend_modifiers
                .iter()
                .enumerate()
                .filter(|&(i, _)| mod_bits & (1 << i) != 0)
                .map(|(_, m)| m.clone())
                .collect();
            out.push((line, col, length, type_name, modifiers));
        }
        out
    }

    fn shutdown(mut self) {
        send(&mut self.stdin, &serde_json::json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null }));
        let response = recv(&mut self.stdout);
        assert!(response.get("error").is_none(), "shutdown returned an error: {response:?}");
        send(&mut self.stdin, &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
        let status = self.child.wait().expect("failed to wait on apexls-server");
        assert!(status.success(), "apexls-server did not exit cleanly after exit: {status:?}");
    }
}

fn token(line: u32, col: u32, len: u32, ty: &str, mods: &[&str]) -> Token {
    (line, col, len, ty.to_string(), mods.iter().map(|s| s.to_string()).collect())
}

#[test]
fn class_declaration_emits_class_with_declaration_bit() {
    let src = "public class Foo {\n}\n";
    let dir = write_fixture_dir("class-decl", &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let mut session = Session::start_with_text(&foo_uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &foo_uri);
    assert_eq!(tokens, vec![token(0, 13, 3, "class", &["declaration", "public"])], "{tokens:?}");
    session.shutdown();
}

#[test]
fn interface_declaration_and_implementing_class_reference() {
    // Real Apex allows exactly one top-level type per file, so the
    // interface and its implementing class each need their own file.
    let c_src = "public class C implements I {\n}\n";
    let dir = write_fixture_dir(
        "interface-decl",
        &[("I.cls", "public interface I {\n}\n"), ("C.cls", c_src)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, c_src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(0, 13, 1, "class", &["declaration", "public"])),
        "expected C's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(0, 26, 1, "interface", &["public"])),
        "expected the `implements I` reference with no declaration bit: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn method_and_constructor_declarations() {
    let src = "public class C {\n    public C() {}\n    public void run() {}\n}\n";
    let dir = write_fixture_dir("method-ctor", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(1, 11, 1, "method", &["declaration", "public"])),
        "expected the constructor's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(2, 16, 3, "method", &["declaration", "public"])),
        "expected run()'s own declaration: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn field_and_property_declarations() {
    let src = "public class C {\n    private static final Integer X = 0;\n    private Integer Y { get; set; }\n}\n";
    let dir = write_fixture_dir("field-property", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(1, 33, 1, "property", &["declaration", "readonly", "static", "private"])),
        "expected X's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(2, 20, 1, "property", &["declaration", "private"])),
        "expected Y's own declaration: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn parameter_and_local_variable_declarations_and_references() {
    let src = "public class C {\n    void run(Integer p) {\n        Integer q = p;\n    }\n}\n";
    let dir = write_fixture_dir("param-local", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(1, 21, 1, "parameter", &["declaration", "private"])),
        "expected p's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(2, 16, 1, "variable", &["declaration", "private"])),
        "expected q's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(2, 20, 1, "parameter", &["private"])),
        "expected the reference to p with no declaration bit: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn enum_declaration_emits_enum_and_enum_member() {
    let src = "public enum Season {\n    WINTER, SUMMER\n}\n";
    let dir = write_fixture_dir("enum-decl", &[("Season.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("Season.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(0, 12, 6, "enum", &["declaration", "public"])),
        "expected Season's own declaration: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(1, 4, 6, "enumMember", &["declaration", "public"])),
        "expected WINTER's own declaration: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn static_call_to_a_project_local_method() {
    let b_src = "public class B {\n    void run() { A.go(); }\n}\n";
    let dir = write_fixture_dir(
        "static-call",
        &[("A.cls", "public class A {\n    public static void go() {}\n}\n"), ("B.cls", b_src)],
    );
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("B.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, b_src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(1, 17, 1, "class", &["public"])),
        "expected the A receiver reference, mirroring A's own public visibility: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(1, 19, 2, "method", &["static", "public"])),
        "expected the go() call resolving to A's static method: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn stdlib_call_emits_class_and_method_with_default_library() {
    let src = "public class C {\n    void run() { System.debug('hi'); }\n}\n";
    let dir = write_fixture_dir("stdlib-call", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.contains(&token(1, 17, 6, "class", &["defaultLibrary"])),
        "expected the System receiver: {tokens:?}"
    );
    assert!(
        tokens.contains(&token(1, 24, 5, "method", &["defaultLibrary"])),
        "expected the debug() call: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn stdlib_property_vs_method_disambiguation() {
    let src = "public class C {\n    void run() { 'x'.toUpperCase(); }\n}\n";
    let dir = write_fixture_dir("stdlib-method", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.iter().any(|t| t.3 == "method" && t.4.contains(&"defaultLibrary".to_string())),
        "expected toUpperCase() to resolve as a stdlib method, not a property: {tokens:?}"
    );
    assert!(
        !tokens.iter().any(|t| t.3 == "property" && t.4.contains(&"defaultLibrary".to_string())),
        "did not expect any stdlib property token in this fixture: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn schema_reference_emits_type_and_property_with_default_library() {
    let src = "public class C {\n    void run() { Account a = new Account(); a.Name = 'x'; }\n}\n";
    let dir = write_fixture_dir("schema-ref", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    assert!(
        tokens.iter().any(|t| t.3 == "type" && t.4.contains(&"defaultLibrary".to_string())),
        "expected an Account type reference: {tokens:?}"
    );
    assert!(
        tokens.iter().any(|t| t.3 == "property" && t.4.contains(&"defaultLibrary".to_string())),
        "expected a Name field reference: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn unresolved_reference_emits_no_token() {
    let src = "public class C {\n    void run() { nonExistentThing(); }\n}\n";
    let dir = write_fixture_dir("unresolved", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let tokens = session.semantic_tokens_full(2, &uri);
    // Line 1, columns 17..34 is `nonExistentThing` -- no token may start
    // anywhere in that span.
    assert!(
        !tokens.iter().any(|t| t.0 == 1 && (17..34).contains(&t.1)),
        "expected no token for the unresolved reference: {tokens:?}"
    );
    session.shutdown();
}

#[test]
fn range_request_returns_a_strict_subset_filtered_by_range() {
    let src = "public class C {\n    public void a() {}\n    public void b() {}\n    public void c() {}\n}\n";
    let dir = write_fixture_dir("range-mode", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let full = session.semantic_tokens_full(2, &uri);
    assert!(full.len() >= 4, "expected at least the class + 3 methods: {full:?}");

    let ranged = session.semantic_tokens_range(3, &uri, (2, 0), (2, 100));
    assert!(!ranged.is_empty(), "expected at least b()'s own token in the requested range");
    assert!(
        ranged.iter().all(|t| t.0 == 2),
        "expected every returned token to fall on line 2 only: {ranged:?}"
    );
    assert!(
        ranged.iter().any(|t| t.3 == "method"),
        "expected b()'s own method token: {ranged:?}"
    );
    session.shutdown();
}

#[test]
fn semantic_tokens_data_is_always_a_multiple_of_five() {
    let src = "public class C {\n    private Integer x;\n    void run() { x = 1; System.debug(x); }\n}\n";
    let dir = write_fixture_dir("multiple-of-five", &[("C.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("C.cls")).unwrap();
    let mut session = Session::start_with_text(&uri, &root_uri, src);

    let response = session.request(
        2,
        "textDocument/semanticTokens/full",
        serde_json::json!({ "textDocument": { "uri": uri } }),
    );
    let data = response["result"]["data"].as_array().unwrap();
    assert_eq!(data.len() % 5, 0, "SemanticTokens.data must always be a multiple of 5: {data:?}");
    assert!(!data.is_empty());
    session.shutdown();
}

/// Encoding sanity: a non-ASCII character (`é`, one UTF-16 code unit but
/// two UTF-8 bytes) sits earlier on the *same* line as a declaration.
/// Under UTF-8 byte counting the declaration would land one column later
/// than under UTF-16 code-unit counting -- exactly the divergence that
/// would silently appear if the walker ever counted raw bytes instead of
/// routing through `line_index::LineIndex`'s encoding-aware conversion.
#[test]
fn token_offsets_respect_the_negotiated_utf16_encoding() {
    let src = "public class Foo {\n    /* café */ Integer x;\n}\n";
    let dir = write_fixture_dir("encoding-sanity", &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    // No `general.positionEncodings` offered -- negotiates UTF-16, the
    // LSP-mandated default (`PositionEncoding::negotiate(None)`).
    let mut session = Session::start_with_text_and_encoding(&uri, &root_uri, src, None);

    let tokens = session.semantic_tokens_full(2, &uri);
    // "    /* café */ Integer x;" -- every character up to and including
    // `é` is exactly one UTF-16 code unit, so `x`'s declaration (a field,
    // declared directly in the class body) starts at UTF-16 column 23
    // (it would be 24 under naive UTF-8 byte counting, since `é` alone
    // costs 2 bytes there).
    assert!(
        tokens.contains(&token(1, 23, 1, "property", &["declaration", "private"])),
        "expected x's declaration at UTF-16 column 23 (not 24, which would mean byte-offsets leaked through): {tokens:?}"
    );
    session.shutdown();
}
