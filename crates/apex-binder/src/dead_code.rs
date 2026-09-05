//! Provably-dead-declaration detection: the analysis backing both
//! `apexls-server`'s `textDocument/publishDiagnostics`/`textDocument/codeAction`
//! and the `apexls check` CLI subcommand. Deliberately conservative --
//! Apex's platform (Flow, Aura/LWC, REST, Visualforce) can invoke code
//! with zero textual Apex call sites, so this only ever flags a
//! declaration when it can rule out every channel it knows about; see
//! each filter's own doc comment for exactly which channel it closes.
//!
//! `private` methods/fields/properties and plain local variables were
//! the first, narrowest-possible slice: `private` is genuinely
//! file-scoped by Apex's own visibility rules (one top-level type per
//! file), and a local can only ever be referenced within its own file by
//! construction, so `references_to_in_file`'s file-scoped reverse index
//! is not just an optimization for these, it's the *correct* lookup.
//! `public` widens this real: `references_to_in_file` is *not* enough
//! there (a public member can be referenced from any file), the platform-
//! reflection channels each need a specific annotation to reach a public
//! member (`@AuraEnabled` for Aura/LWC, `@InvocableMethod`/`@InvocableVariable`
//! for Flow, `@RemoteAction`, `@Http*` for REST -- all checked below),
//! and Visualforce needs no annotation at all, so a class actually named
//! as a `.page`'s `controller`/`extensions` is excluded wholesale rather
//! than trying to prove which specific member a page's markup does or
//! doesn't call.
//!
//! `Constructor` is a candidate under the exact same rule as `Method`/
//! `Field`/`Property`: a `private`/`public` constructor with zero call
//! sites (`references_to_in_file`/`references_to`, matching its
//! visibility) is just as genuinely dead as an unused method. A
//! VF-referenced class's constructor is covered by the same whole-class
//! exemption as its other public members, since VF's page-rendering
//! engine always calls a controller/extension's constructor implicitly.
//! `ForEachVar`/`CatchVar`/`SwitchBindingVar` are never candidates either
//! -- removing one would
//! break the surrounding loop/catch/switch syntax, so there's no safe
//! deletion to pair a diagnostic with. `Protected`/`Global` stay outside
//! candidacy too: `Global` is external managed-package API surface,
//! unprovable by local analysis regardless; `Protected` is a reasonable,
//! cheap future extension (same project-wide reference-counting `Public`
//! already needs, and VF/annotation exposure doesn't apply to it -- VF
//! markup can only bind to `public` members) not built yet.

use crate::file_id::FileId;
use crate::symbol::{Symbol, SymbolId, SymbolKind, Visibility};
use crate::BoundProgram;
use apex_syntax::ast::decl::{
    Annotation, ClassDecl, ConstructorDecl, FieldDecl, HasModifiers, MethodDecl, PropertyDecl,
    VarDeclarator,
};
use apex_syntax::ast::stmt::LocalVarDeclStmt;
use rowan::ast::AstNode;
use rowan::{TextRange, TextSize};

/// Annotation names (case-insensitive, matching Apex's own identifier
/// case-insensitivity) that grant a `public` member an external
/// invocation path this binder can't see any textual reference for:
/// Flow (`@InvocableMethod`/`@InvocableVariable`), Aura/LWC (`@AuraEnabled`
/// -- the two share the one annotation, there's no separate Aura-only
/// annotation), Visualforce remoting (`@RemoteAction`), and REST
/// (`@HttpGet`/`@HttpPost`/`@HttpPut`/`@HttpDelete`/`@HttpPatch`, always
/// paired with a class-level `@RestResource`). The `Http*`/`RestResource`
/// family is belt-and-suspenders more than load-bearing: those methods
/// are required to be `global` in real Apex, already excluded by
/// `is_dead_code_candidate_kind`'s visibility check, so this mainly
/// guards an unusual/invalid `public`-declared one rather than the
/// common case.
const PLATFORM_INVOCATION_ANNOTATIONS: &[&str] = &[
    "AuraEnabled",
    "InvocableMethod",
    "InvocableVariable",
    "RemoteAction",
    "HttpGet",
    "HttpPost",
    "HttpPut",
    "HttpDelete",
    "HttpPatch",
];

fn is_dead_code_candidate_kind(symbol: &Symbol) -> bool {
    match symbol.kind {
        SymbolKind::Method | SymbolKind::Field | SymbolKind::Property | SymbolKind::Constructor => {
            matches!(
                symbol.modifiers.visibility,
                Visibility::Private | Visibility::Public
            )
        }
        SymbolKind::LocalVar => true,
        _ => false,
    }
}

