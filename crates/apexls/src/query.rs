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
//! **A capture can stand for an operator** -- `$L $OP $R` -- which is the
//! one hole admitted at a *choice* point rather than a requirement, and so
//! is fenced on both sides. Only a capture qualifies, never a bare `...`,
//! since an operator is exactly one token. And never straight after a bare
//! `...`: the first attempt admitted any hole there, and `{ ... $X = y; }`
//! read `$X` as an operator applied to the ellipsis, swallowing the
//! assignment after it. An ellipsis on the left means a statement run.
//!
//! **`comment:RE` searches comment text**, the one thing no other form can
//! reach: comments are trivia, filtered out of every structural comparison
//! and out of `regex:`'s significant text, so `regex:.*TODO.*` finds
//! nothing. It reports one hit per *comment*, not per line, so a block
//! comment mentioning TODO twice is one hit. Unanchored, unlike `regex:`.
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
//! **`--and COND` and `--not COND`** keep a match when every `--and` holds
//! and no `--not` does. The division of labour is the whole design: the
//! pattern says the *structure*, `~` says the *text*, and the two flags say
//! yes or no. A condition takes one of two forms:
//!
//! - **A pattern** -- the match *contains* it. "Contains" includes the
//!   match itself, not only its descendants, so a negation can narrow a
//!   pattern rather than only exclude what is nested inside it:
//!   `kind:SoqlExpr --not kind:SoqlWhereClause` is "a query with no WHERE
//!   clause", and both name the same node.
//! - **`$X ~ GLOB`** or **`$X ~ /REGEX/`** -- the capture's text matches.
//!   A glob is shell-style and anchored: `*` any run, `?` one character,
//!   `{add,remove}` alternatives, and `$V` another capture's text, so
//!   `--not '$T ~ $V'` says two captures differ. A glob is
//!   case-insensitive, because Apex names are. A regex is unanchored and
//!   means exactly what it says, as in Perl: case-sensitive unless flagged
//!   `/re/i`, with `x`, `s` and `m` also accepted. Otherwise `[A-Z]` would
//!   quietly match lowercase, and `~` would disagree with `regex:` and
//!   `comment:`, which were always case-sensitive.
//!
//! - **`$X : TYPE`** -- the capture's *type* matches, as the binder infers
//!   it: `$v : String`, `$l : List<*>`. The same glob, but over the type's
//!   text, and a `$V` inside it is V's *type*, so `--not '$a : $b'` says two
//!   expressions differ in type where `--not '$a ~ $b'` says they differ in
//!   spelling. A capture the binder could not type fails every `:` test,
//!   so `--not` keeps it. Only a `:` condition binds the project, costing
//!   a whole-project bind; every other query stays parse-only.
//!
//! The forms cannot be confused: `~` is only ever a prefix operator in
//! Apex, and no Apex construct starts `$X :`, so neither starts a pattern. There is no `--or` -- alternation
//! inside a glob or regex covers the common case, `$M ~ {add,remove}*` --
//! and no boolean operators inside a condition, which keeps each one
//! readable on its own.
//!
//! Globs live only here, never inline in a pattern. Inline, `add*` was
//! told from `a * b` by whitespace alone and `get?` read as a ternary; a
//! condition names its capture, so a matched name can also be carried into
//! a replacement.
//!
//! **`$...NAME`** captures a *run* of elements rather than one, and binds
//! its source text, which is what lets a variable-length run survive a
//! rewrite: `Database.query($...ARGS)` -> `Database.queryWithBinds($...ARGS, ...)`
//! keeps a call's arguments whatever their number.
//!
//! **`${...}` numbers a capture group**: `${[SELECT ... FROM $o ...]}` is
//! `$1`, the next `${` is `$2`, counted from the left. A group is a capture
//! with a shape -- its text is `$1` in templates and conditions -- and the
//! unit a rewrite can target: `-r '$1 => TEMPLATE'` replaces only that
//! group's span, one `-r` per group, while `-r TEMPLATE` still replaces the
//! whole match. Like a `^`, a group is found everywhere it can land, so
//! `void $_(...) { ... ${[SELECT ...]} ... }` rewrites every query in each
//! void method and keeps the methods. Numbers therefore cannot name holes.
//! `=>` rather than sed's `s/.../.../`, whose `/` Apex writes constantly;
//! only the first `=>` separates and at most one space either side of it is
//! dropped, so a template keeps its own whitespace and map literals.
//!
//! **Modifiers match as a subset**, everywhere Apex lets one be written:
//! a type or member declaration, a local variable, a method or catch
//! parameter, a property accessor. Every modifier the pattern names must be
//! present, and ones it is silent about are ignored, so writing one narrows
//! the search rather than pinning the whole list --
//! `static void addChild*() { ... }` finds
//! `@isTest static void addChildQueries_success()`, and
//! `@future static void addChild*() { ... }` finds none of them. Order does
//! not matter either, so `public static` and `static public` are one list.
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
                    "error: `...` cannot appear in a replacement\n\
                     note: an unnamed hole has nothing to refer to; name it in the pattern and use the name here"
                        .to_string(),
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

/// The usual cause of a `$NAME` that vanished before apexls saw it.
const SHELL_QUOTES_NOTE: &str =
    "note: a shell expands `$1` and `$v` inside double quotes, usually to nothing; \
     quote patterns, conditions and templates with single quotes, e.g. -r '$1 => x'";

/// What `--replace` rewrites: the whole match, or some of its `${...}`
/// groups, each with its own template.
enum Rewrite {
    Whole(Replacement),
    Groups(Vec<(usize, Replacement)>),
}

impl Rewrite {
    /// Each `-r` is either `TEMPLATE` for the whole match or `$N =>
    /// TEMPLATE` for group N. Not sed's `s/.../.../`: its delimiter is `/`,
    /// which Apex writes constantly -- division, comments, URLs in strings.
    fn compile(
        srcs: &[String],
        bound: &std::collections::HashSet<String>,
        groups: usize,
    ) -> Result<Option<Rewrite>, ArgError> {
        let err = |message: String| ArgError(message, 2);
        let mut whole = Vec::new();
        let mut by_group: Vec<(usize, Replacement)> = Vec::new();
        for src in srcs {
            match split_group_template(src) {
                Some((number, template)) => {
                    if number == 0 || number > groups {
                        return Err(err(format!(
                            "error: the pattern has no group ${number}\nnote: it has {groups}; `${{...}}` marks one, numbered from the left"
                        )));
                    }
                    if by_group.iter().any(|(n, _)| *n == number) {
                        return Err(err(format!("error: group ${number} is rewritten twice")));
                    }
                    by_group.push((number, Replacement::compile(template, bound)?));
                }
                // No template starts with `=>`, so one that does lost its
                // `$N` on the way in -- almost always a shell expanding `$1`
                // inside double quotes, to nothing.
                None if src.trim_start().starts_with("=>") => {
                    return Err(err(format!(
                        "error: --replace `{src}` starts with `=>`, so the `$N` before it is missing\n{SHELL_QUOTES_NOTE}"
                    )));
                }
                None => whole.push(Replacement::compile(src, bound)?),
            }
        }
        match (whole.len(), by_group.is_empty()) {
            (0, true) => Ok(None),
            (1, true) => Ok(whole.pop().map(Rewrite::Whole)),
            (0, false) => Ok(Some(Rewrite::Groups(by_group))),
            (_, true) => Err(err(
                "error: only one --replace can rewrite the whole match\nnote: to rewrite parts of it, mark them `${...}` and write `-r '$1 => TEMPLATE'`".to_string(),
            )),
            (_, false) => Err(err(
                "error: a whole-match --replace cannot be combined with `$N => ...`\nnote: the whole match already contains every group".to_string(),
            )),
        }
    }
}

/// `$N => TEMPLATE` -> `(N, TEMPLATE)`, or `None` for a whole-match
/// template. Only the first `=>` separates, so a template can hold a map
/// literal, and at most one space either side of it is dropped: the rest
/// of the template is kept exactly, leading spaces and newlines included.
fn split_group_template(src: &str) -> Option<(usize, &str)> {
    let rest = src.trim_start().strip_prefix(HOLE_CAPTURE_SIGIL)?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let number = rest[..digits].parse().ok()?;
    let rest = &rest[digits..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest).strip_prefix("=>")?;
    Some((number, rest.strip_prefix(' ').unwrap_or(rest)))
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
    /// `comment:RE` -- any comment whose text contains a match.
    ///
    /// Comments are trivia, filtered out of every structural comparison
    /// and out of `regex:`'s significant text, so no other form can reach
    /// them -- `regex:.*TODO.*` found nothing on NPSP because every TODO
    /// there is in a comment. Unanchored, unlike `regex:`: that one anchors
    /// so a fragment cannot match a whole file through an ancestor's text,
    /// but a comment is a single leaf with no ancestors in play, and
    /// `comment:TODO` is the spelling anyone would reach for.
    Comment(regex::Regex),
}

/// One `--and` / `--not` condition, tested against a match and its captures.
///
/// Told apart by shape: `$X ~ ...` tests a capture's *text*, and anything
/// else is a pattern the match must *contain*. `~` is only ever a prefix
/// operator in Apex, so `$X ~` never starts a pattern and the two cannot
/// be confused.
enum Condition {
    Contains(Pattern),
    /// `$X ~ ...` when `of_type` is false, `$X : ...` when it is true: the
    /// same test, over the capture's text or over its inferred type.
    Text {
        name: String,
        test: TextTest,
        of_type: bool,
    },
}

/// How a capture's text is tested.
enum TextTest {
    /// Compiled once: a `/regex/`, or a glob that names no other capture.
    Regex(regex::Regex),
    /// A glob naming other captures (`$T ~ $V`). Their text is only known
    /// per match, so it is regex source spliced and compiled per match --
    /// only for matches that survived the pattern, so rarely.
    Template(Vec<GlobPart>),
}

enum GlobPart {
    Regex(String),
    Capture(String),
}

