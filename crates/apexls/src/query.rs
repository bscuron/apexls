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
//! **How a pattern becomes a tree.** Holes are real tokens. `...` and
//! `$NAME` are folded into `PatternHole`/`PatternCapture` before parsing
//! (`apex_parser::parse_pattern`), and the ordinary Apex grammar accepts
//! one wherever it *requires* a construct -- a name, a type, a value, a
//! parameter list, a loop header, a SOQL clause. Nothing about real Apex
//! parsing changes, because those tokens cannot arise from real source.
//!
//! This replaced substituting each hole with an ordinary identifier before
//! parsing. That only works where an identifier is grammatical, so it
//! needed a different filler per position -- `;` for a statement, `T V : C`
//! for a `for` header, `T N` for a parameter -- and could not reach a SOQL
//! value or an operator at all. The list had no end, because "is this text
//! grammatical here" has one answer per grammar position.
//!
//! **The rule is uniform: `...` means "anything here", and omitting it
//! means exact.** `[SELECT ... FROM $o ...]` is any query (2,132 on the
//! NPSP corpus, identical to `kind:SoqlExpr`); `[SELECT ... FROM $o]` is a
//! query with no trailing clauses at all (520). Likewise `{ P }` is a block
//! whose only statement is P, while `{ ... P ... }` is a block containing
//! P.
//!
//! **What `...` means inside a block** is *deep*: it crosses block
//! boundaries, so `{ ... P ... }` means "contains P at any depth". That is
//! the point for the motivating query -- a SOQL call nested in an `if`
//! inside a `for` is still the governor-limit bug. Deep matching runs over
//! the block's subtree flattened into document order with a forward-only
//! cursor, so `{ ... P ... Q ... }` means "P somewhere, then Q somewhere
//! after it" and the reverse order is a different query. It is offered only
//! for a pattern unanchored at both ends that separates every fixed element
//! with an ellipsis; everything else stays shallow rather than guessing,
//! since a missing leading or trailing `...` anchors that end to the
//! block's first or last statement, and two fixed elements with no `...`
//! between them ask to be *consecutive*, which is meaningless once they may
//! sit at different depths.
//!
//! **One `for (...)` covers both loop forms.** Apex's two loop productions
//! are different trees, and a pattern that matched only the for-each form
//! made every C-style loop invisible to every loop query. A hole header
//! parses once and [`kinds_compatible`] treats the two as the same
//! construct.
//!
//! **A string literal is opaque.** Holes written inside quotes are text --
//! `'a...b'` is a three-dot string, `'$x'` a dollar sign -- because a
//! string is data, not structure. The exception is a literal that is
//! entirely `'...'`, which means *any string literal* and nothing else, the
//! way Semgrep spells it.
//!
//! **`$_` matches one construct and binds nothing**, so `f($_, $_)` reads
//! as "two arbitrary arguments" rather than "two identical ones" -- the
//! spelling Semgrep, ast-grep and GritQL all share. A named capture still
//! unifies, including inside generic type arguments:
//! `Map<$K, $V> $v = new Map<$K, $V>();` requires the declared and
//! constructed element types to agree.
//!
//! **Declarations are reachable**, not just the code inside them:
//! `private $T $f;`, `public void $m(...) { ... }`,
//! `@future public static void $m(...) { ... }`.
//!
//! **Operator position is the one place a hole cannot go.** `$L $OP $R`
//! does not compile. Every other hole satisfies a *requirement*, which is
//! unambiguous; an operator is reached only by *choosing* to continue a
//! binary expression, and a hole admitted there read `{ ... $X = y; }` as
//! "the ellipsis, operated on by `$X`" and swallowed the statement after
//! it. Rejected outright rather than answered wrongly.
//!
//! **Two escape hatches**, for the questions a pattern literal cannot ask.
//! `kind:Name` matches any node of a syntax kind, and `regex:RE` any node
//! whose significant text matches, anchored. Both cost the user something
//! the literal form does not -- grammar node names, or a second language --
//! which is why they are a fallback rather than the main road. They earn
//! their place on questions with no literal form at all: a SOQL `WHERE`
//! hole cannot be written, since a SOQL value must be a literal or a bind
//! rather than an identifier, and a hardcoded Salesforce Id is a string of
//! a particular length and alphabet that no tree shape distinguishes.
//!
//! **`--not` and `--containing`** filter a match by what it holds, which is
//! how a question about *absence* gets asked at all. "Contains" includes
//! the match itself, not only its descendants, so a negation can narrow a
//! pattern rather than only exclude what is nested inside it:
//! `kind:SoqlExpr --not kind:SoqlWhereClause --not kind:SoqlLimit` is
//! "a query with neither clause", and all three name the same node.
//!
//! **`$...NAME`** captures a *run* of elements rather than one, and binds
//! its source text, which is what lets a variable-length run survive a
//! rewrite: `Database.query($...ARGS)` -> `Database.queryWithBinds($...ARGS, ...)`
//! keeps a call'''s arguments whatever their number.
//!
//! **Replace** is `--replace TEMPLATE`, a string template taking only the
//! pattern's *named* captures -- a bare `...` has nothing to refer to on
//! the output side and is rejected before any file is touched. The match's
//! significant range is spliced, so untouched formatting survives by
//! construction; an empty template deletes, and takes its whole line when
//! nothing else is on it. Nested matches collapse to the outermost, since
//! the inner text is part of what the outer rewrite replaces; genuinely
//! crossing overlaps are refused and both named. Every rewritten file is
//! re-parsed and **not written if it gained parse errors**.
//!
//! Like `soql`, this binds nothing -- it discovers, parses and walks,
//! skipping the whole `BoundProgram` cost. Matching is purely structural:
//! `System.debug(...)` matches that shape whether or not `System` resolves
//! to the stdlib class, which is a deliberate v1 limit, not an oversight.