/// True for a private `Method` the Apex test-execution engine invokes
/// directly with zero textual call sites: `@isTest`/`@TestSetup`, or the
/// legacy `testMethod` modifier keyword (already modeled via
/// `ModifierSet::is_testmethod`). Must be checked before trusting a
/// `Method`'s zero-`references_to_in_file` count as proof of deadness --
/// unlike `@InvocableMethod`/`@AuraEnabled`/etc. (excluded structurally
/// for a *private* method, since those require `public`/`global`
/// visibility to actually work), Apex test methods are routinely
/// `private` and are still invoked directly by the platform's test
/// runner, never by other Apex.
///
/// `pub`, not `pub(crate)`: also the Run Test code lens's own
/// method-level detection (`apexls-server`'s `capabilities::run_test_lenses`),
/// re-exported from `apex-binder`'s crate root.
pub fn is_platform_invoked_test_method(program: &BoundProgram, symbol: &Symbol) -> bool {
    if symbol.kind != SymbolKind::Method {
        return false;
    }
    if symbol.modifiers.is_testmethod {
        return true;
    }
    let root = program.syntax(symbol.file);
    let Some(node) = symbol.ptr.to_node(&root) else {
        return false;
    };
    let Some(method) = MethodDecl::cast(node) else {
        return false;
    };
    method.annotations().any(|a| {
        a.name().is_some_and(|tok| {
            let text = tok.text();
            text.eq_ignore_ascii_case("isTest") || text.eq_ignore_ascii_case("testSetup")
        })
    })
}

/// `symbol`'s own `@`-annotations, straight off its declaration node via
/// `HasModifiers` (`apex_syntax`'s structural parse already exposes them;
/// nothing in `apex-binder` modeled annotations onto `Symbol`/`ModifierSet`
/// itself, and this is deliberately the only place that needs to). Shared
/// by every annotation-driven exemption below (`PLATFORM_INVOCATION_ANNOTATIONS`,
/// `@TestVisible`) rather than each re-deriving the same per-`SymbolKind`
/// node cast.
fn annotations_of(program: &BoundProgram, symbol: &Symbol) -> Vec<Annotation> {
    // A `LocalVar` (this module's other, by-far-most-common candidate
    // kind) can never carry an annotation at all -- checked before
    // touching the syntax tree so the common case skips `to_node`
    // entirely rather than just falling through the match below.
    if !matches!(
        symbol.kind,
        SymbolKind::Method
            | SymbolKind::Property
            | SymbolKind::Constructor
            | SymbolKind::Field
            | SymbolKind::Class
    ) {
        return Vec::new();
    }
    let root = program.syntax(symbol.file);
    let Some(node) = symbol.ptr.to_node(&root) else {
        return Vec::new();
    };
    let annotated = match symbol.kind {
        SymbolKind::Method => MethodDecl::cast(node).map(|m| m.annotations().collect::<Vec<_>>()),
        SymbolKind::Property => {
            PropertyDecl::cast(node).map(|p| p.annotations().collect::<Vec<_>>())
        }
        SymbolKind::Constructor => {
            ConstructorDecl::cast(node).map(|c| c.annotations().collect::<Vec<_>>())
        }
        SymbolKind::Field => {
            // `symbol.ptr` for a `Field` is the `VarDeclarator`, not the
            // whole `FieldDecl` -- annotations live on the parent
            // (shared across every comma-separated declarator).
            node.parent()
                .and_then(FieldDecl::cast)
                .map(|f| f.annotations().collect::<Vec<_>>())
        }
        SymbolKind::Class => ClassDecl::cast(node).map(|c| c.annotations().collect::<Vec<_>>()),
        _ => return Vec::new(),
    };
    annotated.unwrap_or_default()
}

fn has_annotation(program: &BoundProgram, symbol: &Symbol, name: &str) -> bool {
    annotations_of(program, symbol).into_iter().any(|a| {
        a.name()
            .is_some_and(|tok| tok.text().eq_ignore_ascii_case(name))
    })
}

/// True for a `public` `Method`/`Field`/`Property` carrying one of
/// `PLATFORM_INVOCATION_ANNOTATIONS` -- see this module's own doc
/// comment and that constant's for why each one matters.
pub(crate) fn has_platform_invocation_annotation(program: &BoundProgram, symbol: &Symbol) -> bool {
    annotations_of(program, symbol).into_iter().any(|a| {
        a.name().is_some_and(|tok| {
            let text = tok.text();
            PLATFORM_INVOCATION_ANNOTATIONS
                .iter()
                .any(|candidate| text.eq_ignore_ascii_case(candidate))
        })
    })
}

/// True for a `private`/`protected` `Method`/`Field`/`Property`/`Constructor`
/// carrying `@TestVisible` -- Apex's own way of granting an otherwise
/// purely file-scoped private member a second, real invocation channel:
/// any `@isTest` class in the org, in any file, not just this one.
/// `SymbolTable::is_visible_from` (the resolver's own visibility check,
/// consulted during member-access resolution) already treats such a
/// member as visible from anywhere for exactly this reason, so a real
/// cross-file `@TestVisible` call site does get linked into
/// `references_to` like any other reference -- but only once this
/// function also tells the caller *which* candidates need the
/// project-wide lookup instead of the file-scoped one. Unlike
/// `PLATFORM_INVOCATION_ANNOTATIONS` (which only ever matters for
/// `public`, and *exempts* the member outright since platform reflection
/// has no textual call site to find), `@TestVisible` specifically widens
/// a *private* candidate's own visibility -- so it must route through the
/// project-wide `references_to` instead of the file-scoped
/// `references_to_in_file`, exactly like a `public` candidate already
/// does, rather than being exempted from the reference check altogether.
fn is_test_visible(program: &BoundProgram, symbol: &Symbol) -> bool {
    has_annotation(program, symbol, "TestVisible")
}

