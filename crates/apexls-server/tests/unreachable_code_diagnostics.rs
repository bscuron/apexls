//! Protocol-level verification of `textDocument/publishDiagnostics` for
//! unreachable code (statements after an unconditional
//! return/throw/break/continue, or after an `if`/`else` where both
//! branches terminate). Follows `unresolved_reference_diagnostics.rs`'s
//! exact `Session` harness.

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

fn unreachable_diagnostics(notification: &serde_json::Value) -> Vec<&serde_json::Value> {
    notification["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"] == serde_json::json!("unreachable statement"))
        .collect()
}

fn run_fixture(name: &str, src: &str) -> Session {
    let dir = write_fixture_dir(name, &[("Foo.cls", src)]);
    let root_uri = Url::from_file_path(&dir).unwrap();
    let foo_uri = Url::from_file_path(dir.join("Foo.cls")).unwrap();
    Session::start(&root_uri, &foo_uri, src)
}

const AFTER_RETURN_SRC: &str = "public class Foo {\n    public Integer run() {\n        return 1;\n        System.debug('dead');\n    }\n}\n";

#[test]
fn a_statement_after_an_unconditional_return_is_reported_as_an_error() {
    let mut session = run_fixture("unreachable-after-return", AFTER_RETURN_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], serde_json::json!(1), "expected ERROR severity: {diagnostics:?}");
    assert_eq!(diagnostics[0]["source"], serde_json::json!("apexls"));
    session.shutdown();
}

const AFTER_THROW_SRC: &str = "public class Foo {\n    public void run() {\n        throw new System.DmlException('x');\n        System.debug('dead');\n    }\n}\n";