use crate::project::{parse_apex_file, site_for, walk_project, ArgError, Site};
use apex_parser::Fragment;
use apex_syntax::{NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode};
use apexls_server::LineIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Stripped from a capture token's text (`$NAME`) to get the name.
const HOLE_CAPTURE_SIGIL: char = '$';
/// Apex string literals are single-quoted.
const STRING_QUOTE: char = '\'';

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
    /// `$...NAME` -- matches a *run* of elements, zero or more, and binds
    /// the source text of the whole run.
    ///
    /// The sequence counterpart of [`Hole::Capture`], and the only way to
    /// carry a variable-length run across a rewrite: `f($...ARGS)` ->
    /// `g($...ARGS)` keeps a call's arguments whatever their number, which
    /// a one-construct capture cannot do.
    SeqCapture(String),
    /// `$_` -- matches one construct and binds nothing.
    ///
    /// Distinct from `Capture("_")` because a name that unifies makes
    /// `f($_, $_)` mean "two *identical* arguments", when every tool in the
    /// survey (Semgrep, ast-grep, GritQL) spells "two arbitrary arguments"
    /// exactly that way. Distinct from [`Hole::Ellipsis`] because it
    /// matches exactly one element rather than any number of them.
    Anonymous,
}

/// A replacement template: literal text with capture holes to fill in.
///
/// A *string* template, not a tree transform -- the near-universal choice
/// (ast-grep, Semgrep, Comby), and the right one here because the source is
/// never re-printed. A rewrite is a byte-range splice into text that keeps
/// its own formatting, which is the problem Comby disclaims outright and
/// Semgrep has open indentation bugs for.
#[derive(Debug, Clone)]
struct Replacement {
    parts: Vec<ReplacementPart>,
}

#[derive(Debug, Clone)]
enum ReplacementPart {
    Text(String),
    Capture(String),
}

impl Replacement {
    /// Only *named* captures may appear, and only ones the pattern binds.
    ///
    /// A bare `...` is rejected rather than given a meaning: on the match
    /// side it stands for code nobody named, so on the output side it has
    /// nothing to refer to. Positional correspondence between the nth `...`
    /// of each side is how Coccinelle does it and is easy to get wrong;
    /// requiring a name makes the template total -- every hole in it has
    /// exactly one binding.
    fn compile(text: &str, bound: &std::collections::HashSet<String>) -> Result<Self, ArgError> {
        let mut parts = Vec::new();
        let mut cursor = 0usize;
        for (start, len, kind) in apex_parser::hole_spans(text) {
            let (start, len) = (start as usize, len as usize);
            let name_at = match kind {
                apex_parser::TokenKind::PatternCapture => 1,
                apex_parser::TokenKind::PatternSeqCapture => 4,
                _ => 0,
            };
            if name_at == 0 {
                return Err(ArgError(
                    format!(
                        "error: `...` cannot appear in a replacement\nnote: an unnamed hole has nothing to refer to; name it in the pattern and use the name here"
                    ),
                    2,
                ));
            }
            let name = text[start + name_at..start + len].to_string();
            if !bound.contains(&name) {
                return Err(ArgError(
                    format!("error: replacement uses ${name}, which the pattern never binds"),
                    2,
                ));
            }
            if start > cursor {
                parts.push(ReplacementPart::Text(text[cursor..start].to_string()));
            }
            parts.push(ReplacementPart::Capture(name));
            cursor = start + len;
        }
        if cursor < text.len() {
            parts.push(ReplacementPart::Text(text[cursor..].to_string()));
        }
        Ok(Replacement { parts })
    }

    fn render(&self, binds: &Binds) -> String {
        self.parts
            .iter()
            .map(|part| match part {
                ReplacementPart::Text(t) => t.as_str(),
                ReplacementPart::Capture(name) => binds.get(name).map_or("", String::as_str),
            })
            .collect()
    }

    fn is_deletion(&self) -> bool {
        self.parts.is_empty()
    }
}

