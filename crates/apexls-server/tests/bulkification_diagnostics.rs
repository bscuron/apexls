//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! DML/SOQL-inside-a-loop (bulkification). Follows
//! `unresolved_reference_diagnostics.rs`'s exact `Session` harness.

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

fn read_frame(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout before sending a full frame");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.parse::<usize>().unwrap());
        }
    }
    let content_length = content_length.expect("frame had no Content-Length header");
    let mut buf = vec![0u8; content_length];
    stdout.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

fn recv(stdout: &mut impl BufRead) -> serde_json::Value {
    loop {
        let value = read_frame(stdout);
        if value.get("id").is_some() {
            return value;
        }
    }
}

fn recv_notification(stdout: &mut impl BufRead, method: &str) -> serde_json::Value {
    loop {
        let value = read_frame(stdout);
        if value.get("id").is_none() && value.get("method").and_then(|m| m.as_str()) == Some(method) {
            return value;
        }
    }
}

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apexls-server-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        let path = dir.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
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

    fn next_diagnostics(&mut self) -> serde_json::Value {
        recv_notification(&mut self.stdout, "textDocument/publishDiagnostics")
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

fn bulkification_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"].as_str().unwrap_or_default().contains("may exceed governor limits"))
        .collect()
}

fn run_fixture(name: &str, src: &str) -> (Session, Url) {
    let dir = write_fixture_dir(name, &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    let session = Session::start(&root_uri, &foo_uri, src);
    (session, root_uri)
}

const INSERT_IN_FOR_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Integer i = 0; i < accounts.size(); i++) {\n            insert accounts[i];\n        }\n    }\n}\n";

#[test]
fn a_dml_statement_inside_a_for_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-insert-for", INSERT_IN_FOR_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    let diagnostic = diagnostics[0];
    assert_eq!(diagnostic["severity"], serde_json::json!(2), "expected WARNING severity: {diagnostic:?}");
    assert_eq!(diagnostic["source"], serde_json::json!("apexls"));
    assert!(diagnostic["message"].as_str().unwrap().contains("'insert' statement"));
    session.shutdown();
}

const SOQL_IN_FOREACH_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Account a : accounts) {\n            List<Contact> cs = [SELECT Id FROM Contact WHERE AccountId = :a.Id];\n        }\n    }\n}\n";

#[test]
fn a_soql_query_inside_a_foreach_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-soql-foreach", SOQL_IN_FOREACH_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    assert!(diagnostics[0]["message"].as_str().unwrap().contains("SOQL/SOSL query"));
    session.shutdown();
}

const UPDATE_IN_WHILE_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts, Integer i) {\n        while (i > 0) {\n            update accounts[i];\n            i--;\n        }\n    }\n}\n";

#[test]
fn a_dml_statement_inside_a_while_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-update-while", UPDATE_IN_WHILE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    session.shutdown();
}

const DELETE_IN_DO_WHILE_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts, Integer i) {\n        do {\n            delete accounts[i];\n            i--;\n        } while (i > 0);\n    }\n}\n";

#[test]
fn a_dml_statement_inside_a_do_while_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-delete-dowhile", DELETE_IN_DO_WHILE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    session.shutdown();
}

const DATABASE_INSERT_IN_LOOP_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Account a : accounts) {\n            Database.insert(a);\n        }\n    }\n}\n";

#[test]
fn a_database_insert_call_inside_a_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-database-insert", DATABASE_INSERT_IN_LOOP_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    assert!(diagnostics[0]["message"].as_str().unwrap().contains("'Database.insert' call"));
    session.shutdown();
}

const DATABASE_QUERY_IN_LOOP_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Account a : accounts) {\n            List<Contact> cs = Database.query('SELECT Id FROM Contact');\n        }\n    }\n}\n";

#[test]
fn a_database_query_call_inside_a_loop_is_reported_as_a_warning() {
    let (mut session, _root) = run_fixture("bulk-database-query", DATABASE_QUERY_IN_LOOP_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one bulkification diagnostic: {diagnostics:?}");
    session.shutdown();
}

const NESTED_LOOPS_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Account a : accounts) {\n            for (Integer i = 0; i < 1; i++) {\n                insert a;\n            }\n        }\n    }\n}\n";

#[test]
fn a_dml_statement_inside_nested_loops_is_still_reported() {
    let (mut session, _root) = run_fixture("bulk-nested-loops", NESTED_LOOPS_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(
        diagnostics.len(),
        1,
        "expected exactly one diagnostic even though two loops enclose it (not one per enclosing loop): {diagnostics:?}"
    );
    session.shutdown();
}

const DML_OUTSIDE_LOOP_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        insert accounts;\n        for (Account a : accounts) {\n            a.Name = 'x';\n        }\n    }\n}\n";

#[test]
fn a_dml_statement_outside_any_loop_is_not_reported() {
    let (mut session, _root) = run_fixture("bulk-outside-loop", DML_OUTSIDE_LOOP_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no bulkification diagnostics for DML outside any loop: {diagnostics:?}");
    session.shutdown();
}

/// The canonical Apex bulkified idiom -- a SOQL query as a `for`-each
/// loop's own iterable, evaluated exactly once before the loop starts --
/// must never be flagged, even though it's textually adjacent to a loop.
const SOQL_FOR_LOOP_IDIOM_SRC: &str =
    "public class Foo {\n    public void run() {\n        for (Account a : [SELECT Id FROM Account]) {\n            System.debug(a.Id);\n        }\n    }\n}\n";

#[test]
fn the_soql_for_loop_idiom_itself_is_not_reported() {
    let (mut session, _root) = run_fixture("bulk-soql-for-loop-idiom", SOQL_FOR_LOOP_IDIOM_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert!(
        diagnostics.is_empty(),
        "expected the SOQL-for-loop idiom's own iterable query not to be flagged: {diagnostics:?}"
    );
    session.shutdown();
}

/// A query nested inside an outer loop, reached through an *inner* loop's
/// own iterable position, is still the classic N+1 anti-pattern -- the
/// exemption for a `ForEachStmt`'s own iterable must not swallow this
/// case just because the query's own immediate loop only evaluates it
/// once; the *outer* loop still runs it once per outer iteration.
const NESTED_SOQL_FOR_LOOP_IN_OUTER_LOOP_SRC: &str = "public class Foo {\n    public void run(List<Account> accounts) {\n        for (Account a : accounts) {\n            for (Contact c : [SELECT Id FROM Contact WHERE AccountId = :a.Id]) {\n                System.debug(c.Id);\n            }\n        }\n    }\n}\n";

#[test]
fn a_soql_for_loop_nested_inside_an_outer_loop_is_still_reported() {
    let (mut session, _root) = run_fixture("bulk-nested-soql-for-loop", NESTED_SOQL_FOR_LOOP_IN_OUTER_LOOP_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = bulkification_diagnostics(&notification);
    assert_eq!(
        diagnostics.len(),
        1,
        "expected the inner query to still be flagged, since the outer loop runs it once per outer iteration: {diagnostics:?}"
    );
    assert!(diagnostics[0]["message"].as_str().unwrap().contains("SOQL/SOSL query"));
    session.shutdown();
}