impl Condition {
    fn compile(src: &str, bound: &std::collections::HashSet<String>) -> Result<Self, ArgError> {
        // Likewise a condition starting `~` or `:` lost its `$NAME`.
        if matches!(src.trim_start().chars().next(), Some('~' | ':')) {
            return Err(ArgError(
                format!("error: condition `{src}` has no capture before its operator\n{SHELL_QUOTES_NOTE}"),
                2,
            ));
        }
        let Some((name, of_type, rhs)) = split_text_condition(src) else {
            return Pattern::compile(src).map(Condition::Contains);
        };
        let require_bound = |name: &str| {
            if bound.contains(name) {
                Ok(())
            } else {
                Err(ArgError(
                    format!(
                        "error: `${name}` in `{src}` is not a capture of the pattern\n\
                         note: a condition can only test a name the pattern binds, and `$_` binds nothing"
                    ),
                    2,
                ))
            }
        };
        require_bound(&name)?;
        let rhs = rhs.trim();
        let test = match rhs.strip_prefix('/') {
            Some(body) => TextTest::Regex(flagged_regex(body, src)?),
            None => {
                let parts = glob_parts(rhs)?;
                let mut refers = false;
                for part in &parts {
                    if let GlobPart::Capture(other) = part {
                        require_bound(other)?;
                        refers = true;
                    }
                }
                if refers {
                    TextTest::Template(parts)
                } else {
                    let re = anchored(&parts, |_| None).unwrap_or_default();
                    TextTest::Regex(case_insensitive(&re)?)
                }
            }
        };
        Ok(Condition::Text {
            name,
            test,
            of_type,
        })
    }

    fn needs_types(&self) -> bool {
        matches!(self, Condition::Text { of_type: true, .. })
    }

    fn holds(&self, node: &SyntaxNode, binds: &Binds, types: Option<&Types<'_>>) -> bool {
        match self {
            Condition::Contains(pattern) => !pattern.matches_in(node).is_empty(),
            Condition::Text {
                name,
                test,
                of_type,
            } => {
                // What a capture is tested on: its text, or its type --
                // `None` for a type the binder could not infer, which
                // fails the test either way.
                let subject = |name: &str| -> Option<String> {
                    if *of_type {
                        types?.of_capture(name, binds)
                    } else {
                        Some(binds.get(name).cloned().unwrap_or_default())
                    }
                };
                let Some(value) = binds.contains_key(name).then(|| subject(name)).flatten() else {
                    return false;
                };
                match test {
                    TextTest::Regex(re) => re.is_match(&value),
                    TextTest::Template(parts) => anchored(parts, subject)
                        .and_then(|re| case_insensitive(&re).ok())
                        .is_some_and(|re| re.is_match(&value)),
                }
            }
        }
    }
}

/// Every `--and` holds and no `--not` does.
fn passes(
    node: &SyntaxNode,
    binds: &Binds,
    ands: &[Condition],
    nots: &[Condition],
    types: Option<&Types<'_>>,
) -> bool {
    ands.iter().all(|c| c.holds(node, binds, types))
        && !nots.iter().any(|c| c.holds(node, binds, types))
}

/// `$X ~ rest` -> `("X", false, " rest")` and `$X : rest` -> `("X", true,
/// " rest")`, or `None` for a pattern condition. `$...X` names a sequence
/// capture, whose bound text is its whole run.
fn split_text_condition(src: &str) -> Option<(String, bool, &str)> {
    let rest = src.trim_start().strip_prefix(HOLE_CAPTURE_SIGIL)?;
    let rest = rest.strip_prefix("...").unwrap_or(rest);
    let (name, after) = rest.split_at(name_len(rest));
    let after = after.trim_start();
    let (of_type, rhs) = match after.strip_prefix('~') {
        Some(rhs) => (false, rhs),
        None => (true, after.strip_prefix(':')?),
    };
    (!name.is_empty()).then(|| (name.to_string(), of_type, rhs))
}

/// Set once a `:` condition is compiled: captures then also record where
/// they matched, so their inferred type can be looked up. Off otherwise,
/// so no other query pays for positions it never reads.
static CAPTURE_RANGES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Where a capture matched, kept in the bindings under `@NAME` so it
/// backtracks with them -- no capture can be named `@NAME`. `T` marks a
/// captured type reference, whose type is its own text.
fn record_capture_range(binds: &mut Binds, name: &str, src: &SyntaxElement) {
    let range = src.text_range();
    let is_type = src.kind() == SyntaxKind::Type;
    binds.insert(
        format!("@{name}"),
        format!(
            "{}:{}{}",
            u32::from(range.start()),
            u32::from(range.end()),
            if is_type { ":T" } else { "" }
        ),
    );
}

/// What `:` conditions read: the bound project, and where each searched
/// file sits in it.
struct TypeContext {
    program: apex_binder::BoundProgram,
    file_ids: HashMap<PathBuf, apex_binder::FileId>,
}

impl TypeContext {
    fn new(root: &Path, cwd: &Path) -> Self {
        let program = apex_binder::BoundProgram::from_files(root);
        let file_ids = program
            .files()
            .map(|file| {
                let path = program.file_path(file);
                (path.strip_prefix(cwd).unwrap_or(path).to_path_buf(), file)
            })
            .collect();
        TypeContext { program, file_ids }
    }

    fn for_file(&self, display_path: &Path) -> Option<Types<'_>> {
        Some(Types {
            program: &self.program,
            file: *self.file_ids.get(display_path)?,
            bodies: std::cell::RefCell::default(),
        })
    }
}

/// One file's type lookups, re-binding each declaration at most once.
struct Types<'a> {
    program: &'a apex_binder::BoundProgram,
    file: apex_binder::FileId,
    #[allow(clippy::type_complexity)]
    bodies: std::cell::RefCell<
        HashMap<apex_binder::SymbolId, Vec<(apex_syntax::TextRange, SyntaxKind, String)>>,
    >,
}

impl Types<'_> {
    /// The capture's inferred type, if the binder has one. A captured type
    /// reference (`$T` in `List<$T>`) is its own text: it names a type
    /// rather than having one.
    fn of_capture(&self, name: &str, binds: &Binds) -> Option<String> {
        let at = binds.get(&format!("@{name}"))?;
        let mut fields = at.split(':');
        let start: u32 = fields.next()?.parse().ok()?;
        let end: u32 = fields.next()?.parse().ok()?;
        if fields.next() == Some("T") {
            return binds.get(name).cloned();
        }
        let range = apex_syntax::TextRange::new(start.into(), end.into());
        let unit = self.program.type_unit_at(self.file, range.start())?;
        let mut bodies = self.bodies.borrow_mut();
        let types = bodies
            .entry(unit)
            .or_insert_with(|| self.program.expr_types_in(unit));
        types
            .iter()
            .find(|(r, _, _)| *r == range)
            .map(|(_, _, ty)| ty.clone())
    }
}

fn name_len(s: &str) -> usize {
    s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len())
}

/// A shell-style glob as regex source: `*` any run, `?` one character,
/// `{a,b}` alternatives, `$V` another capture's text, anything else
/// literal.
fn glob_parts(glob: &str) -> Result<Vec<GlobPart>, ArgError> {
    let mut parts = Vec::new();
    let mut re = String::new();
    let mut depth = 0usize;
    let mut chars = glob.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            ',' if depth > 0 => re.push('|'),
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            '$' if name_len(&glob[i + 1..]) > 0 => {
                let len = name_len(&glob[i + 1..]);
                parts.push(GlobPart::Regex(std::mem::take(&mut re)));
                parts.push(GlobPart::Capture(glob[i + 1..i + 1 + len].to_string()));
                chars.nth(len - 1);
            }
            c => re.push_str(&regex::escape(c.encode_utf8(&mut [0; 4]))),
        }
    }
    if depth > 0 {
        return Err(ArgError(
            format!("error: unclosed `{{` in glob `{glob}`"),
            2,
        ));
    }
    parts.push(GlobPart::Regex(re));
    Ok(parts)
}

/// A glob's parts as one anchored regex, each other capture spliced in as
/// a literal from `lookup` -- its text, or its type. `None` if a capture it
/// names has nothing to splice (an untyped expression).
fn anchored(parts: &[GlobPart], lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut re = String::from("^(?:");
    for part in parts {
        match part {
            GlobPart::Regex(s) => re.push_str(s),
            GlobPart::Capture(name) => re.push_str(&regex::escape(&lookup(name)?)),
        }
    }
    re.push_str(")$");
    Some(re)
}

/// `re/flags` (the part after the opening `/`) as a regex. Flags follow
/// the closing `/`, Perl-style: `i` case-insensitive, `x` whitespace and
/// `#` comments ignored, `s` `.` matches a newline, `m` `^`/`$` per line.
fn flagged_regex(body: &str, src: &str) -> Result<regex::Regex, ArgError> {
    let Some((re, flags)) = body.rsplit_once('/') else {
        return Err(ArgError(
            format!("error: unclosed regex in `{src}`\nnote: write it `/REGEX/`, flags after the closing slash"),
            2,
        ));
    };
    let mut builder = regex::RegexBuilder::new(re);
    for flag in flags.chars() {
        match flag {
            'i' => builder.case_insensitive(true),
            'x' => builder.ignore_whitespace(true),
            's' => builder.dot_matches_new_line(true),
            'm' => builder.multi_line(true),
            _ => {
                return Err(ArgError(
                    format!("error: unknown regex flag `{flag}` in `{src}`\nnote: the flags are i, x, s and m"),
                    2,
                ))
            }
        };
    }
    builder
        .build()
        .map_err(|e| ArgError(format!("error: invalid regex: {e}"), 2))
}

fn case_insensitive(re: &str) -> Result<regex::Regex, ArgError> {
    regex::RegexBuilder::new(re)
        .case_insensitive(true)
        .build()
        .map_err(|e| ArgError(format!("error: invalid regex: {e}"), 2))
}