/// True for a `Class` symbol carrying its own `@isTest` annotation --
/// the Run Test code lens's class-level detection
/// (`apexls-server`'s `capabilities::run_test_lenses`). Checks the
/// class's own annotation only, not an enclosing outer class's (an inner
/// class nested inside an `@isTest` outer class doesn't itself need the
/// annotation to hold test methods, but this function isn't answering
/// that question -- callers needing "is this method's test class"
/// already have `is_platform_invoked_test_method` for the method itself).
pub fn is_test_class(program: &BoundProgram, symbol: &Symbol) -> bool {
    symbol.kind == SymbolKind::Class && has_annotation(program, symbol, "isTest")
}

/// Walks `id` up to its outermost enclosing type (mirroring
/// `SymbolTable`'s own private `top_level_of`, reimplemented locally
/// here rather than exposing that as new crate-wide surface for a
/// three-line walk only this module needs).
fn top_level_container(program: &BoundProgram, id: SymbolId) -> SymbolId {
    let mut current = id;
    while let Some(parent) = program.symbols.get(current).container {
        current = parent;
    }
    current
}

/// True when `symbol`'s top-level containing class is one Visualforce
/// actually names as a `controller`/`extensions` (`BoundProgram::vf_referenced_classes`,
/// populated from real `.page` files -- see `apex_metadata::visualforce`).
/// Skips the *whole class*, not just individually-provably-used members:
/// working out exactly which method a page's embedded `{!expr}` markup
/// calls is a much harder, fuzzier problem than extracting the
/// `controller`/`extensions` attributes, and this project's own
/// convention (e.g. rename's refusal-over-guessing) is to stay
/// conservative rather than guess.
pub(crate) fn is_visualforce_referenced(program: &BoundProgram, symbol: &Symbol) -> bool {
    let Some(container) = symbol.container else {
        return false;
    };
    let top_level = top_level_container(program, container);
    let name = program.symbols.get(top_level).name.to_ascii_lowercase();
    program.vf_referenced_classes.contains(&name)
}

/// One provably-dead declaration in a file: everything both
/// `apexls-server`'s LSP wrappers and `apexls check` need, computed once
/// and shared between them. `visibility` exists specifically so
/// `kind_label` can distinguish "private method" from "public method" --
/// `kind` alone (`SymbolKind`) doesn't carry that.
pub struct DeadSymbol {
    pub kind: SymbolKind,
    pub visibility: Visibility,
    pub name: String,
    pub name_range: TextRange,
    pub deletion_range: TextRange,
}

/// Every declaration in `file` this binder can *prove* is dead. See this
/// module's own doc comment for the full scope (`private`/`public`
/// methods/fields/properties/constructors, plain locals; not
/// protected/global, not loop/catch/switch-binding variables) and why
/// each exclusion exists.
pub fn dead_symbols_in_file(program: &BoundProgram, file: FileId) -> Vec<DeadSymbol> {
    // Materialized once and shared across every candidate below, rather
    // than letting `compute_deletion_range` re-flatten the same file's
    // rope into a fresh `String` per candidate -- `symbols_of_file`
    // scopes everything else here to `file` already, and a file with
    // many dead candidates (locals especially) was re-paying that
    // O(file size) flattening once per candidate for no reason.
    let text = program.syntax(file).text().to_string();
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .map(|(local, s)| (SymbolId::new(file, local as u32), s))
        .filter(|(_, s)| is_dead_code_candidate_kind(s))
        .filter(|(_, s)| !is_platform_invoked_test_method(program, s))
        .filter(|(_, s)| {
            s.modifiers.visibility != Visibility::Public
                || (!has_platform_invocation_annotation(program, s)
                    && !is_visualforce_referenced(program, s))
        })
        .filter(|(id, s)| {
            // Private/local candidates are genuinely file-scoped (see the
            // module doc comment) -- the cheaper file-scoped lookup is
            // the *correct* one, not just faster. A `public` candidate
            // can be referenced from any file in the project, so it must
            // use the project-wide lookup instead; reusing the
            // file-scoped one here would silently miss real references
            // and produce false positives. A `@TestVisible` private/
            // protected candidate needs that same project-wide lookup
            // for the same reason -- see `is_test_visible`'s own doc
            // comment -- despite still being a `Private` candidate here.
            if s.modifiers.visibility == Visibility::Public || is_test_visible(program, s) {
                program.references_to(*id).next().is_none()
            } else {
                program.references_to_in_file(file, *id).next().is_none()
            }
        })
        .filter_map(|(_, s)| {
            compute_deletion_range(program, &text, s).map(|deletion_range| DeadSymbol {
                kind: s.kind,
                visibility: s.modifiers.visibility,
                name: s.name.to_string(),
                name_range: s.name_range,
                deletion_range,
            })
        })
        .collect()
}

