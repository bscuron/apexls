//! Pure call-graph queries backing `textDocument/prepareCallHierarchy`/
//! `callHierarchy/incomingCalls`/`callHierarchy/outgoingCalls`. Computed
//! entirely on demand per request -- LSP's call-hierarchy protocol is
//! itself lazy/expand-on-demand (prepare, then incoming/outgoing calls
//! per node the user drills into), so there's no need for a
//! precomputed whole-project call graph; every query here reuses the
//! same resolution/reference-index primitives `references`/`rename`
//! already do (`BoundProgram::references_to`, `Self::enclosing_callable`,
//! `Self::call_sites_in_range`).
//!
//! Ambiguous resolution (`Resolution::Candidates`) is handled the same
//! "don't guess, but don't silently drop it either" way `references`/
//! `documentHighlight` already do, not rename's stricter "refuse
//! outright": an ambiguous call site contributes an edge to *every*
//! candidate rather than silently picking one. This is the honest,
//! unavoidable consequence of not having a full type system --
//! `crate::resolve::narrow_by_overload`'s own doc comment covers exactly
//! which calls stay ambiguous: an argument whose type comes from an
//! unmodeled stdlib call, a system type outside `crate::conversions`'s
//! curated set, or a genuine tie between equally-specific overloads.

use crate::symbol::{SymbolId, SymbolKind};
use crate::{BoundProgram, Resolution, SyntaxPtr};

/// Whether `kind` is a kind `callHierarchy` can represent at all -- a
/// `CallHierarchyItem` is always a callable, never a field/local/type.
pub fn is_callable(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Method | SymbolKind::Constructor)
}

/// One entry in an `incomingCalls` result: `from` calls the requested
/// target at every site in `call_sites` -- LSP collapses every call site
/// within the same caller into one `CallHierarchyIncomingCall`, not one
/// per site, which is exactly why this groups by caller instead of
/// returning a flat list.
pub struct IncomingCall {
    pub from: SymbolId,
    pub call_sites: Vec<SyntaxPtr>,
}

/// One entry in an `outgoingCalls` result: the requested caller's body
/// calls `to` at every site in `call_sites`.
pub struct OutgoingCall {
    pub to: SymbolId,
    pub call_sites: Vec<SyntaxPtr>,
}

/// Every distinct caller referencing `target` (project-wide, via
/// `BoundProgram::references_to`), each paired with every site in that
/// caller calling it. A call site with no enclosing method/constructor
/// at all (a field/property initializer -- Apex allows a call there,
/// e.g. `private static Integer x = computeSomething();`) is silently
/// excluded: there's no honest `CallHierarchyItem` to report such a site
/// *from*.
pub fn incoming_calls(program: &BoundProgram, target: SymbolId) -> Vec<IncomingCall> {
    let mut by_caller: Vec<(SymbolId, Vec<SyntaxPtr>)> = Vec::new();
    for ptr in program.references_to(target) {
        let Some(caller) = program.enclosing_callable(ptr.file(), ptr.range().start()) else {
            continue;
        };
        match by_caller.iter_mut().find(|(id, _)| *id == caller) {
            Some((_, sites)) => sites.push(ptr),
            None => by_caller.push((caller, vec![ptr])),
        }
    }
    by_caller
        .into_iter()
        .map(|(from, call_sites)| IncomingCall { from, call_sites })
        .collect()
}