pub fn run(
    pattern_src: &str,
    replacement_srcs: &[String],
    and_srcs: &[String],
    not_srcs: &[String],
    paths: &[PathBuf],
) -> ExitCode {
    let pattern = match Pattern::compile(pattern_src) {
        Ok(p) => p,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };
    let bound = pattern.capture_names();
    let compile_all = |srcs: &[String]| {
        srcs.iter()
            .map(|src| Condition::compile(src, &bound))
            .collect::<Result<Vec<_>, _>>()
    };
    let (ands, nots) = match (compile_all(and_srcs), compile_all(not_srcs)) {
        (Ok(ands), Ok(nots)) => (ands, nots),
        (Err(ArgError(message, code)), _) | (_, Err(ArgError(message, code))) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    let replacement = match Rewrite::compile(replacement_srcs, &bound, pattern.group_count()) {
        Ok(r) => r,
        Err(ArgError(message, code)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Only a `:` condition binds the project: a whole-project bind costs
    // several times what a search does, and nothing else needs it.
    let types = ands
        .iter()
        .chain(&nots)
        .any(Condition::needs_types)
        .then(|| {
            CAPTURE_RANGES.store(true, std::sync::atomic::Ordering::Relaxed);
            TypeContext::new(&crate::project::find_project_root(&cwd), &cwd)
        });
    let types = types.as_ref();
    let refused = std::sync::atomic::AtomicBool::new(false);
    let found = match &replacement {
        Some(replacement) => run_replace(
            &pattern,
            replacement,
            &ands,
            &nots,
            types,
            paths,
            &cwd,
            &refused,
        ),
        None => walk_project(paths, &cwd, |path, src| {
            matches_in_file_filtered(&pattern, &ands, &nots, types, path, src)
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
        outln!("{m}");
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
        if let Some(re) = pattern_src.strip_prefix("comment:") {
            return regex::Regex::new(re)
                .map(Pattern::Comment)
                .map_err(|e| ArgError(format!("error: invalid regex: {e}"), 2));
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
            Fragment::TriggerUnit,
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
                let focuses = node
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::PatternFocus)
                    .count();
                if focuses > 1 {
                    return Err(ArgError(
                        "error: a pattern can have only one `^`\nnote: `^` marks where each match is reported".to_string(),
                        2,
                    ));
                }
                if let Some(numbered) = node
                    .descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| {
                        matches!(
                            t.kind(),
                            SyntaxKind::PatternCapture | SyntaxKind::PatternSeqCapture
                        )
                    })
                    .find(|t| {
                        let name = t.text().trim_start_matches('$').trim_start_matches("...");
                        !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
                    })
                {
                    return Err(ArgError(
                        format!(
                            "error: `{}` cannot name a hole: `$1`, `$2`, ... are the pattern's `${{...}}` groups\nnote: give the hole a name, or wrap the construct in `${{...}}` to number it",
                            numbered.text()
                        ),
                        2,
                    ));
                }
                if node.kind() == SyntaxKind::PatternGroup {
                    return Err(ArgError(
                        "error: a group around the whole pattern is just the whole match\nnote: `-r TEMPLATE` replaces the whole match; `${...}` marks a part of it".to_string(),
                        2,
                    ));
                }
                // A `^` on the whole pattern is the default report position.
                let node = match node.kind() {
                    SyntaxKind::PatternFocus => match focus_inner(node) {
                        Some(NodeOrToken::Node(inner)) => inner,
                        _ => node.clone(),
                    },
                    _ => node.clone(),
                };
                return Ok(Pattern::Tree(node.green().to_owned()));
            }
        }

        // `@{` is the one near miss worth naming: it looks like a group and
        // is what the group syntax was first sketched as.
        let group_hint = if pattern_src.contains("@{") {
            "\nnote: a capture group is written `${...}`, not `@{...}`"
        } else {
            ""
        };
        Err(ArgError(
            format!(
                "error: could not parse pattern as Apex: {pattern_src}\n\
                 note: a pattern must be one complete expression, statement, block, catch clause or class member\n\
                 note: a method needs its body -- `void $m(...) {{ ... }}` -- or a `;` if it has none{group_hint}"
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
            // Comments are tokens rather than nodes, so they are collected
            // directly by `matches_in_file_filtered`; as an `--and`/`--not`
            // condition a comment pattern asks "does this match contain
            // such a comment", answered by the enclosing node.
            Pattern::Comment(re) => root
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| is_comment_kind(t.kind()) && re.is_match(t.text()))
                .filter_map(|t| t.parent())
                .map(|n| (n, Binds::new()))
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
            .chain((1..=self.group_count()).map(|n| n.to_string()))
            .collect()
    }

    /// How many `${...}` groups the pattern has; they are `$1` to `$N`.
    fn group_count(&self) -> usize {
        let Pattern::Tree(green) = self else {
            return 0;
        };
        SyntaxNode::new_root(green.clone())
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::PatternGroup)
            .count()
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

/// Where a `^` focus landed, kept in the bindings so it backtracks with
/// them. Stored as a byte offset; no capture can be named `^`.
const FOCUS_KEY: &str = "^";

/// Where group `$1` landed, when the pattern has no `^`: what tells one
/// reading of a match from the next, so every place the group can land is
/// found (see [`focus_solutions`]), and rewritten.
const ANCHOR_KEY: &str = "^1";

/// The construct a `${...}` group holds -- `None` unless exactly one.
fn group_inner(group: &SyntaxNode) -> Option<SyntaxElement> {
    match significant_children(group).as_slice() {
        [open, inner, close]
            if open.kind() == SyntaxKind::PatternGroupOpen
                && close.kind() == SyntaxKind::RBrace =>
        {
            Some(inner.clone())
        }
        _ => None,
    }
}

/// A group's number, counting `${` from the start of the pattern, and
/// whether it is the one that tells readings apart (`$1`, with no `^`).
fn group_number(group: &SyntaxNode) -> (usize, bool) {
    let root = group.ancestors().last().unwrap_or_else(|| group.clone());
    let number = 1 + root
        .descendants()
        .take_while(|n| n != group)
        .filter(|n| n.kind() == SyntaxKind::PatternGroup)
        .count();
    let anchor = number == 1
        && !root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::PatternFocus);
    (number, anchor)
}

/// Record what a group matched: its text as the capture `$N`, where it is
/// for a `:` test, and its exact span for a rewrite. Refuses, as a `^`
/// does, a position already reported when this group tells readings apart.
fn record_group(binds: &mut Binds, group: &SyntaxNode, src: &SyntaxElement) -> bool {
    let (number, anchor) = group_number(group);
    if anchor {
        if focus_seen(src) {
            return false;
        }
        if let Some(offset) = focus_offset(src) {
            binds.insert(ANCHOR_KEY.to_string(), offset.to_string());
        }
    }
    let (text, span) = match src {
        NodeOrToken::Token(t) => (t.text().to_string(), Some(t.text_range())),
        NodeOrToken::Node(n) => (significant_text(n), apex_syntax::significant_range(n)),
    };
    let name = number.to_string();
    binds.insert(name.clone(), text);
    record_capture_range(binds, &name, src);
    if let Some(span) = span {
        binds.insert(
            format!("#{number}"),
            format!("{}:{}", u32::from(span.start()), u32::from(span.end())),
        );
    }
    true
}

/// Where group `$N` matched in the source, as a byte span.
fn group_span(binds: &Binds, number: usize) -> Option<(usize, usize)> {
    let (start, end) = binds.get(&format!("#{number}"))?.split_once(':')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// The construct a `^` or `${...}` marks, and how to record a match of it.
fn marker_inner(node: &SyntaxNode) -> Option<SyntaxElement> {
    match node.kind() {
        SyntaxKind::PatternFocus => focus_inner(node),
        SyntaxKind::PatternGroup => group_inner(node),
        _ => None,
    }
}

fn record_marker(marker: &SyntaxNode, binds: &mut Binds, src: &SyntaxElement) -> bool {
    match marker.kind() {
        SyntaxKind::PatternFocus => record_focus(binds, src),
        SyntaxKind::PatternGroup => record_group(binds, marker, src),
        _ => true,
    }
}

/// Whether this marker is the one that tells readings apart, so a position
/// already reported can be refused before it is even compared.
fn marker_tells_readings_apart(marker: &SyntaxNode) -> bool {
    match marker.kind() {
        SyntaxKind::PatternFocus => true,
        SyntaxKind::PatternGroup => group_number(marker).1,
        _ => false,
    }
}

/// The construct a `^` marks -- `None` unless it is exactly one.
fn focus_inner(focus: &SyntaxNode) -> Option<SyntaxElement> {
    match significant_children(focus).as_slice() {
        [caret, inner] if caret.kind() == SyntaxKind::Caret => Some(inner.clone()),
        _ => None,
    }
}

thread_local! {
    /// Positions already reported for the match [`focus_solutions`] is
    /// re-running, so a reading that would put the `^` on one of them is
    /// rejected and the search backtracks to the next. Kept out of the
    /// bindings, which are cloned for every candidate tried, and empty
    /// except during a re-run. Per thread, and each file is matched on one.
    static FOCUS_SEEN: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn focus_offset(src: &SyntaxElement) -> Option<u32> {
    let start = match src {
        NodeOrToken::Token(t) => Some(t.text_range().start()),
        NodeOrToken::Node(n) => apex_syntax::significant_range(n).map(|r| r.start()),
    };
    start.map(u32::from)
}

fn focus_seen(src: &SyntaxElement) -> bool {
    FOCUS_SEEN.with(|seen| {
        let seen = seen.borrow();
        !seen.is_empty() && focus_offset(src).is_some_and(|o| seen.contains(&o))
    })
}

/// Record where the `^` landed, or refuse a position already reported.
fn record_focus(binds: &mut Binds, src: &SyntaxElement) -> bool {
    let Some(offset) = focus_offset(src) else {
        return true;
    };
    if focus_seen(src) {
        return false;
    }
    binds.insert(FOCUS_KEY.to_string(), offset.to_string());
    true
}

/// Every reading of `node` that puts the `^` somewhere new, starting from
/// the first one found.
///
/// A `^` says what is being looked for, so each place it can land is a
/// hit of its own: a method holding two queries is two hits for
/// `void $_(...) { ... ^[SELECT ...] ... }`. Found by re-matching with the
/// positions so far ruled out, which backtracks to the next -- one extra
/// match of an already-matched node per position, and nothing at all for a
/// pattern without a `^`. Each reading keeps its own captures, so
/// conditions are checked per position rather than for the first only.
fn focus_solutions(pattern: &SyntaxNode, node: &SyntaxNode, first: Binds) -> Vec<Binds> {
    let mut solutions = vec![first];
    while let Some(offset) = solutions
        .last()
        .and_then(|b| b.get(FOCUS_KEY).or_else(|| b.get(ANCHOR_KEY)))
        .and_then(|o| o.parse().ok())
    {
        FOCUS_SEEN.with(|seen| seen.borrow_mut().push(offset));
        let mut next = Binds::new();
        if !match_node(pattern, node, &mut next) {
            break;
        }
        solutions.push(next);
    }
    FOCUS_SEEN.with(|seen| seen.borrow_mut().clear());
    solutions
}

fn match_node(pat: &SyntaxNode, src: &SyntaxNode, binds: &mut Binds) -> bool {
    if !kinds_compatible(pat, src) {
        return false;
    }
    // Deep only inside a block: that is where "anywhere in this loop" has
    // to mean what a user expects. An argument list's `...` stays an
    // ordinary sibling wildcard, since `f(..., $X, ...)` reaching into a
    // nested call's arguments would be nobody's intent.
    let deep = pat.kind() == SyntaxKind::Block;

    // A declaration's modifiers match as a *subset*: every modifier the
    // pattern names must be there, and any the pattern is silent about are
    // ignored. Writing one narrows the search rather than pinning the whole
    // list, so `static void addChild*() { ... }` finds
    // `@isTest static void addChildQueries_success()` -- which matching the
    // list exactly did not, since the pattern had no way to say "and
    // whatever else this is annotated with".
    if carries_modifiers(pat.kind()) {
        let (pat_mods, pat_rest) = partition_modifiers(&significant_children(pat));
        let (src_mods, src_rest) = partition_modifiers(&significant_children(src));
        let every_named_modifier_present = pat_mods.iter().all(|wanted| {
            src_mods.iter().any(|have| {
                let mut attempt = binds.clone();
                match_element(wanted, have, &mut attempt)
            })
        });
        if !every_named_modifier_present {
            return false;
        }
        if pat.kind() == SyntaxKind::ConstructorDecl && src.kind() == SyntaxKind::MethodDecl {
            return match_without_return_type(&pat_rest, &src_rest, binds);
        }
        return match_seq(&pat_rest, &src_rest, binds);
    }

    // An array type is flat -- `Map<Id, X>[]` is `Map`, a `TypeArgList`,
    // `[`, `]` as siblings, with no node for the element type -- so a hole
    // standing for the type's name must take the whole run before the
    // brackets, not one sibling, or `$T[]` finds `String[]` but never
    // `Map<Id, X>[]` or `Schema.X[]`. Only the brackets are compared.
    // ponytail: matcher-side because nesting the element type in the real
    // tree would move every array type's hover/go-to target in the binder.
    if pat.kind() == SyntaxKind::Type {
        let (pat_items, src_items) = (significant_children(pat), significant_children(src));
        let is_bracket =
            |e: &SyntaxElement| matches!(e.kind(), SyntaxKind::LBrack | SyntaxKind::RBrack);
        let pat_name = pat_items.iter().take_while(|e| !is_bracket(e)).count();
        if let ([name], brackets) = pat_items.split_at(pat_name) {
            if matches!(hole_of(name), Some(Hole::Capture(_) | Hole::Anonymous)) {
                let src_name = src_items.iter().take_while(|e| !is_bracket(e)).count();
                let (run, src_brackets) = src_items.split_at(src_name);
                return !run.is_empty()
                    && brackets.len() == src_brackets.len()
                    && bind_capture(name, &run_text(run), binds);
            }
        }
    }

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

/// Match a member pattern with no return type against a method, skipping
/// the method's return type -- `$F(String $_) { ... }` finds methods of
/// every return type as well as constructors.
///
/// The pattern parsed as a constructor, so its name is a `Type` where the
/// method's is a `DeclName`; the two are compared as a name, by hole or by
/// text, and everything after the name as usual.
fn match_without_return_type(
    pat: &[SyntaxElement],
    src: &[SyntaxElement],
    binds: &mut Binds,
) -> bool {
    let [pat_name, pat_rest @ ..] = pat else {
        return false;
    };
    let Some(at) = src.iter().position(|e| e.kind() == SyntaxKind::DeclName) else {
        return false;
    };
    let src_name = &src[at];
    let name_matches = if hole_of(pat_name).is_some() {
        match_element(pat_name, src_name, binds)
    } else {
        let text = |e: &SyntaxElement| match e {
            NodeOrToken::Token(t) => t.text().to_string(),
            NodeOrToken::Node(n) => significant_text(n),
        };
        text(pat_name).eq_ignore_ascii_case(&text(src_name))
    };
    name_matches && match_seq(pat_rest, &src[at + 1..], binds)
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
    // A member written with no return type parses as a constructor, but it
    // means "any return type": the return type is optional, as modifiers
    // are, so it matches methods too. See `match_without_return_type`.
    if pat.kind() == SyntaxKind::ConstructorDecl && src.kind() == SyntaxKind::MethodDecl {
        return true;
    }
    let both_loops = matches!(pat.kind(), SyntaxKind::ForEachStmt | SyntaxKind::ForStmt)
        && matches!(src.kind(), SyntaxKind::ForEachStmt | SyntaxKind::ForStmt);
    if both_loops {
        return significant_children(pat)
            .iter()
            .any(|c| hole_of(c) == Some(Hole::Ellipsis));
    }

    // A brace initializer written as nothing but `{ ... }` has no `=>` to
    // make it a map, so it parses as a *set* initializer -- and then never
    // matched a map, returning nothing for `new Map<$K, $V>{ ... }` while
    // NPSP holds 354 map initializers. Only the bare form is widened: once
    // an element is written out, its shape says which kind is meant.
    let initializer = |k: SyntaxKind| {
        matches!(
            k,
            SyntaxKind::SetInitializer | SyntaxKind::MapInitializer | SyntaxKind::ArrayInitializer
        )
    };
    initializer(pat.kind())
        && initializer(src.kind())
        && matches!(
            significant_children(pat).as_slice(),
            [_, middle, _] if hole_of(middle) == Some(Hole::Ellipsis)
        )
}

fn match_element(pat: &SyntaxElement, src: &SyntaxElement, binds: &mut Binds) -> bool {
    // `^X` and `${X}` match whatever X matches, and remember where.
    if let NodeOrToken::Node(marker) = pat {
        if matches!(
            marker.kind(),
            SyntaxKind::PatternFocus | SyntaxKind::PatternGroup
        ) {
            let Some(inner) = marker_inner(marker) else {
                return false;
            };
            return match_element(&inner, src, binds) && record_marker(marker, binds, src);
        }
    }
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
            let record = CAPTURE_RANGES.load(std::sync::atomic::Ordering::Relaxed)
                && !binds.contains_key(&name);
            if !bind_capture(pat, &text, binds) {
                return false;
            }
            if record {
                record_capture_range(binds, &name, src);
            }
            return true;
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

/// Bind a capture hole to `text`, or check it against an earlier binding.
///
/// Unification, scoped per match: the first occurrence binds, later ones
/// must agree. Compared on significant text so `a.b` and `a . b` are the
/// same capture, matching ast-grep's structural rather than byte-wise
/// notion of "the same", and case-insensitively because Apex identifiers
/// are -- `acc` and `Acc` are one variable, so they are one capture. `$_`
/// binds nothing and so always agrees.
fn bind_capture(hole: &SyntaxElement, text: &str, binds: &mut Binds) -> bool {
    let Some(Hole::Capture(name)) = hole_of(hole) else {
        return true;
    };
    match binds.get(&name) {
        Some(existing) => existing.eq_ignore_ascii_case(text),
        None => {
            binds.insert(name, text.to_string());
            true
        }
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
        // `..., $X` must still match a list whose only item is X: an
        // ellipsis that consumes nothing takes its separator with it.
        if rest.first().is_some_and(is_comma) {
            let mut attempt = binds.clone();
            if let Some(name) = &name {
                attempt.insert(name.clone(), String::new());
            }
            if match_seq(&rest[1..], src, &mut attempt) {
                *binds = attempt;
                return true;
            }
        }
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

    // The mirror image: `$X, ...` matches a list ending at X. Only where
    // the source has no comma here -- where it has one, matching it keeps
    // it out of a sequence capture's run, so `f($X, $...REST)` binds `b`
    // from `f(a, b)` rather than `, b`.
    if is_comma(head)
        && !src.first().is_some_and(is_comma)
        && pat
            .get(1)
            .is_some_and(|e| matches!(hole_of(e), Some(Hole::Ellipsis | Hole::SeqCapture(_))))
    {
        let mut attempt = binds.clone();
        if match_seq(&pat[1..], src, &mut attempt) {
            *binds = attempt;
            return true;
        }
    }

    let Some(first) = src.first() else {
        return false;
    };
    match_element(head, first, binds) && match_seq(&pat[1..], &src[1..], binds)
}

fn is_comma(element: &SyntaxElement) -> bool {
    element.kind() == SyntaxKind::Comma
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
    // A `^` on the element survives the unwrapping: the focus is recorded
    // on whichever candidate the element matched.
    // A `${...}` group keeps what it matched the same way.
    let marker = match head {
        NodeOrToken::Node(n)
            if matches!(
                n.kind(),
                SyntaxKind::PatternFocus | SyntaxKind::PatternGroup
            ) =>
        {
            Some(n.clone())
        }
        _ => None,
    };
    let inner = match &marker {
        Some(m) => marker_inner(m),
        None => Some(head.clone()),
    };
    let tells_apart = marker.as_ref().is_some_and(marker_tells_readings_apart);
    let unwrapped = inner.as_ref().and_then(expression_inside);
    for (i, candidate) in candidates.iter().enumerate() {
        // A position already reported is refused before the comparison,
        // not after it: re-matching for the next `^` walks the same
        // candidates again, and comparing each one would go quadratic.
        if tells_apart && focus_seen(candidate) {
            continue;
        }
        for probe in [Some(head), unwrapped.as_ref()].into_iter().flatten() {
            let mut attempt = binds.clone();
            if match_element(probe, candidate, &mut attempt)
                && marker
                    .as_ref()
                    .is_none_or(|m| record_marker(m, &mut attempt, candidate))
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

fn is_comment_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::DocComment
    )
}

/// Does this kind carry a modifier list the pattern may narrow?
///
/// Every place Apex lets one be written: type and member declarations, a
/// method or catch parameter, a property accessor, and a local variable.
fn carries_modifiers(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::ClassDecl
            | SyntaxKind::InterfaceDecl
            | SyntaxKind::EnumDecl
            | SyntaxKind::MethodDecl
            | SyntaxKind::ConstructorDecl
            | SyntaxKind::FieldDecl
            | SyntaxKind::PropertyDecl
            | SyntaxKind::PropertyAccessor
            | SyntaxKind::FormalParam
            | SyntaxKind::CatchClause
            | SyntaxKind::LocalVarDeclStmt
    )
}

/// Is this child one of the node's modifiers?
///
/// Usually a `Modifier` or `Annotation` node, but a local declaration
/// bumps `final`/`transient` as bare tokens rather than wrapping them, so
/// those count too -- otherwise `Integer $x = ...;` would fail to match
/// `final Integer x = 1;` for a reason the user cannot see.
fn is_modifier(element: &SyntaxElement) -> bool {
    matches!(
        element.kind(),
        SyntaxKind::Modifier | SyntaxKind::Annotation | SyntaxKind::Final | SyntaxKind::Transient
    )
}

/// Separate a node's modifiers from the rest of its children.
///
/// A partition rather than a leading run, because they are not always
/// leading: a catch clause's modifiers sit after `catch (`, so taking a
/// prefix would find none and silently fall back to exact matching.
fn partition_modifiers(children: &[SyntaxElement]) -> (Vec<SyntaxElement>, Vec<SyntaxElement>) {
    children.iter().cloned().partition(is_modifier)
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

/// Like [`matches_in_file`], but keeping only the matches that pass every
/// `--and` condition and no `--not` condition.
///
/// "Contains" includes the match node itself, not only its descendants,
/// which is what lets a negation narrow a pattern rather than only exclude
/// what is nested inside it: `[SELECT ... FROM $O]` minus
/// `[SELECT ... FROM $O WHERE ...]` is "a query with no WHERE clause", and
/// the two patterns describe the very same node.
fn matches_in_file_filtered(
    pattern: &Pattern,
    ands: &[Condition],
    nots: &[Condition],
    types: Option<&TypeContext>,
    display_path: &Path,
    src: &str,
) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);
    let root = parse.syntax();

    // A comment is a token, so it is reported at its own position rather
    // than its enclosing node's, which would point at code the comment only
    // happens to sit inside.
    if let Pattern::Comment(re) = pattern {
        return root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| is_comment_kind(t.kind()) && re.is_match(t.text()))
            .map(|t| {
                let start = usize::from(t.text_range().start());
                let (line, col) = index.line_col(src, start as u32);
                Site {
                    path: display_path.to_path_buf(),
                    line,
                    col,
                    text: crate::project::collapse(t.text()),
                }
            })
            .collect();
    }

    // With a `^`, each place it lands is a hit, and a place is reported
    // once: `{ ... ^X ... }` matches a block and the blocks nested in it,
    // and they would all name the same X.
    // A `${...}` group enumerates the same way, so a condition on it is
    // checked for every place it lands; without a `^`, each match is still
    // reported once, at its start.
    let focus_pattern = match pattern {
        Pattern::Tree(green) => Some(SyntaxNode::new_root(green.clone())).filter(|p| {
            p.descendants().any(|n| {
                matches!(
                    n.kind(),
                    SyntaxKind::PatternFocus | SyntaxKind::PatternGroup
                )
            })
        }),
        _ => None,
    };
    let mut reported = std::collections::HashSet::new();
    let file_types = types.and_then(|t| t.for_file(display_path));
    pattern
        .matches_with_binds(&root)
        .into_iter()
        .flat_map(|(node, binds)| {
            let readings = match &focus_pattern {
                Some(p) => focus_solutions(p, &node, binds),
                None => vec![binds],
            };
            readings.into_iter().map(move |b| (node.clone(), b))
        })
        .filter(|(node, binds)| passes(node, binds, ands, nots, file_types.as_ref()))
        .filter_map(|(node, binds)| {
            let mut site = site_for(display_path, src, &index, &node)?;
            // Report at the `^` if the pattern has one; the text stays the
            // whole match, so the line still shows its context.
            if let Some(offset) = binds.get(FOCUS_KEY).and_then(|o| o.parse().ok()) {
                if !reported.insert(offset) {
                    return None;
                }
                (site.line, site.col) = index.line_col(src, offset);
            } else if focus_pattern.is_some()
                && !reported.insert(u32::from(node.text_range().start()))
            {
                return None;
            }
            Some(site)
        })
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
    replacement: &Rewrite,
    ands: &[Condition],
    nots: &[Condition],
    types: Option<&TypeContext>,
    paths: &[PathBuf],
    cwd: &Path,
    refused: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Site>, ArgError> {
    walk_project(paths, cwd, |display_path, src| {
        rewrite_file(
            pattern,
            replacement,
            ands,
            nots,
            types,
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
    replacement: &Rewrite,
    ands: &[Condition],
    nots: &[Condition],
    types: Option<&TypeContext>,
    display_path: &Path,
    src: &str,
    refused: &std::sync::atomic::AtomicBool,
) -> Vec<Site> {
    let parse = parse_apex_file(display_path, src);
    let index = LineIndex::new(src);
    let root = parse.syntax();

    let file_types = types.and_then(|t| t.for_file(display_path));
    let (edits, sites) = match replacement {
        Rewrite::Whole(replacement) => {
            let matches: Vec<_> = pattern
                .matches_with_binds(&root)
                .into_iter()
                .filter(|(node, binds)| passes(node, binds, ands, nots, file_types.as_ref()))
                .collect();
            whole_match_edits(
                outermost_only(matches),
                replacement,
                display_path,
                src,
                &index,
            )
        }
        Rewrite::Groups(groups) => {
            let Pattern::Tree(green) = pattern else {
                return Vec::new();
            };
            let pattern_root = SyntaxNode::new_root(green.clone());
            let readings: Vec<_> = pattern
                .matches_with_binds(&root)
                .into_iter()
                .flat_map(|(node, binds)| {
                    focus_solutions(&pattern_root, &node, binds)
                        .into_iter()
                        .map(move |b| (node.clone(), b))
                })
                .filter(|(node, binds)| passes(node, binds, ands, nots, file_types.as_ref()))
                .map(|(_, binds)| binds)
                .collect();
            group_edits(&readings, groups, display_path, src, &index)
        }
    };
    apply_edits(edits, sites, &parse, display_path, src, refused)
}

/// The edits a whole-match template makes: one per outermost match.
fn whole_match_edits(
    matches: Vec<(SyntaxNode, Binds)>,
    replacement: &Replacement,
    display_path: &Path,
    src: &str,
    index: &LineIndex,
) -> (Vec<Edit>, Vec<Site>) {
    let mut edits = Vec::new();
    let mut sites = Vec::new();
    for (node, binds) in matches {
        let Some(range) = apex_syntax::significant_range(&node) else {
            continue;
        };
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let (start, end) = if replacement.is_deletion() {
            widen_deletion_to_line(src, start, end)
        } else {
            (start, end)
        };
        let Some(mut site) = site_for(display_path, src, index, &node) else {
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
    (edits, sites)
}

/// The edits group templates make: each group's own span, in every
/// reading of every match, and nothing outside the groups. A span two
/// readings or two nested matches both name is rewritten once.
fn group_edits(
    readings: &[Binds],
    groups: &[(usize, Replacement)],
    display_path: &Path,
    src: &str,
    index: &LineIndex,
) -> (Vec<Edit>, Vec<Site>) {
    let mut done = std::collections::HashSet::new();
    let mut edits = Vec::new();
    let mut sites = Vec::new();
    for binds in readings {
        for (number, replacement) in groups {
            let Some((start, end)) = group_span(binds, *number) else {
                continue;
            };
            let new_text = replacement.render(binds);
            if !done.insert((start, end, new_text.clone())) {
                continue;
            }
            let old = crate::project::collapse(&src[start..end]);
            let (line, col) = index.line_col(src, start as u32);
            let (start, end) = if replacement.is_deletion() {
                widen_deletion_to_line(src, start, end)
            } else {
                (start, end)
            };
            sites.push(Site {
                path: display_path.to_path_buf(),
                line,
                col,
                text: if replacement.is_deletion() {
                    format!("{old} -> (deleted)")
                } else {
                    format!("{old} -> {}", crate::project::collapse(&new_text))
                },
            });
            edits.push(Edit {
                start,
                end,
                text: new_text,
            });
        }
    }
    (edits, sites)
}

/// Check, apply and write a file's edits: refused whole if any two cross,
/// or if the result would not parse.
fn apply_edits(
    mut edits: Vec<Edit>,
    sites: Vec<Site>,
    parse: &apex_parser::Parse,
    display_path: &Path,
    src: &str,
    refused: &std::sync::atomic::AtomicBool,
) -> Vec<Site> {
    if edits.is_empty() {
        return Vec::new();
    }

    // Crossing overlaps are genuinely ambiguous -- either rewrite changes
    // text the other was computed against -- so both are skipped and named
    // rather than one being picked silently. Nested matches never reach
    // here; `outermost_only` has already resolved those, and groups that
    // two readings share are rewritten once.
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
    matches_in_file_filtered(pattern, &[], &[], None, display_path, src)
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

    /// Search `src` as a class, keeping matches that pass the conditions.
    fn filtered_hits(pattern: &str, and: &[&str], not: &[&str], src: &str) -> Vec<Site> {
        let pattern = Pattern::compile(pattern).expect("pattern should compile");
        let bound = pattern.capture_names();
        let compile = |srcs: &[&str]| -> Vec<Condition> {
            srcs.iter()
                .map(|s| Condition::compile(s, &bound).unwrap_or_else(|e| panic!("{}", e.0)))
                .collect()
        };
        matches_in_file_filtered(
            &pattern,
            &compile(and),
            &compile(not),
            None,
            Path::new("T.cls"),
            src,
        )
    }

    /// Like [`filtered_hits`], with `src` bound as a one-file project so
    /// `:` conditions have types to read.
    fn typed_hits(pattern: &str, and: &[&str], not: &[&str], src: &str) -> Vec<String> {
        typed_hits_in("T.cls", pattern, and, not, src)
    }

    fn typed_hits_in(
        file: &str,
        pattern: &str,
        and: &[&str],
        not: &[&str],
        src: &str,
    ) -> Vec<String> {
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "apexls-query-types-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), src).unwrap();
        CAPTURE_RANGES.store(true, std::sync::atomic::Ordering::Relaxed);
        let types = TypeContext::new(&dir, &dir);
        let pattern = Pattern::compile(pattern).expect("pattern should compile");
        let bound = pattern.capture_names();
        let compile = |srcs: &[&str]| -> Vec<Condition> {
            srcs.iter()
                .map(|s| Condition::compile(s, &bound).unwrap_or_else(|e| panic!("{}", e.0)))
                .collect()
        };
        let sites = matches_in_file_filtered(
            &pattern,
            &compile(and),
            &compile(not),
            Some(&types),
            Path::new(file),
            src,
        );
        std::fs::remove_dir_all(&dir).ok();
        sites.into_iter().map(|s| s.text).collect()
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

    /// A bare `...` still cannot stand for an operator: an operator is one
    /// token, and only a *capture* may take its place.
    #[test]
    fn an_ellipsis_cannot_stand_for_an_operator() {
        assert!(Pattern::compile("$L ... $R").is_err());
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
        assert!(Pattern::compile("= = =").is_err());
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
        let filtered =
            |not: &[&str], and: &[&str]| filtered_hits("kind:SoqlExpr", and, not, &src).len();
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

    /// `*` in a pattern is only ever multiplication; globs live in
    /// conditions.
    #[test]
    fn a_star_in_a_pattern_is_multiplication() {
        let src = wrap("        Integer n = a * b;\n        Integer m = x.size() * 2;");
        assert_eq!(hits("$A * $B", &src).len(), 2);
        assert_eq!(hits("x.size() * 2", &src).len(), 1);
    }

    /// Modifiers match as a *subset*: every one the pattern names must be
    /// present, and ones it is silent about are ignored. Writing a modifier
    /// narrows the search rather than pinning the whole list.
    #[test]
    fn modifiers_narrow_rather_than_pin_the_whole_list() {
        let src = "public class T {\n    @isTest static void goNow() { f(); }\n    public void plain() { }\n}\n";
        let find = |pattern: &str| filtered_hits(pattern, &["$M ~ go*"], &[], src).len();
        assert_eq!(find("void $M() { ... }"), 1, "no modifiers named at all");
        assert_eq!(
            find("static void $M() { ... }"),
            1,
            "a subset still matches"
        );
        assert_eq!(find("@isTest static void $M() { ... }"), 1, "all of them");
        assert_eq!(find("@future static void $M() { ... }"), 0, "narrows");
        assert_eq!(find("public void $M() { ... }"), 0, "wrong modifier");
    }

    /// Subset matching applies everywhere Apex lets a modifier be written,
    /// not only on members: a type declaration, a local variable, a catch
    /// parameter, a method parameter, a property accessor.
    #[test]
    fn modifiers_narrow_everywhere_they_can_be_written() {
        let src = "@isTest\npublic with sharing class T {\n    public void go(final Integer a, String b) {\n        final Integer n = 1;\n        Integer m = 2;\n        try { f(); } catch (final DmlException e) { }\n        try { g(); } catch (QueryException e) { }\n    }\n}\n";
        let find = |pattern: &str| {
            let p = Pattern::compile(pattern).expect("pattern should compile");
            matches_in_file(&p, Path::new("T.cls"), src).len()
        };

        // A type declaration's annotations and sharing modifier.
        assert_eq!(find("class $C { ... }"), 1, "none named");
        assert_eq!(find("@isTest class $C { ... }"), 1, "named and present");
        assert_eq!(find("with sharing class $C { ... }"), 1);
        assert_eq!(find("@future class $C { ... }"), 0, "named and absent");

        // A local declaration's `final`, which the grammar bumps as a bare
        // token rather than wrapping in a `Modifier` node.
        assert_eq!(find("Integer $v = ...;"), 2, "final and plain alike");
        assert_eq!(
            find("final Integer $v = ...;"),
            1,
            "narrows to the final one"
        );

        // A catch parameter's modifiers, which sit after `catch (` rather
        // than leading the node -- the reason this is a partition and not
        // a leading run.
        assert_eq!(find("catch (...) { }"), 2);
        assert_eq!(find("catch (final $T $e) { }"), 1);

        // A method parameter's `final`.
        assert_eq!(find("public void go(Integer $a, $T $b) { ... }"), 1);
        assert_eq!(find("public void go(final Integer $a, $T $b) { ... }"), 1);
    }

    /// Comments are trivia, invisible to every structural comparison and to
    /// `regex:`, so `comment:` is the only way to find one. It reports one
    /// hit per *comment*, not per line -- a block comment mentioning TODO on
    /// two lines is one hit, which is why it can undercount `git grep`.
    #[test]
    fn comment_searches_comment_text() {
        let src = wrap(
            "        // TODO: fix this\n        f(); /* TODO one\n         TODO two */\n        String s = 'TODO in a string';",
        );
        let p = Pattern::compile("comment:TODO").expect("compiles");
        let found = matches_in_file(&p, Path::new("T.cls"), &src);
        assert_eq!(found.len(), 2, "two comments, the block one counted once");
        assert!(
            found.iter().all(|m| !m.text.contains("in a string")),
            "a string literal is not a comment",
        );
    }

    /// A capture may stand in an operator's place -- the one hole admitted
    /// at a choice point, fenced so it cannot misread a statement run.
    #[test]
    fn a_capture_can_stand_for_an_operator() {
        let src = wrap(
            "        Integer a = x + y;\n        Integer b = x * y;\n        Boolean c = x == y;",
        );
        assert_eq!(hits("$L $OP $R", &src).len(), 3, "any binary operator");
        assert_eq!(hits("x $OP y", &src).len(), 3);
    }

    /// The guard that makes operator captures safe. The first attempt read
    /// `{ ... $X = y; }` as "the ellipsis, operated on by $X" and swallowed
    /// the assignment after it; an ellipsis on the left means a statement
    /// run, never an operand.
    #[test]
    fn an_operator_capture_never_follows_an_ellipsis() {
        let src = wrap("        a();\n        cs = f();");
        assert_eq!(
            hits("{ ... $X = f(); }", &src).len(),
            1,
            "an ellipsis then an assignment, not an operator expression",
        );
    }

    /// `{ ... }` has no `=>` to make it a map, so it parses as a set
    /// initializer; bare, it must still match a map's.
    #[test]
    fn a_bare_brace_initializer_matches_a_map_too() {
        let src = wrap(
            "        m = new Map<String, Integer>{ 'a' => 1 };\n        s = new Set<String>{ 'a' };",
        );
        assert_eq!(hits("new Map<$K, $V>{ ... }", &src).len(), 1);
        assert_eq!(hits("new Set<$K>{ ... }", &src).len(), 1);
        assert_eq!(
            hits("new $T{ $k => $v }", &src).len(),
            1,
            "an element pins the kind"
        );
    }

    /// A type hole still takes `[]`, so an array creation can be named.
    #[test]
    fn a_type_hole_can_be_an_array() {
        let src = wrap("        x = new String[]{ 'a' };\n        y = new List<String>{ 'a' };");
        assert_eq!(hits("new $T[]{ ... }", &src), ["3:13:new String[]{ 'a' }"]);
    }

    /// A clause hole no longer ends the clause list, so a clause can be
    /// named after it -- real GROUP BYs nearly always follow a WHERE.
    #[test]
    fn a_clause_can_be_named_after_a_clause_hole() {
        let src = wrap(
            "        a = [SELECT COUNT(Id) FROM Account WHERE x = 1 GROUP BY Name];\n        b = [SELECT Id FROM Account WHERE x = 1];",
        );
        assert_eq!(hits("[SELECT ... FROM $o ... GROUP BY ...]", &src).len(), 1);
        assert_eq!(hits("[SELECT ... FROM $o ...]", &src).len(), 2);
    }

    /// `$f` after `$X` is an operand, not an operator: an operator is always
    /// followed by something.
    #[test]
    fn an_operator_capture_needs_a_right_hand_side() {
        let src = wrap("        upsert records Id;\n        upsert records;");
        assert_eq!(hits("upsert $X $f;", &src), ["3:9:upsert records Id;"]);
    }

    /// A bare `...` stands for any number of `when` arms.
    #[test]
    fn an_ellipsis_stands_for_switch_arms() {
        let src = wrap(
            "        switch on x { when 1 { a(); } when else { b(); } }\n        switch on y { when 1 { a(); } }",
        );
        assert_eq!(hits("switch on $e { ... }", &src).len(), 2);
        assert_eq!(
            hits("switch on $e { ... when else { ... } }", &src).len(),
            1
        );
    }

    /// A static initializer is one node, so it can be named whole.
    #[test]
    fn finds_static_initializers() {
        let src = "public class T {\n    static { init(); }\n    void run() { init(); }\n}\n";
        assert_eq!(hits("static { ... }", src), ["2:5:static { init(); }"]);
    }

    /// An array type is flat in the tree, so the name hole must take the
    /// whole run before the brackets -- generics and dotted names included.
    #[test]
    fn a_type_hole_covers_generic_and_qualified_array_types() {
        let src = wrap(
            "        String[] a;\n        Map<Id, Account>[] b;\n        Schema.SObjectField[] c;\n        List<String> d;",
        );
        assert_eq!(hits("$T[] $v;", &src).len(), 3);
        assert_eq!(
            hits("$T $v;", &src).len(),
            4,
            "a bare type hole is still any type"
        );
        assert_eq!(
            hits("$T[][] $v;", &src).len(),
            0,
            "the bracket count must agree"
        );
        let src = wrap(
            "        Map<Id, X>[] a = new Map<Id, X>[]{};\n        String[] b = new Integer[]{};",
        );
        assert_eq!(
            hits("$T[] $v = new $T[]{ ... };", &src).len(),
            1,
            "and it unifies"
        );
    }

    /// SOSL clauses take a clause hole anywhere, as SOQL's do.
    #[test]
    fn a_sosl_clause_can_be_named_after_a_clause_hole() {
        let src = wrap("        r = [FIND :q IN NAME FIELDS RETURNING Contact(Id) LIMIT 100];");
        for pattern in [
            "[FIND $q ...]",
            "[FIND $q ... RETURNING ... ...]",
            "[FIND $q IN $g FIELDS ...]",
            "[FIND $q ... LIMIT $n]",
        ] {
            assert_eq!(hits(pattern, &src).len(), 1, "{pattern}");
        }
        assert_eq!(hits("[FIND $q IN ALL FIELDS ...]", &src).len(), 0);
    }

    /// A subquery takes clause holes like a top-level query, in the select
    /// list and in a semi-join.
    #[test]
    fn a_subquery_takes_clause_holes() {
        let src = wrap(
            "        a = [SELECT Id, (SELECT Id FROM Contacts WHERE x = 1) FROM Account];\n        b = [SELECT Id FROM Contact WHERE AccountId IN (SELECT Id FROM Account WHERE y = 2)];",
        );
        assert_eq!(
            hits(
                "[SELECT ..., (SELECT ... FROM $r ...), ... FROM $o ...]",
                &src
            )
            .len(),
            1
        );
        assert_eq!(
            hits(
                "[SELECT ... FROM $o WHERE $f IN (SELECT ... FROM $p ...) ...]",
                &src
            )
            .len(),
            1
        );
    }

    /// A hole can be a whole WHERE condition, so a chain can be named
    /// without spelling out each comparison.
    #[test]
    fn a_hole_can_be_a_whole_soql_condition() {
        let src = wrap(
            "        a = [SELECT Id FROM Account WHERE x = 1 AND y = 2];\n        b = [SELECT Id FROM Account WHERE x = 1 OR y = 2];\n        c = [SELECT Id FROM Account WHERE x = 1];",
        );
        assert_eq!(hits("[SELECT ... FROM $o WHERE $a AND $b]", &src).len(), 1);
        assert_eq!(
            hits("[SELECT ... FROM $o WHERE ... AND ...]", &src).len(),
            1
        );
        assert_eq!(
            hits("[SELECT ... FROM $o WHERE ... AND y = 2]", &src).len(),
            1
        );
        assert_eq!(
            hits("[SELECT ... FROM $o WHERE ...]", &src).len(),
            3,
            "a bare hole is still the whole clause"
        );
        assert_eq!(
            hits("[SELECT ... FROM $o WHERE $f = $v]", &src).len(),
            1,
            "a hole before `=` is still a field"
        );
    }

    /// An ellipsis that consumes nothing takes its comma with it, so
    /// `f(..., $X, ...)` still finds a call whose only argument is X.
    #[test]
    fn an_empty_ellipsis_takes_its_comma_with_it() {
        let src = wrap("        f(a);\n        f(a, b);\n        f(b, a, c);");
        assert_eq!(hits("f(..., a, ...)", &src).len(), 3);
        assert_eq!(hits("f(a, ...)", &src).len(), 2);
        assert_eq!(hits("f(..., a)", &src).len(), 1);

        let p = Pattern::compile("f($X, $...REST)").expect("compiles");
        let binds: Vec<_> = p
            .matches_with_binds(
                &parse_apex_file(Path::new("T.cls"), &wrap("        f(a, b);")).syntax(),
            )
            .into_iter()
            .map(|(_, b)| b.get("REST").cloned())
            .collect();
        assert_eq!(
            binds,
            [Some("b".to_string())],
            "the comma stays out of the run"
        );
    }

    /// A glob tests a capture's text shell-style, anchored and
    /// case-insensitive, with `{a,b}` alternatives.
    #[test]
    fn a_glob_condition_tests_a_captures_text() {
        let src = wrap("        addChild(1);\n        addChildren(2);\n        removeChild(3);\n        ADDCHILD(4);\n        getX(5);\n        getXY(6);");
        let n = |glob: &str| filtered_hits("$M(...)", &[&format!("$M ~ {glob}")], &[], &src).len();
        assert_eq!(n("addChild*"), 3, "prefix, case-insensitively");
        assert_eq!(
            n("addChild"),
            2,
            "anchored: a glob with no wildcard is equality"
        );
        assert_eq!(n("*Child"), 3);
        assert_eq!(n("{add,remove}Child"), 3, "alternatives");
        assert_eq!(n("get?"), 1, "one character");
        assert_eq!(n("*"), 6);
    }

    /// A regex is unanchored and case-sensitive, Perl-style, unless
    /// flagged: `/re/i`, and `x`, `s`, `m`.
    #[test]
    fn a_regex_condition_is_unanchored_and_takes_flags() {
        let src = wrap("        getName(1);\n        setName(2);\n        forget(3);\n        GetAll(4);\n        getter(5);");
        let n = |re: &str| filtered_hits("$M(...)", &[&format!("$M ~ {re}")], &[], &src).len();
        assert_eq!(n("/^(get|set)/"), 3, "case-sensitive: not GetAll");
        assert_eq!(n("/get/"), 3, "unanchored: forget too, but not GetAll");
        assert_eq!(n("/^get/i"), 3, "flagged: GetAll too");
        assert_eq!(
            n("/^(get|set)[A-Z]/"),
            2,
            "[A-Z] means uppercase: not getter"
        );
        assert_eq!(
            n("/^ g e t  # a comment\n/x"),
            2,
            "x ignores whitespace and comments"
        );
        assert_eq!(n("/^GET/ix"), 3, "flags combine");
    }

    /// A regex must close, and only known flags are accepted.
    #[test]
    fn a_regex_condition_rejects_bad_flags() {
        let bound = Pattern::compile("$M(...)").unwrap().capture_names();
        let err = |c: &str| {
            Condition::compile(c, &bound)
                .err()
                .map(|e| e.0)
                .unwrap_or_default()
        };
        assert!(
            err("$M ~ /get").contains("unclosed regex"),
            "{}",
            err("$M ~ /get")
        );
        assert!(err("$M ~ /get/g").contains("unknown regex flag `g`"));
        assert!(
            Condition::compile("$M ~ /a/b/i", &bound).is_ok(),
            "the last slash closes it"
        );
    }

    /// `$V` inside a glob is another capture's text, so `--not '$T ~ $V'`
    /// says two captures differ.
    #[test]
    fn a_glob_can_compare_two_captures() {
        let src = wrap("        List<String> a = new List<String>();\n        List<Object> b = new List<String>();");
        let pattern = "List<$T> $_ = new List<$V>();";
        assert_eq!(
            filtered_hits(pattern, &[], &["$T ~ $V"], &src).len(),
            1,
            "they differ"
        );
        assert_eq!(
            filtered_hits(pattern, &["$T ~ $V"], &[], &src).len(),
            1,
            "they agree"
        );
    }

    /// A condition may only test a capture the pattern binds, and a glob
    /// must close its braces.
    #[test]
    fn a_condition_names_a_bound_capture() {
        let bound = Pattern::compile("$M(...)").unwrap().capture_names();
        let err = |c: &str| {
            Condition::compile(c, &bound)
                .err()
                .map(|e| e.0)
                .unwrap_or_default()
        };
        assert!(
            err("$X ~ a*").contains("not a capture"),
            "{}",
            err("$X ~ a*")
        );
        assert!(err("$M ~ $Y").contains("not a capture"));
        assert!(err("$_ ~ a*").contains("not a capture"));
        assert!(err("$M ~ {a,b").contains("unclosed"));
        assert!(err("$M ~ /(/").contains("invalid regex"));
        assert!(
            err(" ~ a*").contains("no capture before its operator"),
            "a shell ate `$M`"
        );
        assert!(err(" : String").contains("single quotes"));
        assert!(
            Condition::compile("$M.foo()", &bound).is_ok(),
            "no `~`, so a pattern"
        );
    }

    /// A text condition and a pattern condition combine.
    #[test]
    fn conditions_combine() {
        let src = "public class T {\n    void addA() { System.debug(1); }\n    void addB() { }\n    void other() { }\n}\n";
        let pattern = "void $M() { ... }";
        assert_eq!(filtered_hits(pattern, &["$M ~ add*"], &[], src).len(), 2);
        assert_eq!(
            filtered_hits(pattern, &["$M ~ add*"], &["System.debug(...);"], src).len(),
            1
        );
        assert_eq!(
            filtered_hits(pattern, &["System.debug(...);"], &[], src).len(),
            1
        );
    }

    /// A member pattern with no return type means any return type, so it
    /// matches methods as well as constructors -- as with modifiers,
    /// leaving it out stops pinning it.
    #[test]
    fn a_return_type_is_optional() {
        let src = "public class T {\n    public T(String s) { }\n    public void run(String s) { }\n    Integer count(String s) { return 1; }\n    void other(Integer i) { }\n    abstract void bare(String s);\n}\n";
        assert_eq!(
            hits("$F(String $_) { ... }", src).len(),
            3,
            "constructor, void and Integer"
        );
        assert_eq!(
            hits("run(String $_) { ... }", src).len(),
            1,
            "a literal name"
        );
        assert_eq!(
            hits("public $F(...) { ... }", src).len(),
            2,
            "modifiers still narrow"
        );
        assert_eq!(
            hits("void $F(String $_) { ... }", src).len(),
            1,
            "a written return type still pins"
        );
        assert_eq!(
            filtered_hits("$F(...) { ... }", &["$F ~ c*"], &[], src).len(),
            1,
            "the name binds for conditions"
        );
    }

    /// A capture can be a typed `when` arm's type or variable.
    #[test]
    fn a_typed_when_arm_takes_captures() {
        let src = wrap("        switch on o {\n            when Account a { f(a); }\n            when Contact c { g(c); }\n            when else { h(); }\n        }");
        assert_eq!(
            hits("switch on $o { ... when $T $v { ... } ... }", &src).len(),
            1
        );
        let arms = filtered_hits(
            "switch on $o { ... when $T $v { ... } ... }",
            &["$T ~ Account"],
            &[],
            &src,
        );
        assert_eq!(arms.len(), 1, "the type binds for conditions");
    }

    /// `...` before a `finally` or a `catch` stands for any catch clauses,
    /// and after the last clause it still belongs to the enclosing block.
    #[test]
    fn an_ellipsis_stands_for_catch_clauses() {
        let src = wrap("        try { a(); } catch (DmlException e) { b(); } finally { c(); }\n        try { a(); } finally { c(); }\n        try { a(); } catch (Exception e) { b(); }\n        d();");
        assert_eq!(
            hits("try { ... } ... finally { ... }", &src).len(),
            2,
            "any catches, even none"
        );
        assert_eq!(
            hits("try { ... } finally { ... }", &src).len(),
            1,
            "exactly none"
        );
        assert_eq!(
            hits("try { ... } ... catch (Exception $e) { ... }", &src).len(),
            1
        );
        assert_eq!(
            hits("try { ... } ...", &src).len(),
            3,
            "a trailing ellipsis: any clauses"
        );
        assert_eq!(
            hits("{ ... try { ... } catch ($E $e) { ... } ... }", &src).len(),
            1,
            "a statement run after the try, not its clauses"
        );
    }

    /// Where each hit is reported, as `line:col`.
    fn positions(pattern: &str, src: &str) -> Vec<String> {
        hits(pattern, src)
            .into_iter()
            .map(|h| h.splitn(3, ':').take(2).collect::<Vec<_>>().join(":"))
            .collect()
    }

    /// `^` marks where a match is reported: the start of the construct
    /// after it, at a statement, an expression, a member or a name.
    #[test]
    fn a_caret_moves_the_reported_position() {
        let src = "public class T {\n    @isTest static void go() {\n        a();\n        List<Account> x = [SELECT Id FROM Account];\n        System.assertEquals(1, x.size());\n    }\n}\n";
        assert_eq!(
            positions("void $_(...) { ... }", src),
            ["2:5"],
            "no caret: the match start"
        );
        assert_eq!(
            positions("void $_(...) { ... ^[SELECT ... FROM $o ...] ... }", src),
            ["4:27"],
            "the query, found deep inside a declaration"
        );
        assert_eq!(
            positions("System.assertEquals($a, ^$b)", src),
            ["5:32"],
            "an argument"
        );
        assert_eq!(
            positions("void ^$m() { ... }", src),
            ["2:25"],
            "a method name"
        );
        assert_eq!(
            positions("{ ... ^a(); ... }", src),
            ["3:9"],
            "a statement after an ellipsis"
        );
        assert_eq!(
            positions("class $C { ... ^@isTest $_ $m() { ... } ... }", src),
            ["2:5"],
            "a member, at its annotation"
        );
        assert_eq!(
            positions("^System.assertEquals(...)", src),
            ["5:9"],
            "on the whole: the default"
        );
    }

    /// Each place a `^` can land is a hit of its own, and conditions are
    /// checked for each rather than for the first reading only.
    #[test]
    fn a_caret_reports_every_place_it_lands() {
        let src = "public class T {\n    void go() {\n        List<Account> a = [SELECT Id FROM Account];\n        if (x) {\n            List<Contact> c = [SELECT Id FROM Contact];\n        }\n        a();\n        b();\n    }\n}\n";
        assert_eq!(
            positions("void $_(...) { ... ^[SELECT ... FROM $o ...] ... }", src),
            ["3:27", "5:31"],
            "both queries, the nested one too"
        );
        assert_eq!(
            positions("{ ... ^[SELECT ... FROM $o ...] ... }", src),
            ["3:27", "5:31"],
            "each once, though the inner block matches as well"
        );
        let calls = filtered_hits("void $_() { ... ^$f(); ... }", &["$f ~ b*"], &[], src);
        assert_eq!(calls.len(), 1, "b() passes though a() is the first reading");
        assert_eq!((calls[0].line, calls[0].col), (8, 9));
    }

    /// `^` between two operands is still XOR, and a pattern takes one.
    #[test]
    fn a_caret_between_operands_is_xor() {
        let src = wrap("        Integer n = a ^ b;");
        assert_eq!(hits("$a ^ $b", &src).len(), 1);
        assert_eq!(
            positions("$a ^ ^$b", &src),
            ["3:25"],
            "focus on XOR's right operand"
        );
        let err = Pattern::compile("f(^$a, ^$b)")
            .err()
            .map(|e| e.0)
            .unwrap_or_default();
        assert!(err.contains("only one `^`"), "{err}");
    }

    /// `$x : TYPE` tests the type the binder infers -- of a local, a
    /// literal, a call's return, a query -- as a glob over its text.
    #[test]
    fn a_type_condition_tests_the_inferred_type() {
        let src = "public class T {\n    void go(Integer n, List<Account> accs) {\n        String s = 'a';\n        System.debug(s);\n        System.debug(n);\n        System.debug('lit');\n        System.debug(accs);\n        System.debug(s.length());\n        System.debug([SELECT Id FROM Contact]);\n    }\n}\n";
        let debug = |ty: &str| typed_hits("System.debug($v)", &[&format!("$v : {ty}")], &[], src);
        assert_eq!(
            debug("String"),
            ["System.debug(s)", "System.debug('lit')"],
            "a local and a literal"
        );
        assert_eq!(
            debug("integer"),
            ["System.debug(n)", "System.debug(s.length())"],
            "a parameter and a return, any case"
        );
        assert_eq!(
            debug("List<*>"),
            [
                "System.debug(accs)",
                "System.debug([SELECT Id FROM Contact])"
            ],
            "a glob"
        );
        assert_eq!(
            debug("List<Contact>"),
            ["System.debug([SELECT Id FROM Contact])"],
            "a query's rows"
        );
    }

    /// Types come from every body the binder walks, not only methods: a
    /// field initializer, a property accessor, a trigger.
    #[test]
    fn a_type_condition_reaches_initializers_accessors_and_triggers() {
        let class = "public class T {\n    static String name = 'n';\n    static String greeting = 'hi ' + name;\n    List<String> items;\n    public Integer size { get { return items.size(); } }\n}\n";
        assert_eq!(
            typed_hits("'hi ' + $x", &["$x : String"], &[], class),
            ["'hi ' + name"],
            "a field initializer"
        );
        assert_eq!(
            typed_hits("return $e;", &["$e : Integer"], &[], class),
            ["return items.size();"],
            "a property accessor"
        );
        let trigger = "trigger T on Account (before insert) {\n    String s = 'x';\n    System.debug(s);\n    System.debug(1);\n}\n";
        assert_eq!(
            typed_hits_in(
                "T.trigger",
                "System.debug($v)",
                &["$v : String"],
                &[],
                trigger
            ),
            ["System.debug(s)"],
            "a trigger body"
        );
    }

    /// Inside `:`, `$b` is b's *type*, so `$a : $b` compares types where
    /// `$a ~ $b` compares spelling; an untyped capture fails either way.
    #[test]
    fn a_type_condition_compares_two_captures() {
        let src = "public class T {\n    void go(Integer i, Integer j, String s, Object o) {\n        f(i, j);\n        f(i, s);\n        f(i, undeclared);\n    }\n}\n";
        assert_eq!(
            typed_hits("f($a, $b)", &["$a : $b"], &[], src),
            ["f(i, j)"],
            "same type, different spelling"
        );
        assert_eq!(
            typed_hits("f($a, $b)", &[], &["$a : $b"], src),
            ["f(i, s)", "f(i, undeclared)"],
            "--not keeps the untyped"
        );
        assert_eq!(
            typed_hits("f($a, $b)", &["$b : *"], &[], src),
            ["f(i, j)", "f(i, s)"],
            "`*` means typed at all"
        );
    }

    /// A type condition only reads a capture the pattern binds, and without
    /// a bound project it can never hold.
    #[test]
    fn a_type_condition_needs_a_bound_capture() {
        let bound = Pattern::compile("f($a)").unwrap().capture_names();
        let err = |c: &str| {
            Condition::compile(c, &bound)
                .err()
                .map(|e| e.0)
                .unwrap_or_default()
        };
        assert!(err("$x : String").contains("not a capture"));
        assert!(Condition::compile("$a : String", &bound)
            .unwrap()
            .needs_types());
        assert!(!Condition::compile("$a ~ String", &bound)
            .unwrap()
            .needs_types());
        let src = wrap("        f(1);");
        assert!(
            filtered_hits("f($a)", &["$a : Integer"], &[], &src).is_empty(),
            "no project bound"
        );
    }

    /// Rewrite `src` as one file with `-r` values `rewrites`, and return
    /// the file afterwards (or an error message) plus what was reported.
    fn rewrite(
        pattern: &str,
        rewrites: &[&str],
        src: &str,
    ) -> Result<(String, Vec<String>), String> {
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "apexls-query-rewrite-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("T.cls");
        std::fs::write(&file, src).unwrap();
        let pattern = Pattern::compile(pattern).map_err(|e| e.0)?;
        let srcs: Vec<String> = rewrites.iter().map(|s| s.to_string()).collect();
        let plan = Rewrite::compile(&srcs, &pattern.capture_names(), pattern.group_count())
            .map_err(|e| e.0)?
            .ok_or("no rewrite")?;
        let refused = std::sync::atomic::AtomicBool::new(false);
        let sites = rewrite_file(&pattern, &plan, &[], &[], None, &file, src, &refused);
        let after = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        Ok((after, sites.into_iter().map(|s| s.text).collect()))
    }

    /// `${...}` numbers a group from the left; `$1 => TEMPLATE` rewrites
    /// only that group's span, and each group can have its own template.
    #[test]
    fn a_group_rewrite_touches_only_the_group() {
        let src = wrap("        f(a, b);");
        let (after, sites) =
            rewrite("f(${$x}, ${$y})", &["$1 => g($1)", "$2 => $x"], &src).unwrap();
        assert!(after.contains("f(g(a), a);"), "{after}");
        assert_eq!(sites, ["a -> g(a)", "b -> a"]);
    }

    /// Inside a deep pattern, a group is rewritten everywhere it lands and
    /// the surrounding match is left alone -- the method survives.
    #[test]
    fn a_group_in_a_deep_pattern_rewrites_every_place_it_lands() {
        let src = "public class T {\n    void go() {\n        a = [SELECT Id FROM Account];\n        if (x) { c = [SELECT Id FROM Contact]; }\n    }\n    Integer keep() { return [SELECT COUNT() FROM Lead]; }\n}\n";
        let (after, sites) = rewrite(
            "void $_() { ... ${[SELECT ... FROM $o ...]} ... }",
            &["$1 => Data.of($o)"],
            src,
        )
        .unwrap();
        assert_eq!(sites.len(), 2, "{sites:?}");
        assert!(after.contains("a = Data.of(Account);"), "{after}");
        assert!(after.contains("c = Data.of(Contact);"), "{after}");
        assert!(after.contains("void go() {"), "the method itself is kept");
        assert!(
            after.contains("[SELECT COUNT() FROM Lead]"),
            "only void methods"
        );
    }

    /// Only the first `=>` separates, at most one space either side of it
    /// is dropped, and an empty template deletes the group -- its line too
    /// when it stands alone.
    #[test]
    fn a_group_template_keeps_its_whitespace() {
        let src = wrap("        f(a);");
        let (after, _) = rewrite(
            "f(${$x})",
            &["$1 =>  new Map<String, Object>{'k' => $x}"],
            &src,
        )
        .unwrap();
        assert!(
            after.contains("f( new Map<String, Object>{'k' => a});"),
            "{after}"
        );
        let src = wrap("        a();\n        b();");
        let (after, sites) = rewrite("{ ... ${b();} ... }", &["$1 =>"], &src).unwrap();
        assert!(
            !after.contains("b();") && after.contains("        a();\n    }"),
            "{after}"
        );
        assert_eq!(sites, ["b(); -> (deleted)"]);
    }

    /// Groups are numbered holes' only source: a numbered hole, a missing
    /// or doubled group, and a whole-match template beside group ones are
    /// all refused.
    #[test]
    fn group_rewrites_are_checked() {
        let src = wrap("        f(a);");
        let err = |p: &str, r: &[&str]| rewrite(p, r, &src).err().unwrap_or_default();
        assert!(err("f($1)", &["x"]).contains("cannot name a hole"));
        assert!(err("f(${$x})", &["$2 => y"]).contains("has no group $2"));
        assert!(err("f(${$x})", &["$1 => y", "$1 => z"]).contains("rewritten twice"));
        assert!(err("f(${$x})", &["whole", "$1 => y"]).contains("cannot be combined"));
        assert!(err("f(${$x})", &["one", "two"]).contains("only one --replace"));
        assert!(err("${f($x)}", &["x"]).contains("whole pattern"));
        let lost = err("f(${$x})", &[" => ''"]);
        assert!(
            lost.contains("the `$N` before it is missing") && lost.contains("single quotes"),
            "{lost}"
        );
        assert!(err("f(@{$x})", &["x"]).contains("written `${...}`"));
        let (after, _) = rewrite("f(${$x})", &["$1.trim()"], &src).unwrap();
        assert!(
            after.contains("a.trim()") && !after.contains("f("),
            "no `=>`: a whole-match template: {after}"
        );
    }

    /// A group is a capture like any other: `$1` in a condition, and in
    /// search a match is still reported once, at its start.
    #[test]
    fn a_group_works_in_conditions_and_search() {
        let src = wrap("        f(abc);\n        f(xyz);");
        assert_eq!(filtered_hits("f(${$x})", &["$1 ~ a*"], &[], &src).len(), 1);
        let src = "public class T {\n    void go() {\n        a = [SELECT Id FROM Account];\n        c = [SELECT Id FROM Contact];\n    }\n}\n";
        assert_eq!(
            positions("void $_() { ... ${[SELECT ... FROM $o ...]} ... }", src),
            ["2:5"],
            "once per match"
        );
    }

    #[test]
    fn searches_trigger_files_too() {
        let src = "trigger T on Account (before insert) {\n    System.debug('a');\n}\n";
        let pattern = Pattern::compile("System.debug(...);").expect("compiles");
        let found = matches_in_file(&pattern, Path::new("T.trigger"), src);
        assert_eq!(found.len(), 1, "a trigger is not a compilation unit");
    }
}
