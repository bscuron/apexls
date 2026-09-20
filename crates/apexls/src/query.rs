//! `apexls query PATTERN [paths...]`: structural search over Apex source,
//! printed ripgrep `--vimgrep`-style as `path:line:col:text`.
//!
//! A pattern is Apex code with holes in it. `...` matches any code in the
//! position it sits; `$NAME` matches any single construct and captures it,
//! and reusing the same `$NAME` requires both occurrences to match the same
//! code. So `for (...) { ... System.debug(...); ... }` finds a debug call
//! anywhere inside a for-each loop, and `$X.size() > 0` finds that shape
//! whatever `$X` is.
//!
//! **How a pattern becomes a tree.** Neither `...` nor `$` is an Apex
//! token, so a pattern string lexes cleanly and then parses into garbage.
//! Each hole is therefore rewritten into an ordinary identifier before the
//! unmodified parser ever sees it, and the identifiers are recognised again
//! on the way out. Two positions need more than a bare identifier, both
//! detectable from the keyword heading the parenthesised group the hole
//! sits in: `for (...)` wants a whole loop header and `catch (...)` wants a
//! `Type name` pair. See [`substitute`].
//!
//! **What `...` means.** Inside a block it is *deep*: it crosses block
//! boundaries, so `{ ... P ... }` means "this block contains P at any
//! depth", not "P is a direct child statement". That is the whole point for
//! the motivating query -- a SOQL call nested in an `if` inside a `for` is
//! still the governor-limit bug. Everywhere else (argument lists, SOQL
//! clauses) `...` is an ordinary sibling-sequence wildcard. A block pattern
//! written without any `...` is exact: `{ P }` matches only a block whose
//! single statement is P.
//!
//! A block segment may be written as a bare *expression* even though a
//! block grammatically holds statements: `for (...) { ... [SELECT ... FROM $O] ... }`
//! is the flagship query and is exactly how a user thinks of it. The
//! missing `;` is supplied so the pattern parses, and the resulting
//! statement wrapper is unwrapped again when searching, so the query is
//! found wherever it actually sits -- inside a declaration, an argument, a
//! `return`, not only as a statement of its own.
//!
//! Deep matching runs over the block's whole subtree flattened into
//! document order, so `{ ... P ... Q ... }` means "P somewhere, then Q
//! somewhere after it" however deeply either is nested -- the shape behind
//! every ordering query. Order is enforced by a forward-only cursor, so the
//! same two elements reversed is a different query.
//!
//! It is offered only for a pattern unanchored at both ends that separates
//! every fixed element with an ellipsis (`[..., P, ..., Q, ...]`).
//! Everything else is asking for something a descendant search cannot
//! honestly answer, so it stays shallow rather than guessing: a missing
//! leading or trailing `...` anchors that end to the block's first or last
//! *statement*, which a match buried at depth is not, and two fixed
//! elements with no `...` between them ask to be *consecutive*, which is
//! meaningless once they may sit at different depths.
//!
//! **One pattern can denote several shapes.** Apex has two loop forms and
//! they are different productions -- `ForEachStmt` for `for (T v : coll)`
//! and `ForStmt` for `for (init; cond; update)` -- so `for (...)` compiles
//! to both and matches either. Writing the three-part header out
//! (`for (...; ...; ...)`) pins the C-style form specifically. Any of the
//! three parts may be omitted, including all of them, since a hole standing
//! alone in a sequence is an ellipsis and an ellipsis may consume nothing.
//!
//! **A string literal is opaque.** Holes written inside quotes are text --
//! `'a...b'` is a three-dot string, `'$x'` is a dollar sign -- because a
//! string is data, not structure. The one exception is a literal that is
//! entirely `'...'`, which means *any string literal* and nothing else, the
//! way Semgrep spells it: `System.debug('...')` asks for a logged literal
//! message and does not match `System.debug(x)`.
//!
//! **Declarations are reachable too**, not just the code inside them:
//! `private $T $f;`, `public void $m(...) { ... }`,
//! `@future public static void $m(...) { ... }`. A `(...)` is an argument
//! list or a *parameter* list depending on what the rest of the pattern
//! needs -- the two are written identically and a formal parameter is a
//! `Type name` pair, so both readings are tried. Either way it means any
//! number of them, including none. A whole class body (`class $C { ... }`)
//! is still out of reach: a class holds members, not statements, so that
//! needs a member-run hole.
//!
//! Like `soql`, this binds nothing -- it discovers, parses and walks,
//! skipping the whole `BoundProgram` cost. Matching is purely structural:
//! `System.debug(...)` matches that shape whether or not `System` resolves
//! to the stdlib class, which is a deliberate v1 limit, not an oversight.

use crate::project::{parse_apex_file, site_for, walk_project, ArgError, Site};
use apex_syntax::{NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode};
use apexls_server::LineIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a `...` becomes before the real parser sees it. Any identifier
/// starting with this is an ellipsis hole, which is what lets the
/// multi-token expansions (`__AP_DOTS___T`, `__AP_DOTS___V`, ...) be
/// recognised by the same rule as the bare form.
///
/// A source file containing this identifier verbatim cannot be confused
/// for a hole: [`hole_of`] is only ever asked about *pattern* elements, and
/// a hole in the pattern matches whatever sits opposite it regardless. So
/// the sentinel needs to be unlikely, not reserved.
const ELLIPSIS_IDENT: &str = "__AP_DOTS__";
/// A named hole `$FOO` becomes `__AP_CAP_FOO__`; the capture's own name is
/// spliced between the two halves.
const CAPTURE_PREFIX: &str = "__AP_CAP_";
const CAPTURE_SUFFIX: &str = "__";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Hole {
    /// `...` -- matches any code in this position.
    Ellipsis,
    /// `'...'` -- matches any string *literal*, and nothing else. Distinct
    /// from `Ellipsis` because a hole written inside quotes asks for a
    /// string specifically: `System.debug('...')` means "logging a literal
    /// message", and matching `System.debug(x)` too would lose the
    /// distinction the quotes were there to draw.
    AnyString,
    /// `$NAME` -- matches one construct, and unifies with other
    /// occurrences of the same name within the same match.
    Capture(String),
}

/// A pattern compiled to the syntax node, or nodes, it denotes.
///
/// Held as *green* nodes, not the red `SyntaxNode`s the parser handed back.
/// A red node is a thread-local cursor into the tree and is deliberately
/// neither `Send` nor `Sync`, so a pattern holding one could not cross onto
/// rayon's workers; a `GreenNode` is the `Arc`-based shared half and cloning
/// it is one refcount bump. Each worker rebuilds its own cursors via
/// [`Pattern::nodes`].
#[derive(Debug, Clone)]
struct Pattern {
    /// Several trees, because one written pattern can denote more than one
    /// shape. `for (...)` is the case that forces it: Apex's two loop forms
    /// are different productions -- `ForEachStmt` for `for (T v : coll)`
    /// and `ForStmt` for `for (init; cond; update)` -- so a pattern that
    /// compiled to only one of them silently skipped every loop of the
    /// other kind, which for a search tool is worse than failing outright.
    /// A candidate matches if *any* of these does.
    greens: Vec<apex_syntax::GreenNode>,
}