/// Every distinct callable `caller`'s own declaration body calls (via
/// `BoundProgram::call_sites_in_range`), each paired with every site
/// calling it. An ambiguous call site fans out to every candidate --
/// see this module's own doc comment.
pub fn outgoing_calls(program: &BoundProgram, caller: SymbolId) -> Vec<OutgoingCall> {
    let symbol = program.symbols.get(caller);
    let mut by_callee: Vec<(SymbolId, Vec<SyntaxPtr>)> = Vec::new();
    for (ptr, resolution) in program.call_sites_in_range(symbol.file, symbol.ptr.range()) {
        let targets: Vec<SymbolId> = match resolution {
            Resolution::Resolved(id) => vec![id],
            Resolution::Candidates(ids) => ids,
            _ => continue,
        };
        for id in targets {
            match by_callee.iter_mut().find(|(existing, _)| *existing == id) {
                Some((_, sites)) => sites.push(ptr),
                None => by_callee.push((id, vec![ptr])),
            }
        }
    }
    by_callee
        .into_iter()
        .map(|(to, call_sites)| OutgoingCall { to, call_sites })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoundProgram;

    fn write_fixture(test_name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apex-binder-call-hierarchy-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    fn find_method(program: &BoundProgram, name: &str) -> SymbolId {
        program
            .symbols
            .iter()
            .find(|(_, s)| s.name == name && is_callable(s.kind))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no callable named {name} found"))
    }

    #[test]
    fn incoming_calls_finds_the_enclosing_caller() {
        let src = "public class Foo {\n    public void helper() { }\n    public void run() { helper(); }\n}\n";
        let dir = write_fixture("incoming-basic", &[("Foo.cls", src)]);
        let program = BoundProgram::from_files(&dir);
        std::fs::remove_dir_all(&dir).ok();

        let helper = find_method(&program, "helper");
        let incoming = incoming_calls(&program, helper);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].from, find_method(&program, "run"));
        assert_eq!(incoming[0].call_sites.len(), 1);
    }

    #[test]
    fn incoming_calls_crosses_files_and_excludes_a_field_initializer_call_site() {
        let src = "public class Foo {\n    public static Integer x = compute();\n    public static Integer compute() { return 1; }\n}\n";
        let caller = "public class Caller {\n    public void go() { Foo.compute(); }\n}\n";
        let dir = write_fixture("incoming-cross-file", &[("Foo.cls", src), ("Caller.cls", caller)]);
        let program = BoundProgram::from_files(&dir);
        std::fs::remove_dir_all(&dir).ok();

        let compute = find_method(&program, "compute");
        let incoming = incoming_calls(&program, compute);
        // Two real references (the field initializer and `Caller::go`),
        // but only one has an enclosing callable to report it from.
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].from, find_method(&program, "go"));
    }

    #[test]
    fn outgoing_calls_finds_every_call_in_the_body_including_a_constructor() {
        let src = "public class Foo {\n    public Foo() { }\n    public void a() { }\n    public void b() { }\n    public void run() { a(); b(); new Foo(); }\n}\n";
        let dir = write_fixture("outgoing-basic", &[("Foo.cls", src)]);
        let program = BoundProgram::from_files(&dir);
        std::fs::remove_dir_all(&dir).ok();

        let run = find_method(&program, "run");
        let mut outgoing = outgoing_calls(&program, run);
        outgoing.sort_by_key(|c| program.symbols.get(c.to).name.to_string());
        let names: Vec<String> = outgoing.iter().map(|c| program.symbols.get(c.to).name.to_string()).collect();
        assert_eq!(names, vec!["Foo", "a", "b"]);
    }

    /// Real-corpus smoke test: `incoming_calls`/`outgoing_calls` must run
    /// to completion, without panicking, for every `Method`/`Constructor`
    /// in the real NPSP checkout -- the scale that's actually exposed
    /// real bugs in this codebase before.
    #[test]
    fn npsp_corpus_call_hierarchy_sweep_does_not_panic() {
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
        let callables: Vec<SymbolId> = program
            .symbols
            .iter()
            .filter(|(_, s)| is_callable(s.kind))
            .map(|(id, _)| id)
            .collect();
        assert!(callables.len() > 100, "expected many callables in a real corpus this size");
        for id in callables {
            incoming_calls(&program, id);
            outgoing_calls(&program, id);
        }
    }

    #[test]
    fn outgoing_calls_fans_out_an_ambiguous_overload_to_every_candidate() {
        // `bar`'s argument type is unknown (`compute()` isn't declared
        // anywhere in this project, so it's `Unresolved`), so neither
        // overload can be ruled out by argument type -- genuine ambiguity,
        // not resolvable without a real type system.
        let src = "public class Foo {\n    public void bar(Integer x) { }\n    public void bar(String x) { }\n    public void run() { bar(compute()); }\n}\n";
        let dir = write_fixture("outgoing-ambiguous", &[("Foo.cls", src)]);
        let program = BoundProgram::from_files(&dir);
        std::fs::remove_dir_all(&dir).ok();

        let run = find_method(&program, "run");
        let outgoing = outgoing_calls(&program, run);
        assert_eq!(outgoing.len(), 2, "expected both `bar` overloads as candidates");
        assert!(outgoing.iter().all(|c| program.symbols.get(c.to).name == "bar"));
    }
}
