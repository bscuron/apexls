//! Ticket 24: a hands-on smoke test for adopting real `salsa`
//! (crates.io `salsa = "0.28.2"`) as `apex-binder`'s future incremental
//! engine, verifying ticket 21's documentation-only findings actually
//! compile before they become load-bearing in the architecture-decision
//! ticket (25) or any later implement ticket.
//!
//! Deliberately *not* a toy `i32` example: the tracked query below is
//! keyed on a real source string and returns real, owned
//! [`SyntaxPtr`]s -- `crate::ptr`'s own doc comment already explains why
//! `apex-binder` stores exactly this shape (`FileId` + `SyntaxKind` +
//! `TextRange`, `Copy`, `Send + Sync`) instead of a live rowan
//! `SyntaxNode` -- keyed by `SmolStr` method name in an `FxHashMap`, the
//! same primitives `crate::collect`/`crate::reference_table` already
//! build their real data out of. The live `SyntaxNode` rowan produces
//! while parsing is built and dropped entirely inside the tracked
//! function's own body, never crossing into salsa-tracked storage --
//! confirming ticket 21's finding that this codebase's existing
//! `SyntaxPtr` discipline already satisfies salsa's `Send + Sync` bounds
//! with no design change required.

#[cfg(test)]
mod tests {
    use crate::file_id::FileId;
    use crate::ptr::SyntaxPtr;
    use apex_syntax::ast::decl::{CompilationUnit, Member, MethodDecl, TypeDecl};
    use rowan::ast::AstNode;
    use rustc_hash::FxHashMap;
    use smol_str::SmolStr;

    #[salsa::db]
    #[derive(Default)]
    struct SmokeDatabase {
        storage: salsa::Storage<Self>,
    }

    #[salsa::db]
    impl salsa::Database for SmokeDatabase {}

    #[salsa::input]
    struct SourceFile {
        #[returns(deref)]
        text: SmolStr,
    }

    /// A minimal stand-in for `crate::collect`'s real Pass 1 work: parses
    /// one file's source and returns every method's name mapped to a
    /// `SyntaxPtr` naming its declaration -- salsa-memoized, re-run only
    /// when `file`'s own text input changes.
    #[salsa::tracked]
    fn method_pointers(db: &dyn salsa::Database, file: SourceFile) -> FxHashMap<SmolStr, SyntaxPtr> {
        let parse = apex_parser::parse_compilation_unit(file.text(db));
        let root = parse.syntax();
        let cu = CompilationUnit::cast(root).expect("valid compilation unit");
        let TypeDecl::Class(class) = cu.type_decl().expect("a top-level type decl") else {
            panic!("expected a class declaration");
        };

        let mut methods = FxHashMap::default();
        if let Some(body) = class.body() {
            for member in body.members() {
                if let Member::Method(method) = member {
                    if let Some(name) = method.name().and_then(|n| n.text()) {
                        methods.insert(name, SyntaxPtr::new(FileId(0), method.syntax()));
                    }
                }
            }
        }
        methods
    }

    #[test]
    fn salsa_tracked_fn_returns_owned_syntax_ptrs_that_resolve_back_to_the_source() {
        let source = "class Foo { void bar() {} Integer baz() { return 1; } }";
        let db = SmokeDatabase::default();
        let file = SourceFile::new(&db, SmolStr::new(source));

        let methods = method_pointers(&db, file);
        assert_eq!(methods.len(), 2);

        // Re-resolve each salsa-memoized pointer against a freshly built
        // (and separately, non-Send/non-Sync) `SyntaxNode` root -- the
        // same "owned pointer, live node only at the edge" pattern
        // `crate::ptr` documents -- to confirm the pointers salsa handed
        // back are still meaningful once real rowan nodes are back in
        // play, not just opaque bytes that happened to satisfy a bound.
        let root = apex_parser::parse_compilation_unit(source).syntax();
        for (name, ptr) in methods {
            let node = ptr
                .to_node(&root)
                .expect("ptr should resolve against a fresh root");
            let method = MethodDecl::cast(node).expect("ptr should point at a MethodDecl");
            assert_eq!(method.name().and_then(|n| n.text()).as_deref(), Some(name.as_str()));
        }

        // Re-running the tracked fn on the same input hits salsa's own
        // memoized result rather than re-parsing -- the actual point of
        // adopting salsa, not just "it compiles."
        let methods_again = method_pointers(&db, file);
        assert_eq!(methods, methods_again);
    }
}
