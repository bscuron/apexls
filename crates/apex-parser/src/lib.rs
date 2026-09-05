//! Hand-written recursive-descent parser producing an `apex-syntax` CST.
//!
//! Error recovery is a first-class design goal: on a syntax error the
//! parser should resynchronize at a statement/member boundary and keep
//! producing a best-effort tree for the rest of the file.
//!
//! **Deep-tree stack safety caveat.** Parsing itself (`grammar::expressions`'
//! left-associative binary levels, in particular) is iterative, not
//! recursive -- a long same-precedence chain like `a + b + c + ...`
//! costs O(1) parser stack depth regardless of chain length. But the
//! *tree* it produces is still a left-nested `BinExpr` chain exactly as
//! long, and `rowan::GreenNode` (an `Arc`-based recursive structure) is
//! torn down via ordinary generated `Drop` glue, which recurses one
//! frame per tree level. On this project's Windows dev machine a
//! left-nested chain around ~6,500-7,000 terms overflows a default
//! (~1 MiB) thread stack purely on *drop* -- confirmed by isolating each
//! stage (`parse_expression` returning, `Parse::syntax()`, dropping the
//! `SyntaxNode`, then dropping `Parse` itself) and finding only the
//! final drop crashes. This is a `rowan`-level characteristic, not a bug
//! in this crate's own grammar code, and it can't be fixed by changing
//! how events are parsed/built -- only by giving whichever thread
//! eventually drops the tree enough stack headroom. A long chain this
//! long is very rare in hand-written Apex but realistic from generated
//! code (a large dynamic SOQL condition, a big rules-engine-emitted
//! boolean expression, ...), so any caller whose input isn't guaranteed
//! small/hand-written should parse (and, critically, later drop the
//! result) on a thread built with at least [`RECOMMENDED_MIN_STACK_SIZE`]
//! bytes of stack -- see `apex-binder`'s `BoundProgram::from_files` and
//! `apexls-cli`'s `main` for the two call sites in this workspace that
//! do.

mod errors;
mod event;
mod grammar;
mod input;
mod parser;

pub use apex_syntax::NodeCache;
pub use errors::{Parse, ParseError};

use std::sync::Arc;

use input::Input;
use parser::Parser;