/// The exact text range to delete to remove `symbol` cleanly. Not simply
/// `symbol.ptr`'s range: that only covers the whole declaration for
/// `Method`/`Property` (see `Symbol::ptr`'s own doc comment) -- for
/// `Field` it's just the one `VarDeclarator`, and for `LocalVar` just the
/// `Name` token, since both `FieldDecl`/`LocalVarDeclStmt` support
/// multiple comma-separated declarators (`private Integer x, y;`) and
/// `Symbol` is one-per-declarator. "Delete this one" therefore means
/// either the whole declaration (it's the only declarator -- and, for a
/// `Field`, this naturally includes any leading doc comment/modifiers,
/// since `HasDocComment::doc_comment_token` finds the doc comment by
/// walking the very node whose range this returns) or just this
/// declarator plus its neighboring comma (siblings exist, so the shared
/// doc comment/modifiers must stay untouched -- they still document the
/// remaining declarators).
///
/// The whole-declaration case goes through `line_aligned_deletion_range`
/// rather than trusting the node's own raw boundaries directly: this
/// parser's trivia attachment at a statement/declaration's edges turned
/// out not to be trustworthy for this purpose empirically (verified via
/// this module's own splice tests) -- a `LocalVarDeclStmt`'s range, for
/// one, excludes its own leading indentation but can still include
/// trailing whitespace past its own line. Rather than chase that per-kind,
/// `line_aligned_deletion_range` sidesteps it entirely by computing
/// purely from the raw source text once a real-content anchor is found.
fn compute_deletion_range(
    program: &BoundProgram,
    text: &str,
    symbol: &Symbol,
) -> Option<TextRange> {
    let root = program.syntax(symbol.file);
    let node = symbol.ptr.to_node(&root)?;
    match symbol.kind {
        SymbolKind::Method | SymbolKind::Property | SymbolKind::Constructor => {
            Some(line_aligned_deletion_range(text, node.text_range()))
        }
        SymbolKind::Field => {
            let declarator = VarDeclarator::cast(node)?;
            let field_decl = FieldDecl::cast(declarator.syntax().parent()?)?;
            let siblings: Vec<VarDeclarator> = field_decl.declarators().collect();
            if siblings.len() == 1 {
                Some(line_aligned_deletion_range(
                    text,
                    field_decl.syntax().text_range(),
                ))
            } else {
                deletion_range_for_declarator(&declarator, &siblings)
            }
        }
        SymbolKind::LocalVar => {
            let declarator = VarDeclarator::cast(node.parent()?)?;
            let stmt = LocalVarDeclStmt::cast(declarator.syntax().parent()?)?;
            let siblings: Vec<VarDeclarator> = stmt.declarators().collect();
            if siblings.len() == 1 {
                Some(line_aligned_deletion_range(
                    text,
                    stmt.syntax().text_range(),
                ))
            } else {
                deletion_range_for_declarator(&declarator, &siblings)
            }
        }
        _ => None,
    }
}

/// Shared by the `Field`/`LocalVar` arms above, for the case that has
/// *other* declarators to leave untouched: `declarator`'s own range plus
/// whichever neighboring comma separates it from the rest of `siblings`
/// (every declarator of the same `FieldDecl`/`LocalVarDeclStmt`, in
/// source order) -- the *following* comma if this isn't the last
/// declarator, otherwise the *preceding* one -- so deleting the middle
/// of `x, y, z` leaves `x, z`, not `x, , z` or a trailing `x, y,`. Not
/// line-aligned: a non-last declarator never owns its own line, so the
/// line-based reasoning `line_aligned_deletion_range` uses doesn't apply
/// here, only mid-line comma-splicing does.
fn deletion_range_for_declarator(
    declarator: &VarDeclarator,
    siblings: &[VarDeclarator],
) -> Option<TextRange> {
    let target = declarator.syntax().text_range();
    let index = siblings
        .iter()
        .position(|d| d.syntax().text_range() == target)?;
    if index + 1 < siblings.len() {
        Some(TextRange::new(
            siblings[index].syntax().text_range().start(),
            siblings[index + 1].syntax().text_range().start(),
        ))
    } else {
        Some(TextRange::new(
            siblings[index - 1].syntax().text_range().end(),
            siblings[index].syntax().text_range().end(),
        ))
    }
}

/// Computes a deletion range that removes exactly the whole source
/// line(s) `range`'s *real* content occupies -- neither a leftover blank
/// line nor an orphaned indent -- without trusting `range`'s own
/// leading/trailing edges to already be whitespace-free or line-aligned
/// (this parser's trivia attachment varies by node kind: a statement's
/// range can exclude its own leading indentation yet still include
/// trailing whitespace reaching into the next line, as `compute_deletion_range`'s
/// doc comment explains). Three steps, all directly on the raw source
/// text rather than the syntax tree: (1) trim `range` down to its real
/// (non-whitespace) content on both edges, discarding whatever
/// whitespace it happened to include; (2) if only spaces/tabs sit
/// between that content's start and the start of its own line, extend
/// the start back to the line's start, so the declaration's own
/// indentation goes with it; (3) extend the end forward past exactly one
/// trailing line terminator (and any same-line trailing whitespace
/// before it), so the line itself -- not just its content -- disappears.
/// Deliberately never reaches into the *previous* line's own trailing
/// newline (that would merge the previous line into whatever now follows
/// instead of just closing this line's own gap).
fn line_aligned_deletion_range(text: &str, range: TextRange) -> TextRange {
    let bytes = text.as_bytes();
    let mut start = usize::from(range.start());
    let mut end = usize::from(range.end());
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }

    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if text.as_bytes()[line_start..start]
        .iter()
        .all(|&b| b == b' ' || b == b'\t')
    {
        start = line_start;
    }

    let mut i = end;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\r' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }
    TextRange::new(TextSize::from(start as u32), TextSize::from(i as u32))
}