/// One rewrite: what to replace, and with what.
#[derive(Debug)]
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// A compiled pattern.
///
/// The tree form is held as a *green* node, not the red `SyntaxNode` the
/// parser handed back. A red node is a thread-local cursor into the tree
/// and is deliberately neither `Send` nor `Sync`, so a pattern holding one
/// could not cross onto rayon's workers; a `GreenNode` is the `Arc`-based
/// shared half and cloning it is one refcount bump.
#[derive(Debug, Clone)]
enum Pattern {
    /// The ordinary case: code with holes, compiled to the tree it denotes.
    Tree(apex_syntax::GreenNode),
    /// `kind:Name` -- any node of that syntax kind.
    ///
    /// The escape hatch for shapes a pattern literal cannot spell, and the
    /// price is exactly what the survey predicted: it costs the user
    /// grammar node names, which is the thing pattern literals exist to
    /// avoid. It earns its place because some questions have no literal
    /// form at all -- "a query with no WHERE clause" needs to name the
    /// clause, and a `WHERE` hole cannot be written, since a SOQL value
    /// must be a literal or a bind rather than an identifier.
    Kind(SyntaxKind),
    /// `regex:RE` -- any node whose significant text matches, anchored at
    /// both ends so a pattern cannot accidentally match a whole file.
    ///
    /// The other half of the escape hatch, for questions about *text* that
    /// structure cannot answer: a hardcoded Salesforce Id is a string
    /// literal of a particular length and alphabet, and no amount of tree
    /// shape distinguishes it from any other string.
    Regex(regex::Regex),
}

pub fn run(
    pattern_src: &str,
    replacement_src: Option<&str>,
    not_srcs: &[String],
    containing_srcs: &[String],
    paths: &[PathBuf],
) -> ExitCode {
    let pattern = match Pattern::compile(pattern_src) {
        Ok(p) => p,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };
    let mut excluded = Vec::with_capacity(not_srcs.len());
    for src in not_srcs {
        match Pattern::compile(src) {
            Ok(p) => excluded.push(p),
            Err(ArgError(message, code)) => {
                eprintln!("{message}");
                return ExitCode::from(code);
            }
        }
    }
    let mut required = Vec::with_capacity(containing_srcs.len());
    for src in containing_srcs {
        match Pattern::compile(src) {
            Ok(p) => required.push(p),
            Err(ArgError(message, code)) => {
                eprintln!("{message}");
                return ExitCode::from(code);
            }
        }
    }

    let replacement = match replacement_src {
        Some(text) => match Replacement::compile(text, &pattern.capture_names()) {
            Ok(r) => Some(r),
            Err(ArgError(message, code)) => {
                eprintln!("{message}");
                return ExitCode::from(code);
            }
        },
        None => None,
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let refused = std::sync::atomic::AtomicBool::new(false);
    let found = match &replacement {
        Some(replacement) => run_replace(
            &pattern,
            replacement,
            &excluded,
            &required,
            paths,
            &cwd,
            &refused,
        ),
        None => walk_project(paths, &cwd, |path, src| {
            matches_in_file_filtered(&pattern, &excluded, &required, path, src)
        }),
    };
    let matches = match found {
        Ok(matches) => matches,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    for m in &matches {
        println!("{m}");
    }

    // A refused file is an error even though the others were rewritten:
    // a script that checks the exit code must not read "some files were
    // skipped" as success.
    if refused.load(std::sync::atomic::Ordering::Relaxed) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

impl Pattern {
    fn compile(pattern_src: &str) -> Result<Self, ArgError> {
        if let Some(name) = pattern_src.strip_prefix("kind:") {
            let name = name.trim();
            return match SyntaxKind::from_name(name) {
                Some(kind) => Ok(Pattern::Kind(kind)),
                None => Err(ArgError(
                    format!("error: unknown syntax kind: {name}\nnote: kinds are named as the grammar names them, e.g. SoqlWhereClause, MethodDecl, ForEachStmt\nnote: `apexls ast <file>` prints the kinds of a real file"),
                    2,
                )),
            };
        }
        if let Some(re) = pattern_src.strip_prefix("regex:") {
            // Anchored: an unanchored pattern would match the whole file as
            // readily as the token meant, since every ancestor's text
            // contains its descendants'.
            return regex::Regex::new(&format!("^(?:{re})$"))
                .map(Pattern::Regex)
                .map_err(|e| ArgError(format!("error: invalid regex: {e}"), 2));
        }

        // gogrep's multi-entry-point design: the text alone does not say
        // which fragment of the grammar a pattern is, so try each and take
        // the first that consumes the whole thing. Holes need no handling
        // here at all -- `parse_pattern` hands them to the real grammar as
        // `PatternHole` tokens, which it accepts wherever it requires a
        // construct.
        for fragment in [
            Fragment::Expression,
            Fragment::Statement,
            Fragment::Block,
            Fragment::CatchClause,
            Fragment::ClassMember,
        ] {
            let parse = apex_parser::parse_pattern(pattern_src, fragment);
            if !parse.errors.is_empty() {
                continue;
            }
            // Not a `text_range()` coverage check, which looks equivalent
            // and is dead code: the root marker spans the whole input
            // whether the grammar consumed it or not. Unconsumed input
            // shows up as *extra children beside* the real node.
            let root = parse.syntax();
            if let [NodeOrToken::Node(node)] = significant_children(&root).as_slice() {
                return Ok(Pattern::Tree(node.green().to_owned()));
            }
        }

        Err(ArgError(
            format!(
                "error: could not parse pattern as Apex: {pattern_src}\n\
                 note: a pattern must be one complete expression, statement, block, catch clause or class member\n\
                 note: a method needs its body -- `void $m(...) {{ ... }}` -- or a `;` if it has none"
            ),
            2,
        ))
    }

    /// Every match of this pattern in `root`, innermost and outermost both
    /// -- a nested match is a real match and hiding it would be worse than
    /// printing two lines.
    fn matches_in(&self, root: &SyntaxNode) -> Vec<SyntaxNode> {
        self.matches_with_binds(root)
            .into_iter()
            .map(|(node, _)| node)
            .collect()
    }

    /// As [`Pattern::matches_in`], but keeping each match's bindings, which
    /// is what a replacement template is filled from.
    fn matches_with_binds(&self, root: &SyntaxNode) -> Vec<(SyntaxNode, Binds)> {
        match self {
            // A fresh red-tree cursor per call, since a red node is a
            // thread-local cursor and cannot be shared across workers.
            Pattern::Tree(green) => {
                let pattern = SyntaxNode::new_root(green.clone());
                root.descendants()
                    .filter_map(|candidate| {
                        let mut binds = HashMap::new();
                        match_node(&pattern, &candidate, &mut binds).then_some((candidate, binds))
                    })
                    .collect()
            }
            Pattern::Kind(kind) => root
                .descendants()
                .filter(|candidate| candidate.kind() == *kind)
                .map(|c| (c, Binds::new()))
                .collect(),
            Pattern::Regex(re) => root
                .descendants()
                .filter(|candidate| re.is_match(&significant_text(candidate)))
                .map(|c| (c, Binds::new()))
                .collect(),
        }
    }

    /// Every capture name this pattern binds -- what a replacement is
    /// allowed to refer to.
    fn capture_names(&self) -> std::collections::HashSet<String> {
        let Pattern::Tree(green) = self else {
            return std::collections::HashSet::new();
        };
        SyntaxNode::new_root(green.clone())
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::PatternCapture | SyntaxKind::PatternSeqCapture
                )
            })
            .filter_map(|t| {
                let name = if t.kind() == SyntaxKind::PatternSeqCapture {
                    t.text().get(4..)?.to_string()
                } else {
                    t.text().strip_prefix(HOLE_CAPTURE_SIGIL)?.to_string()
                };
                (name != "_").then_some(name)
            })
            .collect()
    }
}