/// The minimum stack size (bytes) a thread should be built with before
/// parsing input that isn't guaranteed small/hand-written, and -- this
/// is the part that actually matters -- before dropping the resulting
/// `Parse`/`SyntaxNode` tree. See the module doc comment's "deep-tree
/// stack safety caveat" for why. 64 MiB comfortably covers even
/// pathological generated chains hundreds of thousands of terms long;
/// actual physical memory use stays near zero for ordinary input, since
/// OS thread stacks are reserved virtual address space, committed
/// page-by-page only as they're actually touched.
pub const RECOMMENDED_MIN_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Parse `src` as a single expression (Phase 2's `<=`/`>=`/shift merges,
/// the full corrected precedence table, and the cast-vs-paren
/// disambiguation all apply). Trailing content after the expression is
/// left unconsumed in the token stream but does not appear in the
/// returned tree -- callers wanting "this whole string must be exactly
/// one expression" should check `Parse::errors` is empty and that the
/// tree's text covers all of `src`.
pub fn parse_expression(src: &str) -> Parse {
    let mut cache = NodeCache::default();
    parse_with(src, apex_syntax::SyntaxKind::ExprRoot, &mut cache, |p| {
        grammar::expressions::expr(p);
    })
}

/// Parse `src` as a single statement (any of the 19 forms in
/// `grammar::statements`, including the six DML statements, `switch on`,
/// and `System.runAs`).
pub fn parse_statement(src: &str) -> Parse {
    let mut cache = NodeCache::default();
    parse_with(src, apex_syntax::SyntaxKind::StmtRoot, &mut cache, |p| {
        grammar::statements::statement(p);
    })
}

/// Parse `src` as a brace-delimited block (`{ stmt* }`).
pub fn parse_block(src: &str) -> Parse {
    let mut cache = NodeCache::default();
    parse_with(src, apex_syntax::SyntaxKind::BlockRoot, &mut cache, |p| {
        grammar::statements::block(p);
    })
}

/// Parse `src` as a whole `.cls` compilation unit: `modifier* (class |
/// interface | enum)` declaration, EOF (Phase 3).
#[hotpath::measure]
pub fn parse_compilation_unit(src: &str) -> Parse {
    let mut cache = NodeCache::default();
    parse_compilation_unit_with_cache(src, &mut cache)
}

/// Parse `src` as a whole `.trigger` file: `trigger Name on Object
/// (before insert, ...) { ... }` (Phase 3).
#[hotpath::measure]
pub fn parse_trigger_unit(src: &str) -> Parse {
    let mut cache = NodeCache::default();
    parse_trigger_unit_with_cache(src, &mut cache)
}

/// Like [`parse_compilation_unit`], but interns the resulting tree's
/// nodes/tokens into a caller-supplied [`NodeCache`] instead of a fresh,
/// throwaway one. A `NodeCache` lets rowan structurally share identical
/// green nodes/tokens (built once, reused via `Arc` clone) rather than
/// allocating a fresh copy every time the same text/shape recurs -- e.g. a
/// `public` keyword token, a `;` separator, or a single-child wrapper node
/// each currently allocate anew for every occurrence within one parse.
/// Within a single file that already saves relatively little (each token
/// still occurs at most as many times as the file repeats it), but a
/// caller parsing a whole *project* full of files one `Parse` at a time
/// (`apex-binder`'s cold bind) shares essentially the whole keyword/
/// punctuation vocabulary and plenty of small recurring node shapes
/// *across* files instead of paying for them again per file -- measured
/// as the single largest contributor to `apexls-server`'s steady-state
/// memory (see `crates/apex-binder/examples/mem_profile.rs` and
/// `BACKLOG.md`). Safe to reuse a `NodeCache` indefinitely (nothing about
/// it depends on which files fed it), but this project deliberately scopes
/// reuse to one bind batch (see `apex-binder`'s Stage 1a) rather than a
/// whole editor session, so the cache's own footprint stays bounded by
/// "however much of the project's vocabulary got parsed this batch"
/// instead of growing across however many edits a session accumulates.
#[hotpath::measure]
pub fn parse_compilation_unit_with_cache(src: &str, cache: &mut NodeCache) -> Parse {
    parse_root(src, cache, grammar::declarations::compilation_unit)
}

/// Like [`parse_trigger_unit`], but shares `cache` -- see
/// [`parse_compilation_unit_with_cache`]'s doc comment.
#[hotpath::measure]
pub fn parse_trigger_unit_with_cache(src: &str, cache: &mut NodeCache) -> Parse {
    parse_root(src, cache, grammar::declarations::trigger_unit)
}

/// Like `parse_with`, but for entry points whose grammar function always
/// produces a real node (never `Option::None`) and opens/completes its
/// *own* root marker as the very first parser action -- no extra
/// generic-root wrapping needed on top.
fn parse_root(
    src: &str,
    cache: &mut NodeCache,
    f: impl FnOnce(&mut Parser<'_>) -> parser::CompletedMarker,
) -> Parse {
    let input = Input::new(src);
    let mut p = Parser::new(&input);
    f(&mut p);
    let (events, errors) = p.finish();
    let green = event::build(src, &input, events, cache);
    Parse { green, errors, text: Arc::from(src) }
}

fn parse_with(
    src: &str,
    root_kind: apex_syntax::SyntaxKind,
    cache: &mut NodeCache,
    f: impl FnOnce(&mut Parser<'_>),
) -> Parse {
    let input = Input::new(src);
    let mut p = Parser::new(&input);
    let m = p.start();
    f(&mut p);
    m.complete(&mut p, root_kind);
    let (events, errors) = p.finish();
    let green = event::build(src, &input, events, cache);
    Parse { green, errors, text: Arc::from(src) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Input;
    use crate::parser::Parser;
    use apex_syntax::SyntaxKind;

    /// `Parser::bump_if_no_progress` -- the infinite-loop guard every
    /// `parse_list`-driven loop relies on for input no sub-parser can
    /// make progress on. A stray `)` inside a class body reaches it: it
    /// isn't a `;`/`{`/`static {`, so `class_body_decl` delegates to
    /// `member_decl`, which fails to parse any member declaration there
    /// and returns without consuming anything.
    #[test]
    fn a_token_no_sub_parser_can_consume_is_force_consumed_as_an_error_node() {
        let src = "public class Foo { ) }";
        let parse = parse_compilation_unit(src);
        assert!(
            parse
                .errors
                .iter()
                .any(|e| e.message.contains("unexpected")),
            "expected an \"unexpected ...\" recovery error: {:?}",
            parse.errors
        );
        assert_eq!(
            parse.syntax().text().to_string(),
            src,
            "recovery must still keep the tree lossless"
        );
    }

    /// `Parser::prev_token_end`'s `pos == 0` branch -- reached only when
    /// an `expect` fails as the very first parser action, before
    /// anything has been consumed yet (so there's no previous token to
    /// point the "missing token" diagnostic after).
    #[test]
    fn a_missing_token_at_the_very_start_of_input_does_not_panic() {
        let parse = parse_block("");
        assert!(!parse.errors.is_empty(), "expected a missing-LBrace error");
    }

    /// `Parse::ok`/its `Debug` impl -- no other test in this crate ever
    /// calls either directly (everything else inspects `.errors` itself).
    #[test]
    fn parse_ok_and_debug_reflect_error_state() {
        let clean = parse_expression("a");
        assert!(clean.ok());
        let broken = parse_expression("a +");
        assert!(!broken.ok());
        let debug_text = format!("{broken:?}");
        assert!(debug_text.contains("Parse"));
        assert!(debug_text.contains("errors"));
    }

    /// Smoke test for the core machinery (input -> parser -> events ->
    /// sink), independent of any real grammar: parse a single identifier
    /// token wrapped in a root node and check the tree round-trips.
    #[test]
    fn core_pipeline_round_trips_a_single_token() {
        let src = "  foo  ";
        let input = Input::new(src);
        let mut p = Parser::new(&input);
        let m = p.start();
        p.bump();
        m.complete(&mut p, SyntaxKind::ExprRoot);
        let (events, errors) = p.finish();
        assert!(errors.is_empty());

        let mut cache = NodeCache::default();
        let green = event::build(src, &input, events, &mut cache);
        let parse = Parse {
            green,
            errors: Vec::new(),
            text: Arc::from(src),
        };
        assert_eq!(parse.syntax().text().to_string(), src);
    }

    /// Exercises the newline-split trivia rule and a nested-node wrap via
    /// `precede`: `a; // trailing\nb` parsed as two child nodes under one
    /// root. `// trailing` must stay glued to `a`'s statement as trailing
    /// trivia (no newline before it truncates that run), while the
    /// newline itself is the split point, so `\n` also ends up trailing
    /// on `a`'s side (up to *and including* the first newline) and `b`
    /// gets no leading trivia at all.
    #[test]
    fn trivia_split_and_precede_round_trip() {
        let src = "a; // trailing\nb";
        let input = Input::new(src);
        let mut p = Parser::new(&input);
        let root = p.start();

        // First statement: `a` wrapped in a node, then retroactively
        // preceded to include the following `;` too.
        let a = p.start();
        p.bump(); // a
        let a = a.complete(&mut p, SyntaxKind::NameExpr);
        let stmt1 = a.precede(&mut p);
        p.bump(); // ;
        stmt1.complete(&mut p, SyntaxKind::ExprStmt);

        // Second statement: bare `b`.
        let stmt2 = p.start();
        p.bump(); // b
        stmt2.complete(&mut p, SyntaxKind::ExprStmt);

        root.complete(&mut p, SyntaxKind::BlockRoot);
        let (events, errors) = p.finish();
        assert!(errors.is_empty());

        let mut cache = NodeCache::default();
        let green = event::build(src, &input, events, &mut cache);
        let parse = Parse {
            green,
            errors: Vec::new(),
            text: Arc::from(src),
        };
        let tree = parse.syntax();
        assert_eq!(tree.text().to_string(), src);

        let children: Vec<_> = tree.children().collect();
        assert_eq!(
            children.len(),
            2,
            "expected two ExprStmt children, got {children:?}"
        );
        assert_eq!(children[0].kind(), SyntaxKind::ExprStmt);
        assert_eq!(children[0].text().to_string(), "a; // trailing\n");
        assert_eq!(children[1].kind(), SyntaxKind::ExprStmt);
        assert_eq!(children[1].text().to_string(), "b");
    }

    fn assert_expr_round_trips(src: &str) {
        let parse = parse_expression(src);
        assert!(
            parse.errors.is_empty(),
            "{src:?}: unexpected errors: {:?}",
            parse.errors
        );
        assert_eq!(
            parse.syntax().text().to_string(),
            src,
            "{src:?} did not round-trip"
        );
    }

    fn assert_stmt_round_trips(src: &str) {
        let parse = parse_statement(src);
        assert!(
            parse.errors.is_empty(),
            "{src:?}: unexpected errors: {:?}",
            parse.errors
        );
        assert_eq!(
            parse.syntax().text().to_string(),
            src,
            "{src:?} did not round-trip"
        );
    }

    /// Broad smoke coverage across the whole precedence table, the two
    /// backtracking cases, and every statement form -- not a substitute
    /// for the structural (insta) golden suite, just a fast "does the
    /// foundation actually work" check before building on top of it.
    #[test]
    fn expressions_round_trip() {
        for src in [
            "a",
            "1",
            "'hello'",
            "true",
            "null",
            "this",
            "super",
            "a = b",
            "a = b = c",
            "a ? b : c",
            "a ? b ? c : d : e",
            "a ?? b ?? c",
            "a || b && c",
            "a | b ^ c & d",
            "a == b != c === d !== e",
            "a instanceof Foo",
            "a < b",
            "a > b",
            "a <= b",
            "a >= b",
            "a << b",
            "a >> b",
            "a >>> b",
            "a + b * c",
            "(a + b) * c",
            "-a + +b",
            "!a && ~b",
            "a++ + ++b",
            "-a++",
            "!a++",
            "a.b.c",
            "a?.b",
            "a[0]",
            "a.b()",
            "a.b(1, 2)",
            "foo()",
            "foo(1, 2)",
            "this(1)",
            "super(1)",
            "(Foo) x",
            "(Foo.Bar) x",
            "(a + b)",
            "(a)",
            "new Foo()",
            "new Foo(1, 2)",
            "new List<Integer>()",
            "new List<List<Integer>>()",
            "new Integer[5]",
            "new Integer[]{1, 2, 3}",
            "new List<Integer>{1, 2, 3}",
            "new Map<String, Integer>{'a' => 1, 'b' => 2}",
            "new Foo.Bar()",
        ] {
            assert_expr_round_trips(src);
        }
    }

    /// A handful of `grammar::expressions`' `p.error(...)` fallback arms,
    /// hit with deliberately malformed input -- proves each produces its
    /// documented message and doesn't panic, rather than silently
    /// accepting garbage.
    #[test]
    fn malformed_expressions_report_the_expected_error() {
        for (src, expected_message) in [
            ("a instanceof", "expected type after 'instanceof'"),
            ("new 5", "expected a type after 'new'"),
            (
                "new Foo",
                "expected constructor arguments, array size, or initializer after 'new' type",
            ),
            (
                // The key-expression parse itself fails first (`primary`'s
                // own catch-all); `map_pair_continue`'s `None` arm must
                // still recover cleanly from there (its `p.expect(MapTo)`
                // succeeds since nothing was consumed, so no second error).
                "new Map<String, Integer>{'a' => 1, => 2}",
                "expected expression, found MapTo",
            ),
        ] {
            let parse = parse_expression(src);
            assert!(
                parse
                    .errors
                    .iter()
                    .any(|e| e.message.contains(expected_message)),
                "{src:?}: expected an error containing {expected_message:?}, got {:?}",
                parse.errors
            );
        }
    }

    #[test]
    fn statements_round_trip() {
        for src in [
            "foo();",
            "a = b;",
            "Integer x = 5;",
            "Integer x = 5, y = 6;",
            "final Integer x = 5;",
            "List<Integer> xs = new List<Integer>();",
            "foo.bar();",
            "if (a) { b(); }",
            "if (a) { b(); } else { c(); }",
            "if (a) b(); else c();",
            "while (a) { b(); }",
            "while (a) ;",
            "do { a(); } while (b);",
            "for (Integer i = 0; i < 10; i++) { a(); }",
            "for (Account a : accounts) { b(); }",
            "for (;;) { a(); }",
            "return;",
            "return a;",
            "throw new MyException();",
            "break;",
            "continue;",
            "try { a(); } catch (Exception e) { b(); }",
            "try { a(); } catch (Exception e) { b(); } finally { c(); }",
            "try { a(); } finally { c(); }",
            "insert a;",
            "insert as system a;",
            "update a;",
            "delete a;",
            "undelete a;",
            "upsert a;",
            "upsert a Some__c;",
            "merge a b;",
            "System.runAs(u) { a(); }",
            "switch on x { when 1, 2 { a(); } when Account acc { b(); } when else { c(); } }",
            // `whenLiteral`'s parenthesized, signed-integer, and negative-
            // integer forms -- every other `switch` fixture above only
            // ever uses a bare positive integer literal.
            "switch on x { when (1) { a(); } }",
            "switch on x { when -1 { a(); } }",
            "switch on x { when +1 { a(); } }",
            // `forUpdate`'s comma-separated multi-expression form.
            "for (Integer i = 0, j = 0; i < 10; i++, j--) { a(); }",
            // `catch (final MyException e)` -- genuinely valid Apex
            // (confirmed against a real org, per `catch_clause`'s own
            // doc comment), just never exercised by any fixture here.
            "try { a(); } catch (final MyException e) { b(); }",
        ] {
            assert_stmt_round_trips(src);
        }
    }

    /// A handful of `grammar::statements`' `p.error(...)` fallback arms,
    /// hit with deliberately malformed input.
    #[test]
    fn malformed_statements_report_the_expected_error() {
        for (src, expected_message) in [
            (
                "switch on x { when true { a(); } }",
                "expected when-literal",
            ),
            (
                "try { a(); } catch (5 e) { b(); }",
                "expected exception type",
            ),
            (
                "insert as bogus a;",
                "expected 'system' or 'user' after 'as'",
            ),
            ("final 5;", "expected a local variable declaration"),
        ] {
            let parse = parse_statement(src);
            assert!(
                parse
                    .errors
                    .iter()
                    .any(|e| e.message.contains(expected_message)),
                "{src:?}: expected an error containing {expected_message:?}, got {:?}",
                parse.errors
            );
        }
    }

    #[test]
    fn block_round_trips() {
        let src = "{ Integer x = 1; if (x > 0) { return x; } return -x; }";
        let parse = parse_block(src);
        assert!(
            parse.errors.is_empty(),
            "unexpected errors: {:?}",
            parse.errors
        );
        assert_eq!(parse.syntax().text().to_string(), src);
    }
}