pub fn kind_label(kind: SymbolKind, visibility: Visibility) -> &'static str {
    match (kind, visibility) {
        (SymbolKind::Method, Visibility::Public) => "public method",
        (SymbolKind::Field, Visibility::Public) => "public field",
        (SymbolKind::Property, Visibility::Public) => "public property",
        (SymbolKind::Constructor, Visibility::Public) => "public constructor",
        (SymbolKind::Method, _) => "private method",
        (SymbolKind::Field, _) => "private field",
        (SymbolKind::Property, _) => "private property",
        (SymbolKind::Constructor, _) => "private constructor",
        (SymbolKind::LocalVar, _) => "local variable",
        _ => "declaration",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Writes `src` as `Foo.cls` (plus any `extra_files`) under a fresh,
    /// uniquely-named temp directory (`test_name` keeps directories from
    /// colliding across tests running in parallel in the same process --
    /// matching this crate's other `tests/*.rs`' own `write_fixture_dir`
    /// convention).
    fn write_fixture(
        test_name: &str,
        src: &str,
        extra_files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apex-binder-dead-code-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Foo.cls"), src).unwrap();
        for (name, content) in extra_files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    fn dead_symbols(test_name: &str, src: &str) -> (BoundProgram, FileId, Vec<DeadSymbol>) {
        dead_symbols_with_extra_files(test_name, src, &[])
    }

    fn dead_symbols_with_extra_files(
        test_name: &str,
        src: &str,
        extra_files: &[(&str, &str)],
    ) -> (BoundProgram, FileId, Vec<DeadSymbol>) {
        let dir = write_fixture(test_name, src, extra_files);
        let program = BoundProgram::from_files(&dir);
        let file = program.file_id(&dir.join("Foo.cls")).unwrap();
        let dead = dead_symbols_in_file(&program, file);
        std::fs::remove_dir_all(&dir).ok();
        (program, file, dead)
    }

    fn dead_names(test_name: &str, src: &str) -> Vec<String> {
        dead_symbols(test_name, src)
            .2
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    fn apply_deletion(text: &str, range: TextRange) -> String {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        format!("{}{}", &text[..start], &text[end..])
    }

    /// A second file that calls `Foo::run()` -- used as the harness across
    /// most of the tests below so `run` itself (and whatever it in turn
    /// calls) has a genuine, provable caller. Deliberately a real
    /// cross-file reference rather than any kind- or name-based exemption:
    /// `run` is `public` and a candidate like any other member, it's just
    /// actually used.
    const CALLER: &str = "public class Caller {\n    public void go() { new Foo().run(); }\n}\n";

    /// Every `kind_label` arm actually used by `apexls check`'s report --
    /// most existing fixtures below only ever exercise `Method`/`Field`
    /// dead symbols, so `Property`/`Constructor`'s private-visibility
    /// arms, and their own public arms in `Property`'s case, had never
    /// been called.
    #[test]
    fn kind_label_covers_every_kind_and_visibility_pairing() {
        assert_eq!(
            kind_label(SymbolKind::Property, Visibility::Public),
            "public property"
        );
        assert_eq!(
            kind_label(SymbolKind::Property, Visibility::Private),
            "private property"
        );
        assert_eq!(
            kind_label(SymbolKind::Constructor, Visibility::Private),
            "private constructor"
        );
        // Any kind/visibility pairing this report never actually
        // produces (a local variable has no meaningful visibility, for
        // instance) still degrades to a generic label rather than
        // panicking.
        assert_eq!(
            kind_label(SymbolKind::Interface, Visibility::Public),
            "declaration"
        );
    }

    #[test]
    fn unused_private_method_is_flagged() {
        let src = "public class Foo {\n    private void helper() { }\n}\n";
        assert_eq!(dead_names("unused-private-method", src), vec!["helper"]);
    }

    #[test]
    fn used_private_method_is_not_flagged() {
        let src = "public class Foo {\n    private void helper() { }\n    public void run() { helper(); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("used-private-method", src, &[("Caller.cls", CALLER)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unused_public_method_is_flagged() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        assert_eq!(dead_names("unused-public-method", src), vec!["helper"]);
    }

    #[test]
    fn public_method_referenced_only_from_another_file_is_not_flagged() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        let caller = "public class Caller {\n    public void run() { new Foo().helper(); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("public-cross-file-ref", src, &[("Caller.cls", caller)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn aura_enabled_public_method_is_not_flagged() {
        let src = "public class Foo {\n    @AuraEnabled\n    public static void helper() { }\n}\n";
        assert!(dead_names("aura-enabled", src).is_empty());
    }

    #[test]
    fn aura_enabled_public_property_is_not_flagged() {
        let src = "public class Foo {\n    @AuraEnabled\n    public Integer x { get; set; }\n}\n";
        assert!(dead_names("aura-enabled-property", src).is_empty());
    }

    #[test]
    fn invocable_method_is_not_flagged() {
        let src =
            "public class Foo {\n    @InvocableMethod\n    public static void helper() { }\n}\n";
        assert!(dead_names("invocable-method", src).is_empty());
    }

    #[test]
    fn invocable_variable_is_not_flagged() {
        let src = "public class Foo {\n    @InvocableVariable\n    public Integer x;\n}\n";
        assert!(dead_names("invocable-variable", src).is_empty());
    }

    #[test]
    fn remote_action_method_is_not_flagged() {
        let src = "public class Foo {\n    @RemoteAction\n    public static void helper() { }\n}\n";
        assert!(dead_names("remote-action", src).is_empty());
    }

    #[test]
    fn visualforce_referenced_class_has_its_public_members_skipped() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        let page = "<apex:page controller=\"Foo\">Hello</apex:page>";
        let (_, _, dead) =
            dead_symbols_with_extra_files("vf-referenced", src, &[("Foo.page", page)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn protected_and_global_members_are_never_flagged() {
        let src =
            "public class Foo {\n    protected void helper() { }\n    global void other() { }\n}\n";
        assert!(dead_names("protected-global", src).is_empty());
    }

    #[test]
    fn public_method_with_zero_callers_and_no_vf_or_annotation_is_flagged() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        assert_eq!(dead_names("public-plain", src), vec!["helper"]);
    }

    #[test]
    fn unused_private_constructor_is_flagged() {
        // Unlike a method, this looks like the "block external
        // instantiation" idiom -- but nothing about it is actually
        // provable as intentional from zero call sites alone, and the
        // same idiom exists in reverse (a genuinely-forgotten overload
        // nobody calls). Treated like any other private member: zero
        // file-scoped references means dead.
        let src = "public class Foo {\n    private Foo() { }\n}\n";
        assert_eq!(dead_names("unused-private-ctor", src), vec!["Foo"]);
    }

    #[test]
    fn constructor_called_within_the_file_is_not_flagged() {
        let src = "public class Foo {\n    private Foo() { }\n    public static Foo make() { return new Foo(); }\n}\n";
        let caller = "public class Caller {\n    public void go() { Foo.make(); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("used-private-ctor", src, &[("Caller.cls", caller)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unused_public_constructor_is_flagged() {
        let src = "public class Foo {\n    public Foo() { }\n}\n";
        assert_eq!(dead_names("unused-public-ctor", src), vec!["Foo"]);
    }

    #[test]
    fn public_constructor_called_from_another_file_is_not_flagged() {
        let src = "public class Foo {\n    public Foo() { }\n}\n";
        let caller = "public class Caller {\n    public void go() { new Foo(); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("used-public-ctor", src, &[("Caller.cls", caller)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vf_referenced_class_constructor_is_not_flagged() {
        // VF's page-rendering engine always calls a controller/extension's
        // constructor implicitly -- covered by the same whole-class
        // exemption as any other public member.
        let src = "public class Foo {\n    public Foo() { }\n}\n";
        let page = "<apex:page controller=\"Foo\">Hello</apex:page>";
        let (_, _, dead) =
            dead_symbols_with_extra_files("vf-referenced-ctor", src, &[("Foo.page", page)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn is_test_annotated_private_method_is_not_flagged() {
        let src =
            "public class Foo {\n    @isTest\n    private static void testSomething() { }\n}\n";
        assert!(dead_names("isTest-annotation", src).is_empty());
    }

    #[test]
    fn legacy_testmethod_modifier_is_not_flagged() {
        let src = "public class Foo {\n    private static testMethod void testSomething() { }\n}\n";
        assert!(dead_names("legacy-testmethod", src).is_empty());
    }

    #[test]
    fn is_test_class_true_for_istest_annotated_class() {
        let src = "@isTest\nprivate class Foo {\n    @isTest\n    static void testSomething() { }\n}\n";
        let (program, file, _) = dead_symbols("is-test-class-true", src);
        let (_, class) = program
            .symbols
            .iter()
            .find(|(_, s)| s.file == file && s.kind == SymbolKind::Class)
            .expect("class symbol");
        assert!(is_test_class(&program, class));
    }

    #[test]
    fn is_test_class_false_for_ordinary_class() {
        let src = "public class Foo {\n    public void doWork() { }\n}\n";
        let (program, file, _) = dead_symbols("is-test-class-false", src);
        let (_, class) = program
            .symbols
            .iter()
            .find(|(_, s)| s.file == file && s.kind == SymbolKind::Class)
            .expect("class symbol");
        assert!(!is_test_class(&program, class));
    }

    #[test]
    fn unused_foreach_variable_is_not_flagged() {
        let src = "public class Foo {\n    public void run(List<Integer> xs) {\n        for (Integer x : xs) { }\n    }\n}\n";
        let caller = "public class Caller {\n    public void go() { new Foo().run(new List<Integer>()); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("unused-foreach-var", src, &[("Caller.cls", caller)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unused_private_field_with_doc_comment_deletes_the_whole_declaration() {
        let src = "public class Foo {\n    /** unused */\n    private Integer x;\n    public void run() { }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("field-with-doc-comment", src, &[("Caller.cls", CALLER)]);
        assert_eq!(dead.len(), 1);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(after, "public class Foo {\n    public void run() { }\n}\n");
    }

    #[test]
    fn unused_middle_declarator_among_siblings_deletes_only_that_one() {
        let src = "public class Foo {\n    private Integer x, y, z;\n    public void run() { System.debug(x); System.debug(z); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("middle-declarator", src, &[("Caller.cls", CALLER)]);
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["y"]
        );
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    private Integer x, z;\n    public void run() { System.debug(x); System.debug(z); }\n}\n"
        );
    }

    #[test]
    fn unused_last_declarator_deletes_the_preceding_comma() {
        let src = "public class Foo {\n    private Integer x, y;\n    public void run() { System.debug(x); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("last-declarator", src, &[("Caller.cls", CALLER)]);
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["y"]
        );
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    private Integer x;\n    public void run() { System.debug(x); }\n}\n"
        );
    }

    /// The `LocalVar` counterpart of `unused_middle_declarator_among_siblings_deletes_only_that_one`
    /// -- `deletion_range_for_declarator`'s `SymbolKind::LocalVar` call
    /// site (as opposed to its `Field` one, the only one any other test
    /// here exercises) had never actually run.
    #[test]
    fn unused_middle_local_declarator_among_siblings_deletes_only_that_one() {
        let src = "public class Foo {\n    public void run() {\n        Integer x = 0, y = 1, z = 2;\n        System.debug(x);\n        System.debug(z);\n    }\n}\n";
        let (_, _, dead) = dead_symbols_with_extra_files(
            "middle-local-declarator",
            src,
            &[("Caller.cls", CALLER)],
        );
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["y"]
        );
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    public void run() {\n        Integer x = 0, z = 2;\n        System.debug(x);\n        System.debug(z);\n    }\n}\n"
        );
    }

    #[test]
    fn unused_local_variable_is_flagged_and_deletes_cleanly() {
        let src = "public class Foo {\n    public void run() {\n        Integer unused = 5;\n        System.debug('hi');\n    }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("unused-local", src, &[("Caller.cls", CALLER)]);
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["unused"]
        );
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    public void run() {\n        System.debug('hi');\n    }\n}\n"
        );
    }

    /// The boundary case `compute_deletion_range`'s own doc comment flags
    /// as needing an empirical check: deleting the *last* statement in a
    /// method body, immediately before the closing `}`, must not leave a
    /// blank line behind.
    #[test]
    fn unused_local_variable_as_the_last_statement_leaves_no_blank_line() {
        let src = "public class Foo {\n    public void run() {\n        System.debug('hi');\n        Integer unused = 5;\n    }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("unused-local-last-stmt", src, &[("Caller.cls", CALLER)]);
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["unused"]
        );
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    public void run() {\n        System.debug('hi');\n    }\n}\n"
        );
    }

    /// The reported bug this fixes: a local bound only inside a dynamic-
    /// SOQL string (`:nameVar`, resolved via `crate::resolve::bind_dynamic_soql_binds`)
    /// used to have no reference recorded for it at all -- `ReferenceTable`
    /// only ever sees real AST-node references, and string *content* was
    /// never scanned for anything. `q` itself is traced one hop back from
    /// `Database.query(q)` to its own literal assignment.
    #[test]
    fn a_local_bound_only_in_a_dynamic_soql_string_reaching_database_query_is_not_flagged() {
        let src = "public class Foo {\n    public void run() {\n        String nameVar = 'Acme';\n        String q = 'SELECT Id FROM Account WHERE Name = :nameVar';\n        Database.query(q);\n    }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("dynamic-soql-bind-used", src, &[("Caller.cls", CALLER)]);
        assert!(
            dead.iter().all(|d| d.name != "nameVar"),
            "nameVar is bound in a dynamic SOQL string reaching Database.query, it must not be flagged dead: {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    /// A local that looks similarly named but is never actually bound
    /// anywhere (no `:otherVar` in any string) must still be flagged --
    /// the fix must not become "any local in a method that calls
    /// Database.query is exempt."
    #[test]
    fn a_local_not_bound_in_any_dynamic_soql_string_is_still_flagged() {
        let src = "public class Foo {\n    public void run() {\n        Integer otherVar = 5;\n        String q = 'SELECT Id FROM Account';\n        Database.query(q);\n    }\n}\n";
        let (_, _, dead) = dead_symbols_with_extra_files(
            "dynamic-soql-unrelated-local",
            src,
            &[("Caller.cls", CALLER)],
        );
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["otherVar"]
        );
    }

    #[test]
    fn test_visible_private_static_method_called_from_another_file_is_not_flagged() {
        let src = "public class Foo {\n    @TestVisible\n    private static void helper() { }\n}\n";
        let caller = "@isTest\nprivate class Caller {\n    @isTest\n    static void go() { Foo.helper(); }\n}\n";
        let (_, _, dead) =
            dead_symbols_with_extra_files("testvisible-cross-file", src, &[("Caller.cls", caller)]);
        assert!(
            dead.is_empty(),
            "expected no dead symbols, got {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    /// Regression test for a real user report against the NPSP corpus
    /// (`UTIL_CurrencyCache`): a singleton accessor's declared return
    /// type is an interface, so every call chained off it -- `instance().greet()`
    /// here -- resolves against the interface's own abstract `greet`
    /// declaration. Without `crate::resolve::expand_dynamic_dispatch`
    /// widening that resolution to also include `Foo.greet` (the
    /// concrete implementation actually reached at runtime), `Foo.greet`
    /// would show zero direct references and be misreported as dead.
    #[test]
    fn a_method_reached_only_through_interface_typed_dispatch_is_not_flagged() {
        let src = "public class Foo implements Foo.Greeter { \
             public static Greeter instance() { return new Foo(); } \
             public String greet() { return 'hi'; } \
             public interface Greeter { String greet(); } \
             public void run() { String s = instance().greet(); } \
         }\n";
        let (_, _, dead) = dead_symbols_with_extra_files(
            "interface-dispatch-not-flagged",
            src,
            &[("Caller.cls", CALLER)],
        );
        assert!(
            !dead.iter().any(|d| d.name == "greet"),
            "greet() implements Greeter.greet and is reached only via interface-typed dispatch \
             -- must not be flagged dead: {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    /// The same dynamic-dispatch widening applies to a plain `virtual`/
    /// `override` class method pair, not just interfaces: a call through
    /// a base-typed reference (`Foo b = new Derived(); b.greet();`)
    /// resolves against `Foo.greet`, but the object referenced at runtime
    /// is a `Derived`, so `Derived.greet` (the override) is just as
    /// reachable and must not be flagged dead despite never being called
    /// through its own concrete type.
    #[test]
    fn an_override_reached_only_through_base_typed_dispatch_is_not_flagged() {
        let src = "public virtual class Foo { \
             public virtual void greet() { } \
             public class Derived extends Foo { \
                 public override void greet() { System.debug('hi'); } \
             } \
             public void run() { Foo b = new Derived(); b.greet(); } \
         }\n";
        let (_, _, dead) = dead_symbols_with_extra_files(
            "override-dispatch-not-flagged",
            src,
            &[("Caller.cls", CALLER)],
        );
        assert!(
            !dead.iter().any(|d| d.name == "greet"),
            "Derived.greet overrides Foo.greet and is reached only via base-typed dispatch -- \
             must not be flagged dead: {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    /// The dispatch-widening exemption above must stay precise -- it
    /// should never blanket-exempt every method on a class that happens
    /// to implement some interface, only the ones actually named on it.
    #[test]
    fn an_unrelated_method_on_an_interface_implementing_class_is_still_flagged() {
        let src = "public class Foo implements Foo.Greeter { \
             public String greet() { return 'hi'; } \
             public interface Greeter { String greet(); } \
             private void unrelatedHelper() { } \
             public void run() { greet(); } \
         }\n";
        let (_, _, dead) = dead_symbols_with_extra_files(
            "interface-implementation-unrelated-method-still-flagged",
            src,
            &[("Caller.cls", CALLER)],
        );
        assert_eq!(
            dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["unrelatedHelper"]
        );
    }

    /// Real-corpus smoke test: `dead_symbols_in_file` must run to
    /// completion, without panicking, across every file in the real NPSP
    /// checkout (the scale that's actually exposed real bugs in this
    /// codebase before), and it must stay conservative in aggregate -- a
    /// wildly overzealous detector flagging a large share of candidates
    /// would be a real regression worth catching here rather than
    /// discovering it live against a real project.
    #[test]
    fn npsp_corpus_dead_symbol_sweep_stays_conservative() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus")
            .join("npsp");
        let Ok(root) = root.canonicalize() else {
            eprintln!("skipping: real NPSP corpus checkout not present at {root:?}");
            return;
        };
        let program = BoundProgram::from_files(&root);
        let candidate_count = program
            .symbols
            .iter()
            .filter(|(_, s)| is_dead_code_candidate_kind(s))
            .count();
        let files: HashSet<FileId> = program.symbols.iter().map(|(_, s)| s.file).collect();
        let dead_count: usize = files
            .iter()
            .map(|&file| dead_symbols_in_file(&program, file).len())
            .sum();
        assert!(
            candidate_count > 0,
            "expected at least some eligible methods/fields/properties/locals in a real corpus this size"
        );
        let ratio = dead_count as f64 / candidate_count as f64;
        assert!(
            ratio < 0.5,
            "flagged {dead_count}/{candidate_count} ({:.0}%) of eligible members/locals as \
             dead -- suspiciously high, likely an overzealous detector rather than a real finding",
            ratio * 100.0
        );
    }
}