#[test]
fn a_statement_after_an_unconditional_throw_is_reported_as_an_error() {
    let mut session = run_fixture("unreachable-after-throw", AFTER_THROW_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    session.shutdown();
}

const AFTER_BREAK_SRC: &str = "public class Foo {\n    public void run(List<Integer> xs) {\n        for (Integer x : xs) {\n            break;\n            System.debug('dead');\n        }\n    }\n}\n";

#[test]
fn a_statement_after_an_unconditional_break_is_reported_as_an_error() {
    let mut session = run_fixture("unreachable-after-break", AFTER_BREAK_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    session.shutdown();
}

const AFTER_CONTINUE_SRC: &str = "public class Foo {\n    public void run(List<Integer> xs) {\n        for (Integer x : xs) {\n            continue;\n            System.debug('dead');\n        }\n    }\n}\n";

#[test]
fn a_statement_after_an_unconditional_continue_is_reported_as_an_error() {
    let mut session = run_fixture("unreachable-after-continue", AFTER_CONTINUE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    session.shutdown();
}

const IF_ELSE_BOTH_RETURN_SRC: &str = "public class Foo {\n    public Integer run(Boolean b) {\n        if (b) {\n            return 1;\n        } else {\n            return 2;\n        }\n        System.debug('dead');\n    }\n}\n";

#[test]
fn code_after_an_if_else_where_both_branches_terminate_is_reported_as_an_error() {
    let mut session = run_fixture("unreachable-if-else-both-return", IF_ELSE_BOTH_RETURN_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    session.shutdown();
}

const BARE_IF_RETURN_SRC: &str = "public class Foo {\n    public Integer run(Boolean b) {\n        if (b) return 1; else return 2;\n        System.debug('dead');\n    }\n}\n";

#[test]
fn a_bare_non_block_if_else_where_both_branches_terminate_is_still_reported() {
    let mut session = run_fixture("unreachable-bare-if-else", BARE_IF_RETURN_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected exactly one unreachable diagnostic: {diagnostics:?}");
    session.shutdown();
}

const IF_NO_ELSE_SRC: &str = "public class Foo {\n    public Integer run(Boolean b) {\n        if (b) {\n            return 1;\n        }\n        return 2;\n    }\n}\n";

#[test]
fn an_if_with_no_else_does_not_make_following_code_unreachable() {
    let mut session = run_fixture("unreachable-if-no-else", IF_NO_ELSE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

const IF_ELSE_ONE_BRANCH_FALLS_THROUGH_SRC: &str = "public class Foo {\n    public Integer run(Boolean b) {\n        if (b) {\n            return 1;\n        } else {\n            System.debug('not a return');\n        }\n        return 2;\n    }\n}\n";

#[test]
fn an_if_else_where_only_one_branch_terminates_does_not_make_following_code_unreachable() {
    let mut session = run_fixture("unreachable-if-else-one-branch", IF_ELSE_ONE_BRANCH_FALLS_THROUGH_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// A `try`/`catch`'s own `return`, with no `finally` at all, must never
/// be treated as making code after the whole `try` unreachable -- only a
/// terminating `finally` does that (see the `finally`-specific tests
/// below, ticket 14's settled extension); a bare `try`/`catch` never is.
const TRY_WITH_RETURN_SRC: &str = "public class Foo {\n    public Integer run() {\n        try {\n            return 1;\n        } catch (Exception e) {\n            System.debug(e);\n        }\n        return 2;\n    }\n}\n";

#[test]
fn a_try_blocks_own_return_does_not_make_code_after_the_try_unreachable() {
    let mut session = run_fixture("unreachable-try-return", TRY_WITH_RETURN_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// But a `try` block's own *body* must still be scanned independently
/// for unreachable code within itself.
const UNREACHABLE_INSIDE_TRY_SRC: &str = "public class Foo {\n    public Integer run() {\n        try {\n            return 1;\n            System.debug('dead');\n        } catch (Exception e) {\n        }\n        return 2;\n    }\n}\n";

#[test]
fn unreachable_code_inside_a_try_block_is_still_reported() {
    let mut session = run_fixture("unreachable-inside-try", UNREACHABLE_INSIDE_TRY_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected the dead statement inside the try body to be reported: {diagnostics:?}");
    session.shutdown();
}

const NO_UNREACHABLE_CODE_SRC: &str = "public class Foo {\n    public Integer run(Boolean b) {\n        Integer x = 1;\n        if (b) {\n            x = 2;\n        }\n        return x;\n    }\n}\n";

#[test]
fn normal_code_with_no_early_termination_has_no_unreachable_diagnostics() {
    let mut session = run_fixture("unreachable-clean", NO_UNREACHABLE_CODE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics on clean code: {diagnostics:?}");
    session.shutdown();
}

// -- ticket 14/15's three extensions: finally, switch, do-while --

const FINALLY_THAT_TERMINATES_SRC: &str = "public class Foo {\n    public Integer run() {\n        try {\n            System.debug('x');\n        } finally {\n            return 1;\n        }\n        System.debug('dead');\n    }\n}\n";

#[test]
fn a_finally_clause_that_unconditionally_terminates_makes_code_after_the_try_unreachable() {
    let mut session = run_fixture("unreachable-finally-terminates", FINALLY_THAT_TERMINATES_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected the statement after the try (with a terminating finally) to be reported: {diagnostics:?}");
    session.shutdown();
}

const FINALLY_THAT_DOES_NOT_TERMINATE_SRC: &str = "public class Foo {\n    public Integer run() {\n        try {\n            System.debug('x');\n        } finally {\n            System.debug('cleanup');\n        }\n        return 1;\n    }\n}\n";

#[test]
fn a_finally_clause_that_does_not_terminate_leaves_code_after_the_try_reachable() {
    let mut session = run_fixture("unreachable-finally-no-terminate", FINALLY_THAT_DOES_NOT_TERMINATE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

const SWITCH_EXHAUSTIVE_ALL_TERMINATE_SRC: &str = "public class Foo {\n    public Integer run(Integer i) {\n        switch on i {\n            when 1 {\n                return 1;\n            }\n            when 2 {\n                return 2;\n            }\n            when else {\n                return 0;\n            }\n        }\n        System.debug('dead');\n    }\n}\n";

#[test]
fn an_exhaustive_switch_where_every_arm_terminates_makes_following_code_unreachable() {
    let mut session = run_fixture("unreachable-switch-exhaustive", SWITCH_EXHAUSTIVE_ALL_TERMINATE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected the statement after the exhaustive, all-terminating switch to be reported: {diagnostics:?}");
    session.shutdown();
}

const SWITCH_MISSING_ELSE_SRC: &str = "public class Foo {\n    public Integer run(Integer i) {\n        switch on i {\n            when 1 {\n                return 1;\n            }\n            when 2 {\n                return 2;\n            }\n        }\n        return 0;\n    }\n}\n";

#[test]
fn a_switch_missing_a_when_else_arm_does_not_make_following_code_unreachable() {
    let mut session = run_fixture("unreachable-switch-no-else", SWITCH_MISSING_ELSE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

const SWITCH_ONE_ARM_FALLS_THROUGH_SRC: &str = "public class Foo {\n    public Integer run(Integer i) {\n        switch on i {\n            when 1 {\n                return 1;\n            }\n            when else {\n                System.debug('not a return');\n            }\n        }\n        return 0;\n    }\n}\n";

#[test]
fn a_switch_where_one_arm_falls_through_does_not_make_following_code_unreachable() {
    let mut session = run_fixture("unreachable-switch-partial", SWITCH_ONE_ARM_FALLS_THROUGH_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

const DO_WHILE_BODY_TERMINATES_SRC: &str = "public class Foo {\n    public Integer run() {\n        do {\n            return 1;\n        } while (false);\n        System.debug('dead');\n    }\n}\n";

#[test]
fn a_do_while_whose_body_unconditionally_terminates_makes_following_code_unreachable() {
    let mut session = run_fixture("unreachable-do-while-terminates", DO_WHILE_BODY_TERMINATES_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert_eq!(diagnostics.len(), 1, "expected the statement after the always-terminating do-while to be reported: {diagnostics:?}");
    session.shutdown();
}

const DO_WHILE_BODY_DOES_NOT_TERMINATE_SRC: &str = "public class Foo {\n    public Integer run(Integer i) {\n        do {\n            i--;\n        } while (i > 0);\n        return i;\n    }\n}\n";

#[test]
fn a_do_while_whose_body_does_not_terminate_leaves_following_code_reachable() {
    let mut session = run_fixture("unreachable-do-while-no-terminate", DO_WHILE_BODY_DOES_NOT_TERMINATE_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}

/// A regular `for` loop must never be treated as terminating even if its
/// body unconditionally returns on what would be the first iteration --
/// unlike `do`-`while`, its conditional entry means the body might never
/// run at all (e.g. an empty `xs`), so code after the loop stays reachable.
const FOR_LOOP_BODY_ALWAYS_RETURNS_SRC: &str = "public class Foo {\n    public Integer run(List<Integer> xs) {\n        for (Integer x : xs) {\n            return x;\n        }\n        return 0;\n    }\n}\n";

#[test]
fn a_foreach_loop_whose_body_always_returns_does_not_make_following_code_unreachable() {
    let mut session = run_fixture("unreachable-foreach-always-returns", FOR_LOOP_BODY_ALWAYS_RETURNS_SRC);
    let notification = session.next_diagnostics();
    let diagnostics = unreachable_diagnostics(&notification);
    assert!(diagnostics.is_empty(), "expected no unreachable diagnostics: {diagnostics:?}");
    session.shutdown();
}
