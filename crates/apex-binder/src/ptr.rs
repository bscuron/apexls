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

use apex_syntax::{ApexLanguage, SyntaxKind, SyntaxNode};
use rowan::ast::AstNode;
use rowan::{NodeOrToken, TextRange};
use std::marker::PhantomData;

/// An untyped node pointer: kind + range, no static guarantee about what
/// kind of declaration/expression/statement it names. Used for `Symbol`s,
/// which point at heterogeneous declaration node kinds (`ClassDecl`,
/// `MethodDecl`, `FieldDecl`, ...) with no single common `AstNode` type
/// to parameterize an `AstPtr` over.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SyntaxPtr {
    kind: SyntaxKind,
    range: TextRange,
}

impl SyntaxPtr {
    pub fn new(node: &SyntaxNode) -> Self {
        SyntaxPtr {
            kind: node.kind(),
            range: node.text_range(),
        }
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
    pub fn new(node: &N) -> Self {
        AstPtr {
            raw: SyntaxPtr::new(node.syntax()),
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
        let ptr = AstPtr::<ClassDecl>::new(&class);

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
        let ptr = SyntaxPtr::new(name.syntax());

        let root2 = parse.syntax();
        let resolved = ptr.to_node(&root2).expect("ptr should resolve");
        assert_eq!(resolved.text().to_string(), "Foo");
    }
}