pub fn run(pattern_src: &str, paths: &[PathBuf]) -> ExitCode {
    let pattern = match Pattern::compile(pattern_src) {
        Ok(p) => p,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let matches = match walk_project(paths, &cwd, |path, src| {
        matches_in_file(&pattern, path, src)
    }) {
        Ok(matches) => matches,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    for m in &matches {
        println!("{m}");
    }

    ExitCode::SUCCESS
}

impl Pattern {
    fn compile(pattern_src: &str) -> Result<Self, ArgError> {
        // Whether a `{`-enclosed `...` means "a run of statements" or "an
        // expression sitting inside one" cannot be decided from the text:
        // `{ ... [SELECT ...] ... }` wants the first reading and
        // `{ ... String $v = ...; ... }` the second, and no lookback rule
        // gets both. So [`substitute`]'s guess is only the *first* attempt.
        // If nothing parses, flip holes to the expression reading and try
        // again -- fewest flips first, so the guess is overridden as little
        // as possible -- and let the parser be the judge. This is the same
        // "try readings until one parses" move the entry-point loop below
        // already makes, and a pattern that compiles on the first attempt
        // pays nothing for it.
        let ambiguous = ambiguous_hole_offsets(pattern_src);
        // 2^n readings, so cap the search. Ten holes is far past any real
        // pattern and still only 1024 attempts over a few hundred bytes.
        let n = ambiguous.len().min(10);
        let mut readings: Vec<u32> = (0..(1u32 << n)).collect();
        readings.sort_by_key(|mask| mask.count_ones());

        // Independently, every whole-header `for (...)` denotes both loop
        // forms at once, so each is compiled both ways and *all* the
        // results are kept -- unlike the reading search above, which picks
        // one. Nested loops multiply, hence the cap.
        let headers = for_header_hole_offsets(pattern_src);
        let h = headers.len().min(4);

        for mask in readings {
            let flipped: Vec<usize> = (0..n)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| ambiguous[bit])
                .collect();

            let mut greens: Vec<apex_syntax::GreenNode> = Vec::new();
            for styles in 0..(1u32 << h) {
                let classic: Vec<usize> = (0..h)
                    .filter(|bit| styles & (1 << bit) != 0)
                    .map(|bit| headers[bit])
                    .collect();
                if let Some(green) = Self::compile_reading(pattern_src, &flipped, &classic) {
                    if !greens.contains(&green) {
                        greens.push(green);
                    }
                }
            }
            if !greens.is_empty() {
                return Ok(Pattern { greens });
            }
        }

        Err(ArgError(
            format!(
                "error: could not parse pattern as Apex: {pattern_src}\n\
                 note: a pattern must be one complete expression, statement or block\n\
                 note: `...` cannot stand for an operator or a whole declaration"
            ),
            2,
        ))
    }

    /// One reading of the pattern, tried against every entry point.
    fn compile_reading(
        pattern_src: &str,
        as_expression: &[usize],
        classic_headers: &[usize],
    ) -> Option<apex_syntax::GreenNode> {
        let substituted = substitute(pattern_src, as_expression, classic_headers);

        // gogrep's multi-entry-point design: try each in turn and take the
        // first that consumes the whole pattern. No user annotation is
        // needed, so Coccinelle's declare-the-kind alternative is not
        // required. Order matters only where a pattern is genuinely
        // ambiguous (`{ ... }` parses as both a statement and a block);
        // first-match-wins is well-defined and the root kind records which.
        // `parse_catch_clause` and `parse_class_member` come last, as
        // fallbacks for constructs the first three cannot name rather than
        // competitors to them. Order matters for the overlaps: a class body
        // admits an initializer block, so `{ ... }` would parse as a member
        // too, and a field declaration is spelled exactly like a local
        // variable one, so `String $v = ...;` would parse as both. Putting
        // members last keeps the statement and block readings, which are
        // what those patterns almost always mean; a pattern that really
        // wants the field says so with a modifier (`private String $v = ...;`).
        let entry_points: [fn(&str) -> apex_parser::Parse; 5] = [
            apex_parser::parse_expression,
            apex_parser::parse_statement,
            apex_parser::parse_block,
            apex_parser::parse_catch_clause,
            apex_parser::parse_class_member,
        ];

        for parse_fn in entry_points {
            let parse = parse_fn(&substituted);
            if !parse.errors.is_empty() {
                continue;
            }
            // Not a `text_range()` coverage check, which looks equivalent
            // and is dead code: `parse_with` completes the root marker over
            // the entire input and the tree is lossless, so the root range
            // always equals the input length whether the grammar consumed
            // the tokens or not. Unconsumed input shows up as *extra
            // children beside* the real node -- which is how
            // `System.debug(...);` is caught being mis-accepted as an
            // expression (`[MethodCallExpr, Semi]`), and how a hole in
            // operator position (`$L $OP $R`, a lone `NameExpr` with two
            // orphaned identifiers after it) is caught at all.
            let root = parse.syntax();
            let significant = significant_children(&root);
            if let [NodeOrToken::Node(node)] = significant.as_slice() {
                return Some(node.green().to_owned());
            }
        }

        None
    }

    /// Fresh red-tree cursors over this pattern's shapes, built per worker.
    fn nodes(&self) -> Vec<SyntaxNode> {
        self.greens
            .iter()
            .map(|g| SyntaxNode::new_root(g.clone()))
            .collect()
    }

    /// Every match of this pattern in `root`, innermost and outermost both
    /// -- a nested match is a real match and hiding it would be worse than
    /// printing two lines.
    fn matches_in(&self, root: &SyntaxNode) -> Vec<SyntaxNode> {
        let patterns = self.nodes();
        root.descendants()
            .filter(|candidate| {
                patterns.iter().any(|pattern| {
                    candidate.kind() == pattern.kind() && {
                        let mut binds = HashMap::new();
                        match_node(pattern, candidate, &mut binds)
                    }
                })
            })
            .collect()
    }
}

/// Rewrite `...` and `$NAME` into ordinary Apex identifiers.
///
/// Position-aware, because a uniform substitution does not survive contact
/// with the grammar. Two rules, both decided from the raw text alone, since
/// there is no tree yet to ask:
///
/// - A hole enclosed by `{` sits where a *statement* is expected, and a
///   bare identifier is not a statement, so it takes a trailing `;`.
/// - `for (...)` and `catch (...)` want a multi-token construct rather than
///   one name -- a loop header and a `Type name` pair respectively.
///
/// Everywhere else -- argument lists, operands, SOQL clauses -- a bare
/// identifier is both correct and sufficient.
///
/// The first rule is a guess, and it is wrong about as often as it is
/// right, because the two readings are genuinely ambiguous from text alone.
/// `{ ... [SELECT ...] ... }` needs the trailing hole read as a statement
/// even though the character before it is `]`; `{ ... String $v = ...; ... }`
/// needs the initializer hole read as an expression even though a `{`
/// encloses it. No lookback rule gets both. So `as_expression` lets
/// [`Pattern::compile`] override the guess per hole and retry -- see
/// [`ambiguous_hole_offsets`].
fn substitute(pattern: &str, as_expression: &[usize], classic_headers: &[usize]) -> String {
    let mut out = String::with_capacity(pattern.len());
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // A string literal is opaque. Substituting inside it was a
            // silent wrong answer: `System.debug('...')` became a search
            // for the literal text `'__AP_DOTS__'` and found nothing,
            // while reading as "any string argument".
            let end = string_literal_end(pattern, i);
            let literal = &pattern[i..end];
            if literal_content(literal) == Some("...") {
                // The whole literal is a hole -- "any string" -- kept as a
                // *string* token so it still matches a string literal and
                // not an identifier. `hole_of` unwraps the quotes.
                out.push('\'');
                out.push_str(ELLIPSIS_IDENT);
                out.push('\'');
            } else {
                out.push_str(literal);
            }
            i = end;
        } else if bytes[i..].starts_with(b"...") {
            let flipped = as_expression.contains(&i);
            let statement = in_statement_position(pattern, i) && !flipped;
            let classic = classic_headers.contains(&i);
            if statement {
                terminate_pending_statement(&mut out);
                out.push_str(&ellipsis_expansion(pattern, i, classic));
            } else {
                out.push_str(&expression_expansion(pattern, i, classic, flipped));
            }
            i += 3;
        } else if bytes[i] == b'}' && in_statement_position(pattern, i) {
            terminate_pending_statement(&mut out);
            out.push('}');
            i += 1;
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

/// Close off a statement the user left unterminated.
///
/// A block pattern is a sequence of statements, and Apex statements end in
/// `;` or `}`. But a user writing `{ ... [SELECT ... FROM $O] ... }` means
/// "a block containing this query", and naturally writes the query without
/// a terminator -- so the text handed to the parser is not a valid block
/// and the whole pattern fails to compile. Appending the `;` they omitted
/// costs nothing and is what they meant. (This parser accepts
/// `[SELECT Id FROM Account];` as an `ExprStmt`, which is what makes the
/// repair possible at all.)
fn terminate_pending_statement(out: &mut String) {
    let last = out.chars().rev().find(|c| !c.is_whitespace());
    if last.is_some_and(|c| !matches!(c, '{' | ';' | '}')) {
        out.push(';');
    }
}

/// Every `...` whose reading [`substitute`] had to guess at, by byte
/// offset. Two kinds, disjoint because a hole is either directly inside a
/// brace or directly inside a paren, never both:
///
/// - A `{`-enclosed hole is a run of statements by default, an expression
///   sitting inside one when flipped.
/// - A `(`-enclosed hole is a call's argument list by default, a
///   declaration's *parameter* list when flipped. `f(...)` and
///   `void m(...)` are written identically and mean different things --
///   a formal parameter is `Type name`, two tokens, so a bare identifier
///   does not parse there.
fn ambiguous_hole_offsets(pattern: &str) -> Vec<usize> {
    let bytes = pattern.as_bytes();
    (0..bytes.len())
        .filter(|&i| {
            bytes[i..].starts_with(b"...")
                && !in_string_literal(pattern, i)
                && (in_statement_position(pattern, i) || in_plain_paren(pattern, i))
        })
        .collect()
}

/// Is the hole directly inside a parenthesised group that is not a `for`
/// or `catch` header? Those two are decided by their keyword; everything
/// else is an argument list or a parameter list, and the text cannot say
/// which.
fn in_plain_paren(pattern: &str, at: usize) -> bool {
    enclosing_paren_offset(pattern, at).is_some()
        && !matches!(
            enclosing_paren_head(pattern, at),
            Some("for") | Some("catch")
        )
}

/// Where the string literal opening at `open` ends, one past its closing
/// quote -- or the end of the pattern if it is unterminated. A backslash
/// escapes the next character, so `'it\'s'` is one literal.
fn string_literal_end(pattern: &str, open: usize) -> usize {
    let bytes = pattern.as_bytes();
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The text between a literal's quotes, if it is properly closed.
fn literal_content(literal: &str) -> Option<&str> {
    literal
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
}

/// Is `at` inside a string literal? Scanned from the start, since quoting
/// is only decidable in order.
fn in_string_literal(pattern: &str, at: usize) -> bool {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() && i <= at {
        if bytes[i] == b'\'' {
            let end = string_literal_end(pattern, i);
            if at > i && at < end - 1 {
                return true;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    false
}

/// Every `...` that stands for a whole `for` header, by byte offset.
///
/// Only a hole that is the *entire* header counts. In an explicitly
/// C-style `for (...; ...; ...)` each hole is one of the three parts, an
/// ordinary expression, and the header's shape is already pinned by the
/// semicolons the user wrote -- so those are left alone.
fn for_header_hole_offsets(pattern: &str) -> Vec<usize> {
    let bytes = pattern.as_bytes();
    (0..bytes.len())
        .filter(|&i| {
            bytes[i..].starts_with(b"...")
                && !in_string_literal(pattern, i)
                && enclosing_paren_head(pattern, i) == Some("for")
                && !encloses_a_semicolon(pattern, i)
        })
        .collect()
}

/// Does the parenthesised group the hole sits in contain a `;`? If so the
/// user wrote the C-style header out themselves.
fn encloses_a_semicolon(pattern: &str, at: usize) -> bool {
    let Some(open) = enclosing_paren_offset(pattern, at) else {
        return false;
    };
    // An explicit loop, not `any`: the scan has to *stop* at the closing
    // paren rather than merely not match it, or a `;` anywhere later in the
    // pattern (the body's statements, invariably) would be read as part of
    // this header and make every `for (...)` look C-style.
    let mut depth = 0i32;
    for c in pattern[open + 1..].chars() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return false,
            ')' => depth -= 1,
            ';' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// The expansion for a hole read as an expression rather than a statement.
fn expression_expansion(pattern: &str, at: usize, classic: bool, alternate: bool) -> String {
    match enclosing_paren_head(pattern, at) {
        Some("for") if encloses_a_semicolon(pattern, at) => ELLIPSIS_IDENT.to_string(),
        Some("for") => for_header_expansion(classic),
        Some("catch") => type_and_name(),
        // Flipped, a plain `(...)` is a declaration's parameter list.
        _ if alternate && in_plain_paren(pattern, at) => type_and_name(),
        _ => ELLIPSIS_IDENT.to_string(),
    }
}

/// A `Type name` pair -- what a formal parameter and a catch parameter
/// both are, and what a single identifier cannot stand in for.
fn type_and_name() -> String {
    format!("{ELLIPSIS_IDENT}_T {ELLIPSIS_IDENT}_N")
}

/// A whole `for` header, in whichever of Apex's two loop forms is being
/// compiled this time round -- `ForEachStmt`'s `T v : coll`, or
/// `ForStmt`'s `init; cond; update`.
fn for_header_expansion(classic: bool) -> String {
    if classic {
        format!("{ELLIPSIS_IDENT}_A; {ELLIPSIS_IDENT}_B; {ELLIPSIS_IDENT}_C")
    } else {
        format!("{ELLIPSIS_IDENT}_T {ELLIPSIS_IDENT}_V : {ELLIPSIS_IDENT}_C")
    }
}

fn ellipsis_expansion(pattern: &str, at: usize, classic: bool) -> String {
    match enclosing_paren_head(pattern, at) {
        Some("for") if encloses_a_semicolon(pattern, at) => ELLIPSIS_IDENT.to_string(),
        Some("for") => for_header_expansion(classic),
        Some("catch") => type_and_name(),
        _ if in_statement_position(pattern, at) => format!("{ELLIPSIS_IDENT};"),
        _ => ELLIPSIS_IDENT.to_string(),
    }
}

/// The identifier or keyword immediately before the open paren of the group
/// the hole sits directly inside -- `for` for `for (...)`, and equally
/// `debug` for `System.debug(...)`, since nothing here distinguishes a
/// keyword from a method name. Callers match only the keywords they care
/// about and let the rest fall through. `None` when the hole is not inside
/// a paren group at all.
fn enclosing_paren_head(pattern: &str, at: usize) -> Option<&str> {
    let open = enclosing_paren_offset(pattern, at)?;
    let head = pattern[..open].trim_end();
    let start = head
        .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// Is the hole at `at` sitting where a *statement* is expected?
///
/// Decided by the nearest enclosing unclosed bracket: inside `{` a block
/// wants statements, inside `(` or `[` an argument list or SOQL clause
/// wants an expression. This replaced an earlier "what is the previous
/// non-space character" rule, which got `{ ... [SELECT ...] ... }` wrong --
/// the trailing hole's previous character is `]`, so it was read as
/// expression position and emitted without the `;` a statement needs.
/// Asking what encloses the hole is both simpler and right, and it still
/// needs nothing but the raw text.
/// Byte offset of the `(` opening the group `at` sits directly inside.
fn enclosing_paren_offset(pattern: &str, at: usize) -> Option<usize> {
    let mut depth = 0i32;
    pattern[..at]
        .char_indices()
        .rev()
        .find_map(|(i, c)| match c {
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
        })
}

fn in_statement_position(pattern: &str, at: usize) -> bool {
    enclosing_open_bracket(pattern, at) == Some('{')
}

/// The nearest bracket opened before `at` and not yet closed.
fn enclosing_open_bracket(pattern: &str, at: usize) -> Option<char> {
    let mut depth = 0i32;
    pattern[..at].chars().rev().find(|&c| match c {
        ')' | ']' | '}' => {
            depth += 1;
            false
        }
        '(' | '[' | '{' if depth == 0 => true,
        '(' | '[' | '{' => {
            depth -= 1;
            false
        }
        _ => false,
    })
}

/// The hole a pattern element denotes, if the element is nothing but a
/// hole. Recognised from the element's significant text rather than its
/// shape, because the parser wraps a bare identifier differently depending
/// on where it sits -- `NameExpr` in expression position, `ExprStmt >
/// NameExpr` in statement position -- and the hole is the same hole either
/// way.
fn hole_of(element: &SyntaxElement) -> Option<Hole> {
    // The element has to be *nothing but* the hole, which means exactly one
    // significant token (plus an optional `;`, since a statement-position
    // hole is substituted as `__AP_DOTS__;`).
    //
    // Matching on the element's concatenated text instead would be subtly
    // and badly wrong: `$A + $B` substitutes to `__AP_CAP_A__ + __AP_CAP_B__`,
    // whose text still begins with the capture prefix and ends with the
    // capture suffix, so a prefix/suffix test reads the whole binary
    // expression as one capture named `A__ + __AP_CAP_B` -- which, being
    // unbound, then matches *anything*. `$X = $Y;` degraded the same way and
    // matched all 48,452 statements in the NPSP corpus. A hole is a token,
    // so it is recognised as a token.
    let mut tokens = match element {
        NodeOrToken::Token(t) => vec![t.clone()],
        NodeOrToken::Node(n) => n
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| !t.kind().is_trivia())
            .collect(),
    };
    if tokens.last().is_some_and(|t| t.kind() == SyntaxKind::Semi) {
        tokens.pop();
    }
    if tokens.is_empty() {
        return None;
    }
    // An element whose every token is a sentinel is an ellipsis, however
    // many tokens that took. The multi-token expansions need this: a
    // formal parameter is `Type name`, so `void m(...)` expands to two
    // sentinels, and unless the pair is recognised as one hole it matches
    // structurally -- meaning *exactly one* parameter rather than any
    // number. Requiring *every* token to be a sentinel is what keeps this
    // from swallowing composites: `$A + $B` and `... + ...` both contain a
    // `+`, so neither is a hole.
    if tokens.iter().all(|t| t.text().starts_with(ELLIPSIS_IDENT)) {
        return Some(Hole::Ellipsis);
    }
    let [only] = tokens.as_slice() else {
        return None;
    };
    // A string hole is a *string* token, so its quotes come off before the
    // sentinel can be seen.
    if is_string_literal_kind(only.kind()) {
        let content = literal_content(only.text()).unwrap_or(only.text());
        return (content == ELLIPSIS_IDENT).then_some(Hole::AnyString);
    }
    let text = only.text();
    if text.starts_with(ELLIPSIS_IDENT) {
        return Some(Hole::Ellipsis);
    }
    text.strip_prefix(CAPTURE_PREFIX)
        .and_then(|rest| rest.strip_suffix(CAPTURE_SUFFIX))
        .map(|name| Hole::Capture(name.to_string()))
}

fn is_string_literal_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral
    )
}

/// Is this element a string literal -- the token itself, or the single
/// expression node wrapping one?
fn is_string_literal(element: &SyntaxElement) -> bool {
    match element {
        NodeOrToken::Token(t) => is_string_literal_kind(t.kind()),
        NodeOrToken::Node(n) => {
            let tokens: Vec<_> = n
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| !t.kind().is_trivia())
                .collect();
            matches!(tokens.as_slice(), [only] if is_string_literal_kind(only.kind()))
        }
    }
}

type Binds = HashMap<String, String>;

fn match_node(pat: &SyntaxNode, src: &SyntaxNode, binds: &mut Binds) -> bool {
    if pat.kind() != src.kind() {
        return false;
    }
    // Deep only inside a block: that is where "anywhere in this loop" has
    // to mean what a user expects. An argument list's `...` stays an
    // ordinary sibling wildcard, since `f(..., $X, ...)` reaching into a
    // nested call's arguments would be nobody's intent.
    let deep = pat.kind() == SyntaxKind::Block;

    // A block's children include its own braces, and they must not take
    // part in the statement sequence: a trailing `...` followed by `}`
    // could never match once the `...` had already consumed everything, so
    // `{ ... P ... }` would fail on exactly the nesting it exists to find.
    // Statements are all nodes, so keeping only the nodes drops both braces
    // without special-casing either token.
    let (pat_items, src_items) = if deep {
        (child_nodes(pat), child_nodes(src))
    } else {
        (significant_children(pat), significant_children(src))
    };
    // Shallow first: it is cheaper, and it is the only reading that can
    // honour a pattern anchored to the block's first or last statement.
    if match_seq(&pat_items, &src_items, binds) {
        return true;
    }
    deep && match_block_deep(&pat_items, &src_items, binds)
}

fn match_element(pat: &SyntaxElement, src: &SyntaxElement, binds: &mut Binds) -> bool {
    match hole_of(pat) {
        Some(Hole::Ellipsis) => return true,
        Some(Hole::AnyString) => return is_string_literal(src),
        Some(Hole::Capture(name)) => {
            let text = match src {
                NodeOrToken::Token(t) => t.text().to_string(),
                NodeOrToken::Node(n) => significant_text(n),
            };
            // Unification, scoped per match: the first occurrence binds,
            // later ones must agree. Compared on significant text so
            // `a.b` and `a . b` are the same capture, matching ast-grep's
            // structural rather than byte-wise notion of "the same", and
            // case-insensitively because Apex identifiers are -- `acc` and
            // `Acc` are one variable, so they are one capture.
            return match binds.get(&name) {
                Some(existing) => existing.eq_ignore_ascii_case(&text),
                None => {
                    binds.insert(name, text);
                    true
                }
            };
        }
        None => {}
    }

    match (pat, src) {
        (NodeOrToken::Token(p), NodeOrToken::Token(s)) => {
            p.kind() == s.kind() && token_text_eq(p.kind(), p.text(), s.text())
        }
        (NodeOrToken::Node(p), NodeOrToken::Node(s)) => match_node(p, s, binds),
        _ => false,
    }
}

/// Compare two token texts the way Apex itself compares them.
///
/// **Apex is a case-insensitive language**: `Database.query(q)` and
/// `database.query(q)` are the same call, and a pattern that found only one
/// spelling would silently under-report -- on the NPSP corpus
/// `Database.query(...)` and `database.query(...)` return 260 and 64 hits
/// respectively, and both are the same set of call sites written two ways.
/// `soql.rs` already matches its `Database` receiver with
/// `eq_ignore_ascii_case` for exactly this reason.
///
/// String literals are the exception: case-insensitivity is a property of
/// Apex's *identifiers and keywords*, not of the characters inside a
/// string, where `'USER_MODE'` and `'user_mode'` are genuinely different
/// values.
fn token_text_eq(kind: SyntaxKind, pat: &str, src: &str) -> bool {
    if matches!(
        kind,
        SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral
    ) {
        pat == src
    } else {
        pat.eq_ignore_ascii_case(src)
    }
}

/// Match a pattern element sequence against a source one, at one level.
///
/// Without any ellipsis this is an exact, element-for-element comparison,
/// which is what makes `{ P }` mean "a block whose only statement is P". An
/// ellipsis consumes zero or more elements, with backtracking over where it
/// stops.
///
/// Strictly shallow: everything here compares siblings. Crossing block
/// boundaries is [`match_block_deep`]'s job, tried afterwards.
fn match_seq(pat: &[SyntaxElement], src: &[SyntaxElement], binds: &mut Binds) -> bool {
    let Some(head) = pat.first() else {
        return src.is_empty();
    };

    if hole_of(head) == Some(Hole::Ellipsis) {
        let rest = &pat[1..];
        if rest.is_empty() {
            return true;
        }
        for split in 0..=src.len() {
            let mut attempt = binds.clone();
            if match_seq(rest, &src[split..], &mut attempt) {
                *binds = attempt;
                return true;
            }
        }
        return false;
    }

    let Some(first) = src.first() else {
        return false;
    };
    match_element(head, first, binds) && match_seq(&pat[1..], &src[1..], binds)
}

/// The fixed elements of a block pattern shaped `... P ... Q ... `, in
/// order -- or `None` if the pattern is not that shape.
///
/// Deep matching is only offered for a pattern that is unanchored at both
/// ends and separates every fixed element with an ellipsis, i.e. the
/// element list alternates `[..., P, ..., Q, ...]`. Everything else is
/// asking for something a descendant search cannot honestly answer:
///
/// - No leading or trailing ellipsis means the user anchored that end to
///   the block's first or last *statement*, and a match buried at depth is
///   not that. `{ ... P }` must stay shallow or it stops meaning "P is
///   last".
/// - Two fixed elements with no ellipsis between them ask to be
///   *consecutive*, and consecutiveness is meaningless once the two may sit
///   at different depths.
fn deep_plan(pat: &[SyntaxElement]) -> Option<Vec<SyntaxElement>> {
    // An alternating `[..., P, ..., Q, ...]` list is always odd-length and
    // at least three long.
    if pat.len() < 3 || pat.len().is_multiple_of(2) {
        return None;
    }
    let mut fixed = Vec::with_capacity(pat.len() / 2);
    for (i, element) in pat.iter().enumerate() {
        let is_ellipsis = hole_of(element) == Some(Hole::Ellipsis);
        if i % 2 == 0 {
            if !is_ellipsis {
                return None;
            }
        } else {
            if is_ellipsis {
                return None;
            }
            fixed.push(element.clone());
        }
    }
    Some(fixed)
}

/// Match a block pattern against the block's whole subtree rather than its
/// immediate children -- what makes `...` inside a block *deep*.
///
/// The block's descendants are flattened into one document-ordered list and
/// the pattern's fixed elements are matched across it with a forward-only
/// cursor, so `{ ... P ... Q ... }` means "P somewhere, then Q somewhere
/// after it" however deeply either is nested. Matching each fixed element
/// independently would not do: that is what made this shape unanswerable
/// before, because a descendant match left no defined place to resume the
/// search for Q.
fn match_block_deep(pat: &[SyntaxElement], src: &[SyntaxElement], binds: &mut Binds) -> bool {
    let Some(fixed) = deep_plan(pat) else {
        return false;
    };
    let candidates: Vec<SyntaxElement> = src
        .iter()
        .filter_map(|e| e.as_node())
        .flat_map(|n| n.descendants())
        .map(NodeOrToken::Node)
        .collect();
    match_in_document_order(&fixed, &candidates, binds)
}

fn match_in_document_order(
    fixed: &[SyntaxElement],
    candidates: &[SyntaxElement],
    binds: &mut Binds,
) -> bool {
    let Some(head) = fixed.first() else {
        return true;
    };
    // Try the element as written, and -- if the user wrote a bare
    // *expression* where the block grammar demanded a statement -- as that
    // expression too. `{ ... [SELECT ...] ... }` means "a block containing
    // this query somewhere", and the query turns up inside a declaration or
    // an argument, not as a statement of its own.
    let unwrapped = expression_inside(head);
    for (i, candidate) in candidates.iter().enumerate() {
        for probe in [Some(head), unwrapped.as_ref()].into_iter().flatten() {
            let mut attempt = binds.clone();
            if match_element(probe, candidate, &mut attempt)
                && match_in_document_order(&fixed[1..], &candidates[i + 1..], &mut attempt)
            {
                *binds = attempt;
                return true;
            }
        }
    }
    false
}

/// The single expression an `ExprStmt` pattern element wraps, if that is
/// all it is. `None` for any other element, including an `ExprStmt` that
/// holds more than one significant child.
fn expression_inside(element: &SyntaxElement) -> Option<SyntaxElement> {
    let NodeOrToken::Node(node) = element else {
        return None;
    };
    if node.kind() != SyntaxKind::ExprStmt {
        return None;
    }
    match significant_children(node).as_slice() {
        [NodeOrToken::Node(inner)] => Some(NodeOrToken::Node(inner.clone())),
        // The `;` is a child too, so an expression plus its terminator is
        // still just an expression.
        [NodeOrToken::Node(inner), NodeOrToken::Token(semi)] if semi.kind() == SyntaxKind::Semi => {
            Some(NodeOrToken::Node(inner.clone()))
        }
        _ => None,
    }
}

fn significant_children(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children_with_tokens()
        .filter(|c| !c.kind().is_trivia())
        .collect()
}

/// Just the child *nodes*, dropping every token. Used for a block, whose
/// tokens are its braces.
fn child_nodes(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children().map(NodeOrToken::Node).collect()
}

fn significant_text(node: &SyntaxNode) -> String {
    apex_syntax::significant_range(node).map_or_else(String::new, |range| {
        let full = node.text_range();
        let text = node.text().to_string();
        let start = usize::from(range.start() - full.start());
        let end = usize::from(range.end() - full.start());
        text[start..end].to_string()
    })
}

fn matches_in_file(pattern: &Pattern, display_path: &Path, src: &str) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);

    pattern
        .matches_in(&parse.syntax())
        .into_iter()
        .filter_map(|node| site_for(display_path, src, &index, &node))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile `pattern`, search `src` as a class, and render each hit the
    /// way the CLI would minus the path.
    fn hits(pattern: &str, src: &str) -> Vec<String> {
        let pattern = Pattern::compile(pattern).expect("pattern should compile");
        matches_in_file(&pattern, Path::new("T.cls"), src)
            .into_iter()
            .map(|m| format!("{}:{}:{}", m.line, m.col, m.text))
            .collect()
    }

    fn wrap(body: &str) -> String {
        format!("public class T {{\n    public void run() {{\n{body}\n    }}\n}}\n")
    }

    #[test]
    fn compiles_each_grammatical_position_a_hole_can_sit_in() {
        for pattern in [
            "System.debug(...)",
            "System.debug(...);",
            "$X.size() > 0",
            "[SELECT ... FROM $OBJ]",
            "for (...) { ... }",
            "{ ... System.debug(...); ... }",
            "try { ... } catch (...) { }",
            "insert $X;",
        ] {
            assert!(
                Pattern::compile(pattern).is_ok(),
                "should compile: {pattern}"
            );
        }
    }

    /// The one position no substitution can reach: an operator is not an
    /// identifier. It must fail loudly at compile time rather than silently
    /// matching a bare name.
    #[test]
    fn rejects_a_hole_in_operator_position() {
        let err = Pattern::compile("$LEFT $OP $RIGHT").expect_err("operator holes are unreachable");
        assert!(err.0.contains("could not parse pattern"), "{}", err.0);
    }

    /// The flagship query: SOQL anywhere inside a loop, corpus item 1.
    ///
    /// A user writing this means "a block containing this query", and writes
    /// the query the way it appears in code -- as an expression, with no
    /// terminator. Two things make that work: the missing `;` is supplied so
    /// the block parses at all, and the resulting `ExprStmt` wrapper is
    /// unwrapped when searching descendants, so the query is found wherever
    /// it really sits. Here it sits in a *declaration*, which is the shape
    /// the assignment form misses entirely.
    #[test]
    fn finds_a_bare_expression_anywhere_inside_a_loop() {
        let declared = wrap(
            "        for (Account a : accounts) {\n            List<Contact> cs = [SELECT Id FROM Contact];\n        }",
        );
        assert_eq!(
            hits("for (...) { ... [SELECT ... FROM $O] ... }", &declared).len(),
            1,
            "the query is inside a declaration, not a statement of its own",
        );

        // Still deep, and still a loop: nested two blocks down, and absent
        // from a method with no loop at all.
        let nested = wrap(
            "        for (Account a : accounts) {\n            if (a.Name != null) {\n                insert [SELECT Id FROM Contact];\n            }\n        }",
        );
        assert_eq!(
            hits("for (...) { ... [SELECT ... FROM $O] ... }", &nested).len(),
            1,
        );
        let unlooped = wrap("        List<Contact> cs = [SELECT Id FROM Contact];");
        assert!(
            hits("for (...) { ... [SELECT ... FROM $O] ... }", &unlooped).is_empty(),
            "a query outside any loop is not a governor-limit bug",
        );
    }

    /// The repair is only for a segment the user left unterminated -- it must
    /// not paper over a genuinely unparseable pattern.
    #[test]
    fn an_unterminated_segment_is_repaired_but_nonsense_is_still_rejected() {
        assert!(Pattern::compile("{ ... [SELECT ... FROM $O] }").is_ok());
        assert!(Pattern::compile("$LEFT $OP $RIGHT").is_err());
    }

    /// A `{`-enclosed `...` is ambiguous: a run of statements, or an
    /// expression sitting inside one. No lookback rule decides it, because
    /// `{ ... [SELECT ...] ... }` wants the first reading and
    /// `{ ... String $v = ...; ... }` the second. Both must compile, which
    /// is what the reading retry in `compile` buys.
    #[test]
    fn both_readings_of_a_brace_enclosed_hole_compile() {
        for pattern in [
            "{ ... [SELECT ... FROM $O] ... }",
            "{ ... String $v = ...; ... }",
            "{ ...; String $v = ...; ...; }",
            "{ ... return ...; ... }",
            "{ ... $x = f(...); ... }",
        ] {
            assert!(
                Pattern::compile(pattern).is_ok(),
                "should compile: {pattern}"
            );
        }
    }

    /// The hole in an initializer is an expression, so it matches whatever
    /// initialises the variable -- and the statement it sits in is still
    /// found at any depth.
    #[test]
    fn a_hole_in_an_initializer_matches_any_initialiser() {
        let src = wrap(
            "        for (Account a : accounts) {\n            if (x) {\n                String s = a.Name + '!';\n            }\n        }",
        );
        assert_eq!(hits("{ ... String $v = ...; ... }", &src).len(), 3);
        assert!(
            hits("{ ... Integer $v = ...; ... }", &src).is_empty(),
            "the declared type is still part of the shape",
        );
    }

    #[test]
    fn finds_a_call_shape_regardless_of_its_arguments() {
        let src = wrap(
            "        System.debug('a');\n        System.debug(x, y);\n        Other.debug('a');",
        );
        assert_eq!(
            hits("System.debug(...);", &src),
            vec![
                "3:9:System.debug('a');".to_string(),
                "4:9:System.debug(x, y);".to_string(),
            ],
        );
    }

    #[test]
    fn captures_match_any_single_construct() {
        let src = wrap("        if (accounts.size() > 0) { return; }");
        assert_eq!(
            hits("$X.size() > 0", &src),
            vec!["3:13:accounts.size() > 0".to_string()],
        );
    }

    /// Reusing a capture requires both occurrences to be the same code --
    /// the property that makes a capture more than a wildcard.
    #[test]
    fn a_reused_capture_must_match_the_same_code() {
        let same = wrap("        if (a != null) { a.doIt(); }");
        let different = wrap("        if (a != null) { b.doIt(); }");
        let pattern = "if ($X != null) { $X.doIt(); }";
        assert_eq!(hits(pattern, &same).len(), 1, "same receiver should match");
        assert!(
            hits(pattern, &different).is_empty(),
            "a different receiver must not match"
        );
    }

    /// The motivating query, and the reason `...` is deep: the SOQL call is
    /// nested inside an `if` inside the loop, and that is still the
    /// governor-limit bug.
    #[test]
    fn ellipsis_in_a_block_crosses_block_boundaries() {
        let src = wrap(
            "        for (Account a : accounts) {\n            if (a.Name != null) {\n                System.debug(a);\n            }\n        }",
        );
        assert_eq!(
            hits("for (...) { ... System.debug(...); ... }", &src).len(),
            1,
            "a debug nested two blocks deep is still inside the loop",
        );
    }

    /// The converse: no ellipsis means an exact block, so a loop with other
    /// statements in it does not match a single-statement pattern.
    #[test]
    fn a_block_pattern_without_an_ellipsis_is_exact() {
        let only =
            wrap("        for (Account a : accounts) {\n            System.debug(a);\n        }");
        let plus = wrap(
            "        for (Account a : accounts) {\n            System.debug(a);\n            count++;\n        }",
        );
        let pattern = "for (...) { System.debug(...); }";
        assert_eq!(hits(pattern, &only).len(), 1, "exact single statement");
        assert!(
            hits(pattern, &plus).is_empty(),
            "an extra statement must break an exact block match"
        );
    }

    /// Trivia is not structure: a pattern written tightly must match source
    /// written loosely, comments included.
    #[test]
    fn matching_ignores_whitespace_and_comments() {
        let src = wrap("        System . debug( /* why */ 'a' );");
        assert_eq!(hits("System.debug(...);", &src).len(), 1);
    }

    /// A hole is one token, so a *composite* pattern must never collapse
    /// into a single match-anything hole. `$A + $B` substitutes to text that
    /// still begins with the capture prefix and ends with the capture
    /// suffix, so recognising holes by text prefix/suffix read the whole
    /// binary expression as one unbound capture -- and `$X = $Y;` then
    /// matched all 48,452 statements in the NPSP corpus.
    #[test]
    fn a_composite_pattern_is_not_mistaken_for_one_hole() {
        let src =
            wrap("        Database.query(soql);\n        Database.query(a + b);\n        x = y;");
        assert_eq!(
            hits("Database.query($A + $B)", &src),
            vec!["4:9:Database.query(a + b)".to_string()],
            "a plain variable argument is not a concatenation",
        );
        // An assignment pattern matches assignments, not every statement.
        assert_eq!(hits("$X = $Y;", &src).len(), 1);
    }

    /// A trailing `...` is load-bearing, so deep descent is only sound when
    /// the fixed element is followed by one. Without this, `{ ... P }`
    /// matched a block with P buried in the middle and further statements
    /// after it.
    #[test]
    fn deep_descent_requires_a_trailing_ellipsis() {
        let buried = wrap(
            "        for (Account a : accounts) {\n            if (x) {\n                System.debug(a);\n            }\n            count++;\n        }",
        );
        assert!(
            hits("for (...) { ... System.debug(...); }", &buried).is_empty(),
            "no trailing `...` means the debug must be the block's last statement",
        );
        assert_eq!(
            hits("for (...) { ... System.debug(...); ... }", &buried).len(),
            1,
            "with a trailing `...` the same nesting matches",
        );
    }

    /// `... P ... Q ...` -- "P somewhere, then Q somewhere after it" --
    /// matched across the block's whole subtree in document order, however
    /// deeply either sits. This is the shape behind every ordering query:
    /// acquire/release, open/close, startTest/stopTest.
    #[test]
    fn two_fixed_elements_around_an_ellipsis_match_in_document_order() {
        let nested = wrap(
            "        for (Account a : accounts) {\n            if (x) {\n                System.debug(a);\n                insert a;\n            }\n        }",
        );
        assert_eq!(
            hits(
                "for (...) { ... System.debug(...); ... insert $X; ... }",
                &nested
            )
            .len(),
            1,
            "both nested two blocks deep, in order",
        );

        // Order is real, not incidental: the same two elements the other
        // way round must not match.
        assert!(
            hits(
                "for (...) { ... insert $X; ... System.debug(...); ... }",
                &nested
            )
            .is_empty(),
            "Q before P is a different query and must not match",
        );

        // And they may sit at different depths from one another.
        let straddling = wrap(
            "        for (Account a : accounts) {\n            System.debug(a);\n            if (x) {\n                insert a;\n            }\n        }",
        );
        assert_eq!(
            hits(
                "for (...) { ... System.debug(...); ... insert $X; ... }",
                &straddling
            )
            .len(),
            1,
        );
    }

    /// Corpus item 4: a `catch` that swallows its exception.
    ///
    /// A bare `catch` is not valid Apex -- a real org rejects it with
    /// "Unexpected token 'catch'" -- so this is a pattern the compiler
    /// would refuse, parsed through a fragment entry point that exists
    /// precisely to name sub-constructs. Anchoring on the `catch` rather
    /// than the whole `try` is what lets it report the clause's own
    /// position and isolate one clause of a multi-`catch`.
    #[test]
    fn finds_a_swallowing_catch_on_its_own() {
        let src = wrap(
            "        try {\n            insert a;\n        } catch (DmlException e) {\n        } catch (QueryException e) {\n            System.debug(e);\n        }",
        );
        assert_eq!(
            hits("catch (...) { }", &src),
            vec!["5:11:catch (DmlException e) { }".to_string()],
            "the empty clause only, reported at its own position",
        );
        assert_eq!(
            hits("catch (...) { System.debug(...); }", &src).len(),
            1,
            "the log-and-continue clause, isolated from its sibling",
        );
    }

    /// Apex is a case-insensitive language, so a pattern must be too --
    /// otherwise `Database.query(...)` and `database.query(...)` report
    /// different halves of the same set of call sites.
    #[test]
    fn matching_is_case_insensitive_like_apex_itself() {
        let src = wrap(
            "        Database.query(q);\n        database.QUERY(q);\n        DATABASE.query(q);",
        );
        for pattern in ["Database.query(...);", "database.QUERY(...);"] {
            assert_eq!(
                hits(pattern, &src).len(),
                3,
                "every spelling is the same call: {pattern}",
            );
        }
    }

    /// ...but case-insensitivity belongs to identifiers and keywords, not
    /// to the characters inside a string, where case is a real difference
    /// in value.
    /// A string literal is opaque, so a hole written inside one is text,
    /// not a hole -- with a single exception: a literal that is *entirely*
    /// `'...'` means "any string", the way Semgrep spells it.
    ///
    /// Before this, `System.debug('...')` substituted inside the quotes and
    /// became a search for the literal text `'__AP_DOTS__'`, so it found
    /// nothing at all while reading as "any string argument" -- a confident
    /// wrong answer rather than an error.
    #[test]
    fn a_whole_string_literal_hole_matches_any_string() {
        let src = wrap(
            "        System.debug('hello');\n        System.debug('world');\n        System.debug(x);",
        );
        assert_eq!(
            hits("System.debug('...');", &src).len(),
            2,
            "any string literal, but not a non-literal argument",
        );
        assert_eq!(
            hits("System.debug('hello');", &src),
            vec!["3:9:System.debug('hello');".to_string()],
            "an ordinary literal still matches only itself",
        );
    }

    /// Holes inside a string are literal text -- the string is data, not
    /// structure, so `'a...b'` is a three-dot string and `'$x'` is a dollar
    /// sign.
    #[test]
    fn holes_inside_a_string_literal_are_just_text() {
        let src = wrap("        f('a...b');\n        f('$x');\n        f('zzz');");
        assert_eq!(hits("f('a...b');", &src).len(), 1);
        assert_eq!(hits("f('$x');", &src).len(), 1);
        // And a string is opaque to the substituter, so quotes in the
        // pattern do not derail the holes outside them.
        assert_eq!(hits("f('a...b');", &src).len(), 1);
        assert_eq!(hits("f(...);", &src).len(), 3);
    }

    #[test]
    fn string_literal_contents_stay_case_sensitive() {
        let src = wrap("        f('USER_MODE');\n        f('user_mode');");
        assert_eq!(
            hits("f('USER_MODE');", &src),
            vec!["3:9:f('USER_MODE');".to_string()],
        );
    }

    /// A capture is an identifier, so its two occurrences unify across a
    /// difference in case.
    #[test]
    fn a_reused_capture_unifies_across_case() {
        let src = wrap("        if (acc != null) { Acc.doIt(); }");
        assert_eq!(hits("if ($X != null) { $X.doIt(); }", &src).len(), 1);
    }

    #[test]
    fn reports_every_nested_match_not_just_the_outermost() {
        let src = wrap(
            "        for (Account a : outer) {\n            for (Account b : inner) {\n                System.debug(b);\n            }\n        }",
        );
        assert_eq!(
            hits("for (...) { ... System.debug(...); ... }", &src).len(),
            2,
            "both the outer and inner loop contain the debug call",
        );
    }

    #[test]
    fn finds_soql_inside_a_loop() {
        let src = wrap(
            "        for (Account a : accounts) {\n            cs = [SELECT Id FROM Contact];\n        }",
        );
        assert_eq!(
            hits("for (...) { ... $X = [SELECT ... FROM $OBJ]; ... }", &src).len(),
            1,
        );
    }

    /// The pattern language is structural, so an assignment and a variable
    /// *declaration* are different shapes and one pattern does not catch
    /// both -- `cs = [...]` matches, `List<Contact> cs = [...]` does not.
    /// This is the known ceiling that Coccinelle-style isomorphisms exist to
    /// lift (see the map's "Not yet specified"), recorded here as behaviour
    /// rather than left for a user to discover.
    #[test]
    fn an_assignment_pattern_does_not_match_a_declaration() {
        let declared = wrap(
            "        for (Account a : accounts) {\n            List<Contact> cs = [SELECT Id FROM Contact];\n        }",
        );
        assert!(
            hits(
                "for (...) { ... $X = [SELECT ... FROM $OBJ]; ... }",
                &declared
            )
            .is_empty(),
            "a declaration is a different tree from an assignment",
        );
        // The SOQL expression itself is still findable, which is the
        // workaround a user reaches for until isomorphisms exist.
        assert_eq!(hits("[SELECT ... FROM $OBJ]", &declared).len(), 1);
    }

    /// Apex has two loop forms and they are different productions, so one
    /// written `for (...)` has to denote both. Matching only the for-each
    /// form made every C-style loop invisible to every loop query -- a
    /// silent under-report, which for a search tool is worse than an error.
    #[test]
    fn for_matches_both_loop_forms() {
        let each = wrap("        for (Account a : accounts) {\n            insert a;\n        }");
        let classic = wrap(
            "        for (Integer i = 0; i < n; i++) {\n            insert rows[i];\n        }",
        );
        let bare = wrap("        for (Integer i = 0; i < n; i++) {\n            f();\n        }");

        for src in [&each, &classic] {
            assert_eq!(
                hits("for (...) { ... insert $X; ... }", src).len(),
                1,
                "both loop forms are loops",
            );
        }
        assert!(hits("for (...) { ... insert $X; ... }", &bare).is_empty());

        // A C-style header may omit any of its three parts, and `for (...)`
        // still covers it: a hole standing alone in a sequence is an
        // ellipsis, and an ellipsis may consume nothing. NPSP's
        // `for ( ;j<installments;j++ )` -- which contains a DML insert, and
        // was invisible before -- is exactly this shape.
        let no_init = wrap("        for ( ; i < n; i++) {\n            insert rows[i];\n        }");
        let empty = wrap("        for (;;) {\n            insert row;\n        }");
        for src in [&no_init, &empty] {
            assert_eq!(hits("for (...) { ... insert $X; ... }", src).len(), 1);
        }
    }

    /// The C-style header can also be written out, which pins the shape and
    /// lets a pattern ask for that form specifically.
    #[test]
    fn an_explicit_c_style_header_matches_only_that_form() {
        let each = wrap("        for (Account a : accounts) {\n            f();\n        }");
        let classic =
            wrap("        for (Integer i = 0; i < n; i++) {\n            f();\n        }");

        assert_eq!(hits("for (...; ...; ...) { ... }", &classic).len(), 1);
        assert!(
            hits("for (...; ...; ...) { ... }", &each).is_empty(),
            "an explicit three-part header is not a for-each loop",
        );
        // And the parts are still holes, so the header's contents vary.
        assert_eq!(hits("for (...; $C; ...) { ... }", &classic).len(), 1);
    }

    #[test]
    fn finds_dml_inside_a_loop() {
        let src = wrap("        for (Account a : accounts) {\n            insert a;\n        }");
        assert_eq!(hits("for (...) { ... insert $X; ... }", &src).len(), 1);
    }

    #[test]
    fn position_is_anchored_past_a_leading_comment() {
        let src = wrap("        /* note */ System.debug('a');");
        assert_eq!(
            hits("System.debug(...);", &src),
            vec!["3:20:System.debug('a');".to_string()],
        );
    }

    /// Declarations are reachable at all only through the member entry
    /// point -- an expression, statement or block pattern cannot name one.
    /// This is what puts `@future` and `@AuraEnabled` methods, and fields,
    /// within reach of a query.
    #[test]
    fn finds_declarations_not_just_code_inside_them() {
        let src = "public class T {\n    private String secret;\n    @future public static void go(Id x) { f(); }\n    public void plain() { }\n}\n";
        let find = |pattern: &str| {
            let p = Pattern::compile(pattern).expect("pattern should compile");
            matches_in_file(&p, Path::new("T.cls"), src).len()
        };
        assert_eq!(find("private $T $f;"), 1);
        assert_eq!(find("@future public static void $m(...) { ... }"), 1);
        assert_eq!(find("public void $m() { ... }"), 1, "no parameters");
        assert_eq!(find("@isTest static void $m() { ... }"), 0);
    }

    /// `f(...)` and `void m(...)` are written identically and mean
    /// different things: an argument list takes expressions, a parameter
    /// list takes `Type name` pairs. Neither reading can be chosen from the
    /// text, so both are tried -- and each pattern must land on its own.
    #[test]
    fn a_paren_hole_is_arguments_or_parameters_as_the_pattern_requires() {
        let src = "public class T {\n    public void go(Id x, String y) { f(1, 2); }\n}\n";
        let find = |pattern: &str| {
            let p = Pattern::compile(pattern).expect("pattern should compile");
            matches_in_file(&p, Path::new("T.cls"), src).len()
        };
        assert_eq!(find("public void $m(...) { ... }"), 1, "two parameters");
        assert_eq!(find("f(...);"), 1, "argument list");

        // `(...)` is any number of parameters, not exactly one.
        let none = "public class T {
    public void go() { }
}
";
        let three = "public class T {
    public void go(Id a, String b, Integer c) { }
}
";
        for src in [none, three] {
            let p = Pattern::compile("public void $m(...) { ... }").expect("compiles");
            assert_eq!(matches_in_file(&p, Path::new("T.cls"), src).len(), 1);
        }
    }

    #[test]
    fn searches_trigger_files_too() {
        let src = "trigger T on Account (before insert) {\n    System.debug('a');\n}\n";
        let pattern = Pattern::compile("System.debug(...);").expect("compiles");
        let found = matches_in_file(&pattern, Path::new("T.trigger"), src);
        assert_eq!(found.len(), 1, "a trigger is not a compilation unit");
    }
}
