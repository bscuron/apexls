//! Storage-safe "pointers" into a syntax tree: a node's [`SyntaxKind`]
//! plus its [`TextRange`], re-resolved against a root [`SyntaxNode`] on
//! demand rather than held as a live node handle.
//!
//! rowan's green tree is `Rc`-based, so `SyntaxNode`/`SyntaxToken` are
//! neither `Send` nor `Sync` and keep their whole tree alive for as long
//! as any single handle survives -- exactly the shape a long-lived,
//! arena-stored `SymbolTable` (thousands of entries, held for the whole
//! life of a bound project) must avoid. `SyntaxPtr`/`AstPtr<N>` mirror
//! rust-analyzer's `SyntaxNodePtr`/`AstPtr<N>` split: cheap, `Copy`,
//! `Send + Sync` data that can be re-resolved into a real node against
//! whichever `SyntaxNode` root is currently in hand (a fresh
//! `Parse::syntax()` call against the same `Parse` produces a
//! structurally-identical tree, since the underlying green node is
//! reused).
//!
//! Every pointer also carries the [`FileId`] of the file it points into.
//! Without it, two *different* files that happen to produce an
//! identically-shaped node at the same byte range (easy with a shared
//! boilerplate prefix, e.g. `public class Foo {`) would collide as
//! `HashMap` keys in `ReferenceTable`/`crate::BoundProgram`'s scope-tree
//! map, silently corrupting one file's result with another's. Carrying
//! `FileId` closes that (kind, range) alone can't disambiguate, and is
//! also exactly the granularity `crate::BindCache`'s per-file caching
//! needs to invalidate/replace one file's pointers without touching any
//! other file's.

use crate::file_id::FileId;
use apex_syntax::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::AstNode;
use rowan::{NodeOrToken, TextRange};
use std::marker::PhantomData;

/// An untyped node pointer: file + kind + range, no static guarantee
/// about what kind of declaration/expression/statement it names. Used
/// for `Symbol`s, which point at heterogeneous declaration node kinds
/// (`ClassDecl`, `MethodDecl`, `FieldDecl`, ...) with no single common
/// `AstNode` type to parameterize an `AstPtr` over.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SyntaxPtr {
    file: FileId,
    kind: SyntaxKind,
    range: TextRange,
}

impl SyntaxPtr {
    pub fn new(file: FileId, node: &SyntaxNode) -> Self {
        SyntaxPtr {
            file,
            kind: node.kind(),
            range: node.text_range(),
        }
    }

    /// Like [`Self::new`], but for a single token rather than a node --
    /// one segment of a qualified `Outer.Inner` reference (a dotted
    /// `Type`), which has no node of its own to key a `Resolution` by
    /// (the whole path is one flat `Type` node; see
    /// `crate::resolve::resolve_type_ref`'s doc comment). Never re-resolved
    /// via [`Self::to_node`] -- a `ReferenceTable` entry is only ever
    /// looked up by exact `(file, kind, range)` equality, never walked
    /// back into a live node, so a token-shaped `kind` (never a real
    /// `SyntaxNode` kind for a multi-token path) is safe to store.
    pub fn for_token(file: FileId, token: &SyntaxToken) -> Self {
        SyntaxPtr {
            file,
            kind: token.kind(),
            range: token.text_range(),
        }
    }

    pub fn file(&self) -> FileId {
        self.file
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    /// Re-resolves this pointer against `root`. `None` only if `root` is
    /// not (a structurally-identical copy of) the tree this pointer was
    /// built from -- no node at the exact recorded range/kind exists.
    pub fn to_node(&self, root: &SyntaxNode) -> Option<SyntaxNode> {
        let element = root.covering_element(self.range);
        let mut node = match element {
            NodeOrToken::Node(n) => n,
            NodeOrToken::Token(t) => t.parent()?,
        };
        loop {
            if node.text_range() == self.range && node.kind() == self.kind {
                return Some(node);
            }
            node = node.parent()?;
        }
    }
}

/// A typed counterpart to [`SyntaxPtr`], narrowed to a specific
/// `AstNode` wrapper type (`AstPtr<ClassDecl>`, `AstPtr<Type>`, ...) on
/// the way out of [`AstPtr::to_node`].
pub struct AstPtr<N> {
    raw: SyntaxPtr,
    _marker: PhantomData<fn() -> N>,
}

impl<N> Clone for AstPtr<N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N> Copy for AstPtr<N> {}

impl<N> PartialEq for AstPtr<N> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<N> Eq for AstPtr<N> {}

impl<N> std::hash::Hash for AstPtr<N> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<N> std::fmt::Debug for AstPtr<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AstPtr").field("raw", &self.raw).finish()
    }
}

impl<N: AstNode<Language = ApexLanguage>> AstPtr<N> {
    pub fn new(file: FileId, node: &N) -> Self {
        AstPtr {
            raw: SyntaxPtr::new(file, node.syntax()),
            _marker: PhantomData,
        }
    }

    pub fn range(&self) -> TextRange {
        self.raw.range()
    }

    pub fn to_node(&self, root: &SyntaxNode) -> Option<N> {
        self.raw.to_node(root).and_then(N::cast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_syntax::ast::decl::{ClassDecl, CompilationUnit, TypeDecl};

    #[test]
    fn ast_ptr_round_trips_against_a_freshly_fetched_root() {
        let parse = apex_parser::parse_compilation_unit("class Foo { Integer x; }");
        assert!(parse.errors.is_empty());
        let root = parse.syntax();
        let cu = CompilationUnit::cast(root.clone()).unwrap();
        let TypeDecl::Class(class) = cu.type_decl().unwrap() else {
            panic!("expected a ClassDecl");
        };
        let ptr = AstPtr::<ClassDecl>::new(FileId(0), &class);

        // A second, independently-fetched `SyntaxNode` root for the same
        // `Parse` -- structurally identical, but not the same handle.
        let root2 = parse.syntax();
        let resolved = ptr.to_node(&root2).expect("ptr should resolve");
        assert_eq!(resolved.name().unwrap().text().unwrap(), "Foo");
    }

    #[test]
    fn syntax_ptr_resolves_a_leaf_token_sized_node() {
        // `DeclName` here wraps exactly one token (`Foo`) with no
        // trailing trivia of its own (no space before `{`, so nothing
        // trails onto the still-open `DeclName` node) -- exercises the
        // token-covering-element-then-climb path in `to_node`, not just
        // the common node-covering-element case.
        let parse = apex_parser::parse_compilation_unit("class Foo{ }");
        let root = parse.syntax();
        let cu = CompilationUnit::cast(root.clone()).unwrap();
        let TypeDecl::Class(class) = cu.type_decl().unwrap() else {
            panic!("expected a ClassDecl");
        };
        let name = class.name().unwrap();
        let ptr = SyntaxPtr::new(FileId(0), name.syntax());

        let root2 = parse.syntax();
        let resolved = ptr.to_node(&root2).expect("ptr should resolve");
        assert_eq!(resolved.text().to_string(), "Foo");
    }
}
