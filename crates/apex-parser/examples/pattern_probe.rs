//! **Throwaway prototype for `.scratch/semantic-query` ticket 01.**
//!
//! Answers one question: can a structural-search pattern string like
//! `for (...) { ... System.debug(...); ... }` be turned into a usable
//! syntax tree by substituting its holes for ordinary identifiers and
//! handing the result to the real Apex parser?
//!
//! Neither `...` nor `$` is an Apex token -- `...` lexes as three `Dot`s
//! and `$` falls through to `TokenKind::Unknown` -- so a pattern string
//! lexes cleanly and then parses into garbage. ast-grep's documented fix
//! is to preprocess the pattern, replacing each hole with something the
//! unmodified grammar already accepts, parse, then reinterpret. Its
//! documented ceiling: holes only work where an *identifier* is
//! grammatical.
//!
//! This probe applies that substitution to a corpus of real patterns,
//! runs each through every entry point (`parse_expression`,
//! `parse_statement`, `parse_block`), and reports which combinations
//! yield a clean, fully-covering parse. Run it with:
//!
//! ```text
//! cargo run -p apex-parser --example pattern_probe
//! ```
//!
//! Not part of the shipped CLI. Delete once ticket 01 is resolved.

use apex_parser::Parse;

/// What a hole becomes before the real parser sees it. Chosen to be a
/// legal Apex identifier that no real code would collide with.
const ELLIPSIS_IDENT: &str = "__AP_DOTS__";
/// A named hole `$FOO` becomes `__AP_CAP_FOO__` -- the two halves are
/// separate consts because the capture's own name is spliced between them.
const CAPTURE_PREFIX: &str = "__AP_CAP_";
const CAPTURE_SUFFIX: &str = "__";