fn hole_of(element: &SyntaxElement) -> Option<Hole> {
    // The element has to be *nothing but* the hole: one significant token,
    // plus an optional `;` where the grammar wrapped a hole into a
    // statement. Asking about tokens rather than text is what keeps a
    // composite from being swallowed -- `$A + $B` and `... + ...` each hold
    // three tokens, so neither reads as a single hole.
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
    let [only] = tokens.as_slice() else {
        return None;
    };
    match only.kind() {
        SyntaxKind::PatternHole => return Some(Hole::Ellipsis),
        SyntaxKind::PatternSeqCapture => {
            // `$...NAME` -- strip the sigil and the three dots.
            let name = only.text().get(4..).unwrap_or_default().to_string();
            return Some(Hole::SeqCapture(name));
        }
        SyntaxKind::PatternCapture => {
            let name = only
                .text()
                .strip_prefix(HOLE_CAPTURE_SIGIL)
                .unwrap_or(only.text());
            return Some(if name == "_" {
                Hole::Anonymous
            } else {
                Hole::Capture(name.to_string())
            });
        }
        _ => {}
    }
    // `'...'` -- a string literal whose whole content is an ellipsis. Never
    // folded into a hole token, because the lexer sees one string literal
    // and its insides are data, not structure.
    if is_string_literal_kind(only.kind()) {
        let content = literal_content(only.text()).unwrap_or(only.text());
        return (content == "...").then_some(Hole::AnyString);
    }
    None
}

