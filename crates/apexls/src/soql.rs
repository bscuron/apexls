//! `apexls soql [paths...]`: a ripgrep `--vimgrep`-style inventory of
//! every SOQL query *site* in the project -- `path:line:col:text`, one
//! line per query, sorted by location.
//!
//! Two shapes count, both found straight off the raw syntax tree:
//!
//! - An inline query expression (`SyntaxKind::SoqlExpr`), printed as its
//!   own source text, brackets included (`[SELECT Id FROM Account]`).
//!   The node spans `[`..`]` (see `apex_parser`'s `grammar::soql`), and a
//!   subquery completes as its own `SoqlSubQuery` kind rather than a
//!   nested `SoqlExpr`, so each top-level inline query is reported
//!   exactly once, never again for its own subqueries.
//! - A `Database.<method>` call naming one of [`DATABASE_QUERY_METHOD_NAMES`],
//!   printed as the whole call's source text (`Database.query(q)`).
//!
//! SOSL (`[FIND ...]`, `Search.query`) is deliberately excluded -- the
//! command is `soql`, and an inventory that quietly folded a different
//! query language into the same list would be lying about what it found.
//!
//! **The printed text is whatever is literally written at the call
//! site** -- no attempt to reconstruct a dynamic query's real string by
//! tracing a variable back to its assignments. `Database.query(q)`
//! prints as `Database.query(q)`. That's the point of the command, not a
//! gap in it: it answers "where do queries happen", and a site whose
//! text can't be read straight off the source is exactly the one worth
//! listing rather than silently dropping. (`apex-binder`'s
//! `resolve::dynamic_soql_source_tokens` does trace literals back
//! through same-method assignments, for dynamic-SOQL *bind-variable*
//! resolution -- deliberately not used here, since reaching for it would
//! mean binding the whole project for an answer this command doesn't
//! need.)
//!
//! Because neither shape needs any name resolution, this binds nothing:
//! it discovers, parses, and walks, skipping the whole `BoundProgram`
//! cost `check`/`fix` pay. The `Database` receiver is matched
//! textually/case-insensitively, exactly as `apexls-server`'s
//! `bulkification_diagnostics` and `apex_binder::resolve`'s dynamic-SOQL
//! handling already match it -- no real Apex project shadows the stdlib
//! `Database` with its own class.
//!
//! `paths` behaves as it does for `check`/`fix`: zero or more
//! files/directories filtering *which* files are reported on, empty
//! meaning the whole project.

use crate::project::{parse_apex_file, site_for, walk_project, ArgError, Site};
use apex_syntax::ast::expr::{Expr, MethodCallExpr};
use apex_syntax::{AstNode, SyntaxKind};
use apexls_server::LineIndex;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Every `Database.<method>` entry point that runs a SOQL query, named
/// case-insensitively (lowercase here, lowercased before comparison).
/// Taken from the scraped `System.Database` signatures in
/// `apex-stdlib`'s `apex_reference.json` rather than from memory, and
/// deliberately *narrower* than `bulkification_diagnostics`'
/// `DATABASE_BULK_METHOD_NAMES`, which also covers the DML entry points
/// (`Database.insert`, ...) -- those are governor-limit-relevant but are
/// not queries, so they have no place in a query inventory.
///
/// `getQueryLocator`/`getCursor` have `sObject`-taking overloads that
/// don't take a query string at all; they're still listed, since the
/// call site is still a query site and this command prints what's
/// written rather than what a string argument says.
const DATABASE_QUERY_METHOD_NAMES: &[&str] = &[
    "query",
    "querywithbinds",
    "countquery",
    "countquerywithbinds",
    "getquerylocator",
    "getquerylocatorwithbinds",
    "getcursor",
    "getcursorwithbinds",
    "getpaginationcursor",
    "getpaginationcursorwithbinds",
];

pub fn run(paths: &[PathBuf]) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let queries = match find_queries(paths, &cwd) {
        Ok(queries) => queries,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    // ripgrep's `--vimgrep` shape: `path:line:col:text`, no spaces around
    // the separators, one match per line -- which is why `text` is
    // whitespace-collapsed (see `collapse`), since real inline SOQL is
    // routinely written across half a dozen lines.
    for q in &queries {
        println!("{q}");
    }

    ExitCode::SUCCESS
}

/// The actual inventory logic, factored out of [`run`] the same way
/// `check`'s `find_findings` is and for the same reason: testable
/// without touching the real process-global CWD or capturing stdout.
fn find_queries(paths: &[PathBuf], cwd: &Path) -> Result<Vec<Site>, ArgError> {
    walk_project(paths, cwd, queries_in_file)
}