/// Rewrite `...` and `$NAME` into ordinary identifiers.
///
/// A first uniform pass showed why this cannot be position-blind: in
/// statement position a bare identifier is not a statement, so every
/// `{ ... }` pattern died on `expected Semi`. The fix costs one lookback.
/// A hole whose preceding significant character is `{`, `;` or `}` sits
/// where a *statement* is expected, so it becomes `__AP_DOTS__;`;
/// everywhere else (argument lists, SOQL clauses, expressions) it stays a
/// bare identifier. No tree is needed for that call, only the raw text,
/// which is what makes it usable before parsing.
fn substitute(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"...") {
            out.push_str(&expansion_for(pattern, i));
            i += 3;
        } else if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            out.push_str(CAPTURE_PREFIX);
            out.push_str(&pattern[start..end]);
            out.push_str(CAPTURE_SUFFIX);
            i = end;
        } else {
            let ch = pattern[i..].chars().next().expect("char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// What a `...` at byte `at` expands to.
///
/// The uniform "always an identifier" rule has a documented ceiling: a
/// hole only works where an *identifier* is grammatical. Two slots in the
/// corpus cross it, and both want a hole standing for a multi-token
/// construct rather than one name:
///
/// - `for (...)` -- a whole for-header. Expanded to the for-each shape
///   (`Type v : coll`); the C-style shape would need its own variant, so a
///   real implementation compiles `for (...)` to both pattern trees and
///   matches either.
/// - `catch (...)` -- a catch parameter, which is `Type name`, two tokens.
///
/// Both are recognised from the keyword immediately preceding the open
/// paren the hole sits in, which needs no tree -- only the raw text.
fn expansion_for(pattern: &str, at: usize) -> String {
    match enclosing_paren_keyword(pattern, at) {
        Some("for") => format!("{ELLIPSIS_IDENT}_T {ELLIPSIS_IDENT}_V : {ELLIPSIS_IDENT}_C"),
        Some("catch") => format!("{ELLIPSIS_IDENT}_T {ELLIPSIS_IDENT}_N"),
        _ if in_statement_position(pattern, at) => format!("{ELLIPSIS_IDENT};"),
        _ => ELLIPSIS_IDENT.to_string(),
    }
}

/// The identifier or keyword immediately before the open paren of the
/// group the hole sits directly inside -- `for` for `for (...)`, `catch`
/// for `catch (...)`, and equally `debug` for `System.debug(...)`, since
/// nothing here distinguishes a keyword from a method name. Callers are
/// expected to match only the keywords they care about and let everything
/// else fall through to the default expansion. Returns `None` only when
/// the hole is not inside a paren group at all (a statement run, a SOQL
/// clause).
fn enclosing_paren_keyword(pattern: &str, at: usize) -> Option<&str> {
    let before = &pattern[..at];
    let mut depth = 0i32;
    let open = before.char_indices().rev().find_map(|(i, c)| match c {
        ')' => {
            depth += 1;
            None
        }
        '(' if depth == 0 => Some(i),
        '(' => {
            depth -= 1;
            None
        }
        _ => None,
    })?;
    let head = before[..open].trim_end();
    let start = head
        .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// Is the hole starting at `at` sitting where a statement is expected?
///
/// Decided by the nearest preceding non-whitespace character: `{` opens a
/// block, `;` ends the previous statement, `}` closes a nested block --
/// after any of those, the grammar wants a statement next. Anything else
/// (`(`, `,`, an operator, a keyword like `SELECT`) means the hole is in
/// expression, argument or clause position.
fn in_statement_position(pattern: &str, at: usize) -> bool {
    pattern[..at]
        .chars()
        .rev()
        .find(|c| !c.is_whitespace())
        .is_some_and(|c| matches!(c, '{' | ';' | '}'))
}

/// A parse counts as usable only if it recorded no errors **and** consumed
/// the whole pattern into a single top-level node.
///
/// The obvious spelling of that second condition -- comparing the root's
/// `text_range()` against `src.len()` -- is dead code, and finding that
/// out is one of this probe's results. `parse_with`
/// (`crates/apex-parser/src/lib.rs:180-183`) completes the root marker
/// over the entire input, and the tree is lossless, so *every* token
/// lands under the root whether the grammar consumed it or not. The root
/// range therefore always equals `src.len()` and the comparison can never
/// fail.
///
/// What actually discriminates is the root's own child list. A pattern
/// that parsed cleanly yields exactly one non-trivia child, and that child
/// is a node. Trailing tokens the grammar never consumed show up as extra
/// children beside it, which is how `System.debug(...);` is caught being
/// mis-accepted as an *expression* (root children: `MethodCallExpr`, then
/// a stray `;`) and how `$LEFT $OP $RIGHT` is caught parsing as a lone
/// `NameExpr` with two orphaned identifiers trailing it.
fn verdict(parse: &Parse, _src: &str) -> Result<(), String> {
    if !parse.errors.is_empty() {
        let first = &parse.errors[0];
        return Err(format!(
            "{} error(s), first: {} @ {}",
            parse.errors.len(),
            first.message,
            first.offset
        ));
    }

    let root = parse.syntax();
    let significant: Vec<_> = root
        .children_with_tokens()
        .filter(|c| !c.kind().is_trivia())
        .collect();
    match significant.as_slice() {
        [apex_syntax::NodeOrToken::Node(_)] => Ok(()),
        other => Err(format!(
            "unconsumed input: root has {} significant children [{}], expected 1 node",
            other.len(),
            other
                .iter()
                .map(|c| format!("{:?}", c.kind()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

struct Case {
    /// Why this pattern is in the probe: either a corpus item number from
    /// the map's "The corpus" section, or the grammatical slot it exercises
    /// (`expr`, `block`, `stmt`) or the limit it demonstrates (`ceiling`).
    note: &'static str,
    pattern: &'static str,
}

const CASES: &[Case] = &[
    Case {
        note: "expr",
        pattern: "System.debug(...)",
    },
    Case {
        note: "3/R2",
        pattern: "System.debug(...);",
    },
    Case {
        note: "R1",
        pattern: "$X.size() > 0",
    },
    Case {
        note: "1",
        pattern: "[SELECT ... FROM $OBJ]",
    },
    Case {
        note: "1/2",
        pattern: "for (...) { ... }",
    },
    Case {
        note: "1/2",
        pattern: "for (Account $A : $L) { ... }",
    },
    Case {
        note: "1",
        pattern: "for (...) { ... $X = [SELECT ... FROM Account]; ... }",
    },
    Case {
        note: "block",
        pattern: "{ ... System.debug(...); ... }",
    },
    Case {
        note: "4",
        pattern: "try { ... } catch (...) { }",
    },
    Case {
        note: "4",
        pattern: "catch (...) { }",
    },
    Case {
        note: "2",
        pattern: "insert $X;",
    },
    Case {
        note: "6",
        pattern: "Database.query($Q)",
    },
    Case {
        note: "stmt",
        pattern: "if ($C) { ... }",
    },
    Case {
        note: "ceiling",
        pattern: "$LEFT $OP $RIGHT",
    },
];

type EntryPoint = (&'static str, fn(&str) -> Parse);

const ENTRY_POINTS: &[EntryPoint] = &[
    ("expression", apex_parser::parse_expression),
    ("statement", apex_parser::parse_statement),
    ("block", apex_parser::parse_block),
];

fn main() {
    // Same stack precaution every other tree-walking entry point in this
    // workspace takes -- see `apex_parser`'s "deep-tree stack safety
    // caveat". Overkill for patterns this small, but free and consistent.
    std::thread::Builder::new()
        .stack_size(apex_parser::RECOMMENDED_MIN_STACK_SIZE)
        .spawn(probe)
        .expect("failed to spawn worker thread")
        .join()
        .expect("worker thread panicked");
}

fn probe() {
    let mut unparseable = Vec::new();

    for case in CASES {
        let substituted = substitute(case.pattern);
        println!("\n[{}] {}", case.note, case.pattern);
        println!("  substituted: {substituted}");

        let mut any_ok = false;
        for (name, parse_fn) in ENTRY_POINTS {
            let parse = parse_fn(&substituted);
            match verdict(&parse, &substituted) {
                Ok(()) => {
                    // Dump the whole tree for the winning entry point, not
                    // just its top-level child kinds. The synthesised
                    // header shapes (`for (...)` becoming a for-each,
                    // `catch (...)` becoming `Type name`) are only
                    // judgeable by seeing where the hole identifiers
                    // actually landed, which a one-line summary hides.
                    // Later entry points that also accept are noted but not
                    // dumped -- the first-match-wins rule means only the
                    // first tree is the one a real matcher would use.
                    let root = parse.syntax();
                    if any_ok {
                        println!(
                            "  {name:<11} OK   root={:?} (also parses; tree omitted)",
                            root.kind()
                        );
                    } else {
                        println!("  {name:<11} OK   root={:?}", root.kind());
                        for line in format!("{root:#?}").lines() {
                            println!("      {line}");
                        }
                    }
                    any_ok = true;
                }
                Err(why) => println!("  {name:<11} --   {why}"),
            }
        }
        if !any_ok {
            unparseable.push(case.pattern);
        }
    }

    println!("\n=== summary ===");
    if unparseable.is_empty() {
        println!("every pattern parsed under at least one entry point");
    } else {
        println!(
            "{} pattern(s) no entry point could parse:",
            unparseable.len()
        );
        for p in &unparseable {
            println!("  {p}");
        }
    }
}