/// The text between a string literal's quotes, if it is properly closed.
fn literal_content(literal: &str) -> Option<&str> {
    literal
        .strip_prefix(STRING_QUOTE)
        .and_then(|rest| rest.strip_suffix(STRING_QUOTE))
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
    if !kinds_compatible(pat, src) {
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

/// Do these two nodes name the same construct?
///
/// Identical kinds, plus one equivalence: Apex's two loop productions,
/// `ForEachStmt` and `ForStmt`, are the same construct to a pattern that
/// left the header a hole. `for (...)` is written once and means "any
/// loop", so a pattern compiled to one form must still match the other --
/// otherwise every C-style loop is invisible to every loop query, which for
/// a search tool is worse than failing outright.
fn kinds_compatible(pat: &SyntaxNode, src: &SyntaxNode) -> bool {
    if pat.kind() == src.kind() {
        return true;
    }
    let both_loops = matches!(pat.kind(), SyntaxKind::ForEachStmt | SyntaxKind::ForStmt)
        && matches!(src.kind(), SyntaxKind::ForEachStmt | SyntaxKind::ForStmt);
    both_loops
        && significant_children(pat)
            .iter()
            .any(|c| hole_of(c) == Some(Hole::Ellipsis))
}

fn match_element(pat: &SyntaxElement, src: &SyntaxElement, binds: &mut Binds) -> bool {
    match hole_of(pat) {
        Some(Hole::Ellipsis) | Some(Hole::Anonymous) => return true,
        // A sequence capture reached here is standing where a single
        // element is expected rather than in a run, so it binds that one
        // element. Consistent either way: it binds whatever it consumed.
        Some(Hole::SeqCapture(name)) => {
            let text = match src {
                NodeOrToken::Token(t) => t.text().to_string(),
                NodeOrToken::Node(n) => significant_text(n),
            };
            binds.insert(name, text);
            return true;
        }
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

    // An ellipsis consumes zero or more elements; a *named* sequence does
    // the same and records what it swallowed, so a rewrite can put it back.
    let sequence = match hole_of(head) {
        Some(Hole::Ellipsis) => Some(None),
        Some(Hole::SeqCapture(name)) => Some(Some(name)),
        _ => None,
    };
    if let Some(name) = sequence {
        let rest = &pat[1..];
        for split in 0..=src.len() {
            let mut attempt = binds.clone();
            if let Some(name) = &name {
                attempt.insert(name.clone(), run_text(&src[..split]));
            }
            if rest.is_empty() {
                if split < src.len() {
                    // A trailing sequence swallows everything that is left,
                    // so only the full-length split is the real binding.
                    continue;
                }
                *binds = attempt;
                return true;
            }
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

/// The source text spanned by a consumed run of elements, first
/// significant byte to last. Empty when the run is.
fn run_text(run: &[SyntaxElement]) -> String {
    let significant = |e: &SyntaxElement| match e {
        NodeOrToken::Token(t) => Some(t.text_range()),
        NodeOrToken::Node(n) => apex_syntax::significant_range(n),
    };
    let Some(first) = run.iter().find_map(significant) else {
        return String::new();
    };
    let last = run.iter().rev().find_map(significant).unwrap_or(first);
    let root = match &run[0] {
        NodeOrToken::Token(t) => t.parent().map(|p| p.ancestors().last().unwrap_or(p)),
        NodeOrToken::Node(n) => Some(n.ancestors().last().unwrap_or_else(|| n.clone())),
    };
    let Some(root) = root else {
        return String::new();
    };
    let base = usize::from(root.text_range().start());
    let text = root.text().to_string();
    let (from, to) = (
        usize::from(first.start()) - base,
        usize::from(last.end()) - base,
    );
    text.get(from..to).unwrap_or_default().to_string()
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

/// Like [`matches_in_file`], but dropping any match that itself contains a
/// match of one of `excluded`, and keeping only those containing a match of
/// every one of `required`.
///
/// "Contains" includes the match node itself, not only its descendants,
/// which is what lets a negation narrow a pattern rather than only exclude
/// what is nested inside it: `[SELECT ... FROM $O]` minus
/// `[SELECT ... FROM $O WHERE ...]` is "a query with no WHERE clause", and
/// the two patterns describe the very same node.
fn matches_in_file_filtered(
    pattern: &Pattern,
    excluded: &[Pattern],
    required: &[Pattern],
    display_path: &Path,
    src: &str,
) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);
    let root = parse.syntax();

    pattern
        .matches_in(&root)
        .into_iter()
        .filter(|node| {
            !excluded
                .iter()
                .any(|unwanted| !unwanted.matches_in(node).is_empty())
                && required
                    .iter()
                    .all(|wanted| !wanted.matches_in(node).is_empty())
        })
        .filter_map(|node| site_for(display_path, src, &index, &node))
        .collect()
}

/// Rewrite every match in the project, in place.
///
/// In place with no dry-run by design: a repository is under version
/// control, so `git diff` is the preview and `git checkout` the undo, and
/// making the common case take two invocations would only duplicate what
/// the VCS already does.
fn run_replace(
    pattern: &Pattern,
    replacement: &Replacement,
    excluded: &[Pattern],
    required: &[Pattern],
    paths: &[PathBuf],
    cwd: &Path,
    refused: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Site>, ArgError> {
    walk_project(paths, cwd, |display_path, src| {
        rewrite_file(
            pattern,
            replacement,
            excluded,
            required,
            display_path,
            src,
            refused,
        )
    })
}

/// Rewrite one file, returning the sites changed.
///
/// Reads the file again rather than trusting the copy the walk handed over,
/// because `walk_project` is parse-only and hands out a borrowed `&str`;
/// the write has to go through the real path either way.
fn rewrite_file(
    pattern: &Pattern,
    replacement: &Replacement,
    excluded: &[Pattern],
    required: &[Pattern],
    display_path: &Path,
    src: &str,
    refused: &std::sync::atomic::AtomicBool,
) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);
    let root = parse.syntax();

    let matches: Vec<_> = pattern
        .matches_with_binds(&root)
        .into_iter()
        .filter(|(node, _)| {
            !excluded
                .iter()
                .any(|unwanted| !unwanted.matches_in(node).is_empty())
                && required
                    .iter()
                    .all(|wanted| !wanted.matches_in(node).is_empty())
        })
        .collect();

    let mut edits = Vec::new();
    let mut sites = Vec::new();
    for (node, binds) in outermost_only(matches) {
        let Some(range) = apex_syntax::significant_range(&node) else {
            continue;
        };
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let (start, end) = if replacement.is_deletion() {
            widen_deletion_to_line(src, start, end)
        } else {
            (start, end)
        };
        let Some(mut site) = site_for(display_path, src, &index, &node) else {
            continue;
        };
        let new_text = replacement.render(&binds);
        // Report what each site *became*, not what it was: a bulk rewrite
        // is read to confirm it did the right thing, and the old text is
        // still one `git diff` away.
        site.text = if replacement.is_deletion() {
            format!("{} -> (deleted)", site.text)
        } else {
            format!("{} -> {}", site.text, crate::project::collapse(&new_text))
        };
        edits.push(Edit {
            start,
            end,
            text: new_text,
        });
        sites.push(site);
    }
    if edits.is_empty() {
        return Vec::new();
    }

    // Crossing overlaps are genuinely ambiguous -- either rewrite changes
    // text the other was computed against -- so both are skipped and named
    // rather than one being picked silently. Nested matches never reach
    // here; `outermost_only` has already resolved those.
    edits.sort_by_key(|e| e.start);
    if let Some(pair) = edits.windows(2).find(|w| w[1].start < w[0].end) {
        eprintln!("error: overlapping rewrites in {}", display_path.display());
        for e in pair {
            eprintln!("  note: {}..{} {}", e.start, e.end, &src[e.start..e.end]);
        }
        eprintln!("  note: both change the same text; file skipped");
        refused.store(true, std::sync::atomic::Ordering::Relaxed);
        return Vec::new();
    }

    // Highest offset first, so earlier edits' offsets stay valid without
    // remapping -- the idiom `apexls-server`'s own fix pipeline uses.
    let mut rewritten = src.to_string();
    for edit in edits.iter().rev() {
        rewritten.replace_range(edit.start..edit.end, &edit.text);
    }

    // The safety net the parser buys us, and which no surveyed tool has: if
    // the rewrite would not parse, it is not written. One parse per changed
    // file turns a bad replacement from silent corruption into a refusal.
    let after = parse_apex_file(display_path, &rewritten);
    if after.errors.len() > parse.errors.len() {
        eprintln!(
            "error: rewrite of {} would not parse",
            display_path.display()
        );
        if let Some(e) = after.errors.first() {
            eprintln!("  note: {} @ {}", e.message, e.offset);
        }
        eprintln!("  note: file not written");
        refused.store(true, std::sync::atomic::Ordering::Relaxed);
        return Vec::new();
    }

    if std::fs::write(display_path, &rewritten).is_err() {
        eprintln!("error: could not write {}", display_path.display());
        refused.store(true, std::sync::atomic::Ordering::Relaxed);
        return Vec::new();
    }
    sites
}

/// Drop every match contained in another.
///
/// Search reports nested matches deliberately -- a call inside two nested
/// loops really is two hits. For a rewrite they are guaranteed to overlap,
/// so refusing the pair would make every nesting pattern unrewritable.
/// Rewriting the outermost is the reading that loses nothing: the inner
/// text is part of what the outer rewrite replaces.
fn outermost_only(matches: Vec<(SyntaxNode, Binds)>) -> Vec<(SyntaxNode, Binds)> {
    let ranges: Vec<_> = matches.iter().map(|(n, _)| n.text_range()).collect();
    matches
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            !ranges.iter().enumerate().any(|(j, other)| {
                j != *i && other.contains_range(ranges[*i]) && *other != ranges[*i]
            })
        })
        .map(|(_, m)| m.clone())
        .collect()
}

/// Grow a deletion to swallow its whole line when nothing else is on it.
///
/// Splicing only the significant range would leave a blank, indented line
/// behind, which makes "delete every `System.debug(...);`" useless in
/// practice. Deliberately narrow: the line must be whitespace either side
/// of the match, so a deletion never takes code with it.
fn widen_deletion_to_line(src: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[end..].find('\n').map_or(src.len(), |i| end + i + 1);
    let before_blank = src[line_start..start].trim().is_empty();
    let after_blank = src[end..line_end].trim().is_empty();
    if before_blank && after_blank {
        (line_start, line_end)
    } else {
        (start, end)
    }
}

#[cfg(test)]
fn matches_in_file(pattern: &Pattern, display_path: &Path, src: &str) -> Vec<Site> {
    matches_in_file_filtered(pattern, &[], &[], display_path, src)
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

    /// A capture inside a generic type argument unifies like any other, so
    /// `List<$X> $v = new List<$X>();` asks for the declared and
    /// constructed element types to agree.
    #[test]
    fn captures_unify_inside_generic_type_arguments() {
        let src = wrap(
            "        Map<String, Id> a = new Map<String, Id>();\n        Map<String, Id> b = new Map<Id, String>();",
        );
        assert_eq!(
            hits("Map<$K, $V> $v = new Map<$K, $V>();", &src),
            vec!["3:9:Map<String, Id> a = new Map<String, Id>();".to_string()],
            "only the declaration whose element types agree",
        );
        assert_eq!(
            hits("Map<$K, $V> $v = new Map<$V, $K>();", &src),
            vec!["4:9:Map<String, Id> b = new Map<Id, String>();".to_string()],
            "swapped arguments pick out only the swapped declaration",
        );
    }

    /// `$_` matches one construct and binds nothing, so `f($_, $_)` reads
    /// as "two arbitrary arguments" -- the spelling every tool in the
    /// survey uses. A capture that unified would instead demand the two be
    /// identical, which is the opposite of what the underscore suggests.
    #[test]
    fn an_underscore_capture_binds_nothing() {
        let src = wrap("        f(a, b);\n        f(a, a);");
        assert_eq!(hits("f($_, $_);", &src).len(), 2, "any two arguments");
        assert_eq!(
            hits("f($x, $x);", &src),
            vec!["4:9:f(a, a);".to_string()],
            "a named capture still requires them to agree",
        );
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

    /// `kind:` names a syntax kind directly -- the escape hatch for shapes
    /// a pattern literal cannot spell. A SOQL `WHERE` hole cannot be
    /// written, because a SOQL value must be a literal or a bind rather
    /// than an identifier, so naming the clause is the only way to ask
    /// about it.
    #[test]
    fn kind_names_a_syntax_kind_directly() {
        let src = wrap("        List<Account> a = [SELECT Id FROM Account WHERE Name = 'x'];");
        assert_eq!(hits("kind:SoqlWhereClause", &src).len(), 1);
        assert_eq!(
            hits("kind:soqlwhereclause", &src).len(),
            1,
            "case-insensitive"
        );
        let err = Pattern::compile("kind:NoSuchKind").expect_err("unknown kind");
        assert!(err.0.contains("unknown syntax kind"), "{}", err.0);
    }

    /// `regex:` asks about *text*, which structure cannot answer: a
    /// hardcoded Salesforce Id is a string literal of a particular length
    /// and alphabet, indistinguishable by shape from any other string.
    /// Anchored, so a pattern cannot accidentally match a whole file.
    #[test]
    fn regex_matches_node_text_anchored() {
        let src = wrap(
            "        f('001000000000000AAA');
        f('short');",
        );
        assert_eq!(hits("regex:'[a-zA-Z0-9]{18}'", &src).len(), 1);
        assert!(
            hits("regex:001", &src).is_empty(),
            "anchored: a fragment must not match the whole literal",
        );
        assert!(Pattern::compile("regex:[unclosed").is_err());
    }

    /// `--not` and `--containing` filter a match by what it holds.
    /// "Contains" includes the match itself, which is what lets a negation
    /// narrow a pattern rather than only exclude what is nested inside it.
    #[test]
    fn not_and_containing_filter_by_what_a_match_holds() {
        let src = wrap(
            "        List<Account> a = [SELECT Id FROM Account WHERE Name = 'x'];
        List<Contact> c = [SELECT Id FROM Contact];",
        );
        let filtered = |not: &[&str], containing: &[&str]| {
            let p = Pattern::compile("kind:SoqlExpr").expect("compiles");
            let not: Vec<_> = not.iter().map(|s| Pattern::compile(s).unwrap()).collect();
            let req: Vec<_> = containing
                .iter()
                .map(|s| Pattern::compile(s).unwrap())
                .collect();
            matches_in_file_filtered(&p, &not, &req, Path::new("T.cls"), &src).len()
        };
        assert_eq!(filtered(&[], &[]), 2);
        assert_eq!(
            filtered(&["kind:SoqlWhereClause"], &[]),
            1,
            "the one without"
        );
        assert_eq!(filtered(&[], &["kind:SoqlWhereClause"]), 1, "the one with");
        assert_eq!(
            filtered(&["kind:SoqlExpr"], &[]),
            0,
            "a match contains itself"
        );
    }

    /// A replacement is filled from the match's own bindings, and the
    /// captured text is spliced back verbatim.
    #[test]
    fn a_replacement_is_filled_from_the_match() {
        let names = |p: &str| Pattern::compile(p).unwrap().capture_names();
        let r = Replacement::compile("!$X.isEmpty()", &names("$X.size() > 0")).unwrap();
        let mut binds = Binds::new();
        binds.insert("X".to_string(), "accounts".to_string());
        assert_eq!(r.render(&binds), "!accounts.isEmpty()");
    }

    /// Only *named* captures may appear, and only ones the pattern binds.
    /// A bare `...` has nothing to refer to on the output side, so it is
    /// rejected rather than given a meaning.
    #[test]
    fn a_replacement_rejects_unnamed_and_unbound_holes() {
        let names = |p: &str| Pattern::compile(p).unwrap().capture_names();
        let err = Replacement::compile("log(...);", &names("System.debug(...);"))
            .expect_err("`...` has nothing to refer to");
        assert!(
            err.0.contains("cannot appear in a replacement"),
            "{}",
            err.0
        );

        let err = Replacement::compile("g($B)", &names("f($A)")).expect_err("$B is never bound");
        assert!(err.0.contains("never binds"), "{}", err.0);

        // `$_` binds nothing, so it cannot be referred to either.
        assert!(Replacement::compile("g($_)", &names("f($_)")).is_err());
    }

    /// An empty replacement deletes, and takes the whole line when nothing
    /// else is on it -- otherwise "delete every `System.debug(...);`" would
    /// leave a blank indented line behind at every site.
    #[test]
    fn deleting_takes_the_line_only_when_it_is_otherwise_empty() {
        let src = "a;\n    f();\n  g(); h();\n";
        // `f();` is alone on its line, so the line goes.
        assert_eq!(widen_deletion_to_line(src, 7, 11), (3, 12));
        // `g();` shares its line with `h();`, so only the call goes.
        let g = src.find("g();").unwrap();
        assert_eq!(widen_deletion_to_line(src, g, g + 4), (g, g + 4));
    }

    /// Search reports nested matches on purpose, but for a rewrite they are
    /// guaranteed to overlap. Rewriting the outermost loses nothing, since
    /// the inner text is part of what the outer rewrite replaces.
    #[test]
    fn nested_matches_collapse_to_the_outermost() {
        let src = wrap("        f(f(a));");
        let pattern = Pattern::compile("f(...)").expect("compiles");
        let parse = parse_apex_file(Path::new("T.cls"), &src);
        let all = pattern.matches_with_binds(&parse.syntax());
        assert_eq!(all.len(), 2, "search sees both calls");
        assert_eq!(
            outermost_only(all).len(),
            1,
            "a rewrite takes only the enclosing one",
        );
    }

    /// `$...NAME` matches a run of any length, including none, and binds
    /// what it swallowed -- which is the only way a variable-length run
    /// survives a rewrite.
    #[test]
    fn a_sequence_capture_matches_a_run_of_any_length() {
        let src = wrap("        f();\n        f(a);\n        f(a, b, c);");
        assert_eq!(hits("f($...ARGS);", &src).len(), 3, "zero, one and many");

        let bound = |src: &str| {
            let p = Pattern::compile("f($...ARGS);").expect("compiles");
            let parse = parse_apex_file(Path::new("T.cls"), src);
            p.matches_with_binds(&parse.syntax())
                .into_iter()
                .map(|(_, b)| b.get("ARGS").cloned().unwrap_or_default())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            bound(&src),
            vec!["".to_string(), "a".to_string(), "a, b, c".to_string()],
            "the run's own source text, verbatim",
        );
    }

    /// The rewrite that a one-construct capture could not express: carry a
    /// call's arguments across, whatever their number.
    #[test]
    fn a_sequence_capture_carries_a_run_across_a_replacement() {
        let names = Pattern::compile("f($...ARGS);").unwrap().capture_names();
        let r = Replacement::compile("g($...ARGS);", &names).expect("compiles");
        let mut binds = Binds::new();
        binds.insert("ARGS".to_string(), "a, b, c".to_string());
        assert_eq!(r.render(&binds), "g(a, b, c);");

        // Still rejected if the pattern never bound it.
        assert!(Replacement::compile("g($...OTHER);", &names).is_err());
    }

    /// A method pattern must say what its body is. Forgiving the missing
    ///  produced a tree with neither a body nor a , which no
    /// real declaration has -- so  compiled
    /// happily and then matched nothing at all, anywhere. A pattern that
    /// can never match is worse than one that will not compile.
    #[test]
    fn a_method_pattern_must_say_what_its_body_is() {
        let err = Pattern::compile("public static void $m(...)")
            .expect_err("a method needs a body or a semicolon");
        assert!(err.0.contains("needs its body"), "{}", err.0);

        assert!(Pattern::compile("public static void $m(...) { ... }").is_ok());
        assert!(Pattern::compile("public static void $m(...);").is_ok());

        // The statement-terminator leniency this scoped back is still
        // there, which is what lets a block segment be written bare.
        assert!(Pattern::compile("{ ... [SELECT ... FROM $O] ... }").is_ok());
    }

    #[test]
    fn searches_trigger_files_too() {
        let src = "trigger T on Account (before insert) {\n    System.debug('a');\n}\n";
        let pattern = Pattern::compile("System.debug(...);").expect("compiles");
        let found = matches_in_file(&pattern, Path::new("T.trigger"), src);
        assert_eq!(found.len(), 1, "a trigger is not a compilation unit");
    }
}