fn queries_in_file(display_path: &Path, src: &str) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);

    parse
        .syntax()
        .descendants()
        .filter_map(|node| {
            match node.kind() {
                SyntaxKind::SoqlExpr => {}
                SyntaxKind::MethodCallExpr => {
                    let mc = MethodCallExpr::cast(node.clone())?;
                    let is_database_call = matches!(mc.target(), Some(Expr::Name(n)) if n
                        .name_token()
                        .is_some_and(|t| t.text().eq_ignore_ascii_case("Database")));
                    if !is_database_call {
                        return None;
                    }
                    let method = mc.method_name_token()?;
                    if !DATABASE_QUERY_METHOD_NAMES
                        .contains(&method.text().to_ascii_lowercase().as_str())
                    {
                        return None;
                    }
                }
                _ => return None,
            }
            site_for(display_path, src, &index, &node)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apexls-soql-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rendered(queries: &[Site]) -> Vec<String> {
        queries.iter().map(Site::to_string).collect()
    }

    #[test]
    fn finds_inline_and_dynamic_queries_in_vimgrep_format() {
        let dir = temp_dir("basic");
        std::fs::write(
            dir.join("Foo.cls"),
            "public class Foo {\n    public void run(String q) {\n        List<Account> a = [SELECT Id,\n            Name\n            FROM Account];\n        Database.query(q);\n        Database.queryWithBinds(q, new Map<String, Object>(), AccessLevel.USER_MODE);\n    }\n}\n",
        )
        .unwrap();
        let queries = find_queries(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            rendered(&queries),
            vec![
                // Multi-line inline SOQL collapses onto one record.
                "Foo.cls:3:27:[SELECT Id, Name FROM Account]".to_string(),
                // Dynamic queries print what's written, never a traced string.
                "Foo.cls:6:9:Database.query(q)".to_string(),
                "Foo.cls:7:9:Database.queryWithBinds(q, new Map<String, Object>(), AccessLevel.USER_MODE)".to_string(),
            ],
        );
    }

    #[test]
    fn ignores_sosl_dml_and_unrelated_database_calls() {
        let dir = temp_dir("exclusions");
        std::fs::write(
            dir.join("Bar.cls"),
            "public class Bar {\n    public void run(List<Account> rows) {\n        List<List<SObject>> hits = [FIND 'acme' IN ALL FIELDS RETURNING Account(Id)];\n        Database.insert(rows);\n        Database.setSavepoint();\n        Search.query('FIND \\'acme\\' RETURNING Account(Id)');\n    }\n}\n",
        )
        .unwrap();
        let queries = find_queries(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            queries.is_empty(),
            "SOSL, DML and non-query Database calls are all out of scope: {:?}",
            rendered(&queries),
        );
    }

    #[test]
    fn reports_a_subquery_only_as_part_of_its_outer_query() {
        let dir = temp_dir("subquery");
        std::fs::write(
            dir.join("Baz.cls"),
            "public class Baz {\n    public void run() {\n        List<Account> a = [SELECT Id, (SELECT Id FROM Contacts) FROM Account];\n    }\n}\n",
        )
        .unwrap();
        let queries = find_queries(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            rendered(&queries),
            vec!["Baz.cls:3:27:[SELECT Id, (SELECT Id FROM Contacts) FROM Account]".to_string()],
        );
    }

    #[test]
    fn finds_queries_in_a_trigger_file() {
        let dir = temp_dir("trigger");
        std::fs::write(
            dir.join("AccountTrigger.trigger"),
            "trigger AccountTrigger on Account (before insert) {\n    List<Contact> cs = [SELECT Id FROM Contact];\n    Database.query('SELECT Id FROM Lead');\n}\n",
        )
        .unwrap();
        let queries = find_queries(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            rendered(&queries),
            vec![
                "AccountTrigger.trigger:2:24:[SELECT Id FROM Contact]".to_string(),
                "AccountTrigger.trigger:3:5:Database.query('SELECT Id FROM Lead')".to_string(),
            ],
        );
    }

    /// Guards `apex_syntax::significant_range`: a comment sitting directly
    /// before a query is attached *inside* the node by the tree-builder, so
    /// a raw `text_range()` would anchor the report at the comment's column
    /// and print the comment as part of the query text.
    #[test]
    fn anchors_past_a_leading_comment_rather_than_at_it() {
        let dir = temp_dir("leading-comment");
        std::fs::write(
            dir.join("Commented.cls"),
            "public class Commented {\n    public void run() {\n        List<Account> a = /* why */ [SELECT Id FROM Account];\n    }\n}\n",
        )
        .unwrap();
        let queries = find_queries(&[], &dir).expect("no path arguments to fail on");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            rendered(&queries),
            vec!["Commented.cls:3:37:[SELECT Id FROM Account]".to_string()],
        );
    }

    #[test]
    fn filters_to_only_the_requested_path() {
        let dir = temp_dir("filtered");
        std::fs::create_dir_all(dir.join("included")).unwrap();
        std::fs::create_dir_all(dir.join("excluded")).unwrap();
        std::fs::write(
            dir.join("included").join("Included.cls"),
            "public class Included {\n    public void run() { List<Account> a = [SELECT Id FROM Account]; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("excluded").join("Excluded.cls"),
            "public class Excluded {\n    public void run() { List<Contact> c = [SELECT Id FROM Contact]; }\n}\n",
        )
        .unwrap();

        let queries =
            find_queries(&[dir.join("included")], &dir).expect("a real, existing path argument");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(queries.len(), 1, "{:?}", rendered(&queries));
        assert!(queries[0].text.contains("FROM Account"));
    }

    #[test]
    fn rejects_a_nonexistent_path_argument() {
        let dir = temp_dir("bad-path");
        let missing = dir.join("DoesNotExist.cls");
        let err =
            find_queries(std::slice::from_ref(&missing), &dir).expect_err("a nonexistent path");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(err.1, 2);
        assert!(err.0.contains("does not exist"), "{}", err.0);
    }
}
