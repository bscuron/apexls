//! Typed AST wrappers for declaration grammar
//! (`grammar::declarations`/`grammar::statements::{var_declarator,
//! catch_clause}`): classes/interfaces/enums/triggers, their members, and
//! the shared `Name`/`Type`/`modifier`/`annotation` building blocks. This
//! is what a symbol-table binder walks to build scopes -- everything
//! here is read-only, structural access over the tree, no semantics.

use super::{
    ast_node, dispatch_enum, first_non_trivia_token, token_after, Block, Expr, Name, Type,
};
use crate::{ApexLanguage, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::{support, AstChildren, AstNode};

dispatch_enum! {
    TypeDecl {
        Class(ClassDecl) => ClassDecl,
        Interface(InterfaceDecl) => InterfaceDecl,
        Enum(EnumDecl) => EnumDecl,
    }
}

// A `ClassBody`/`TriggerBlock` member that's a declaration (as opposed
// to a bare statement or initializer block, neither of which are
// declarations `can_cast` picks up -- `support::children::<Member>`
// silently skips them, which is exactly what a declaration-only walk
// wants).
dispatch_enum! {
    Member {
        Method(MethodDecl) => MethodDecl,
        Constructor(ConstructorDecl) => ConstructorDecl,
        Field(FieldDecl) => FieldDecl,
        Property(PropertyDecl) => PropertyDecl,
        NestedClass(ClassDecl) => ClassDecl,
        NestedInterface(InterfaceDecl) => InterfaceDecl,
        NestedEnum(EnumDecl) => EnumDecl,
    }
}

ast_node!(CompilationUnit, CompilationUnit);
ast_node!(TriggerUnit, TriggerUnit);
ast_node!(TriggerCase, TriggerCase);
ast_node!(TriggerBlock, TriggerBlock);
ast_node!(ClassDecl, ClassDecl);
ast_node!(InterfaceDecl, InterfaceDecl);
ast_node!(EnumDecl, EnumDecl);
ast_node!(EnumConstantList, EnumConstantList);
ast_node!(ClassBody, ClassBody);
ast_node!(InterfaceBody, InterfaceBody);
ast_node!(TypeRefList, TypeRefList);
ast_node!(Modifier, Modifier);
ast_node!(Annotation, Annotation);
ast_node!(MethodDecl, MethodDecl);
ast_node!(ConstructorDecl, ConstructorDecl);
ast_node!(FieldDecl, FieldDecl);
ast_node!(PropertyDecl, PropertyDecl);
ast_node!(PropertyAccessor, PropertyAccessor);
ast_node!(FormalParamList, FormalParamList);
ast_node!(FormalParam, FormalParam);
ast_node!(VarDeclarator, VarDeclarator);

/// Shared by every declaration that can carry `modifier*` (classes,
/// interfaces, enums, methods, constructors, fields, properties,
/// parameters, catch clauses): plain modifiers (`public`, `static`,
/// `with sharing`, ...) and `@`-annotations are two different node kinds
/// at the same sibling position (see `grammar::declarations::modifier`),
/// so both are exposed rather than merged into one.
pub trait HasModifiers: AstNode<Language = ApexLanguage> {
    fn modifiers(&self) -> AstChildren<Modifier> {
        support::children(self.syntax())
    }

    fn annotations(&self) -> AstChildren<Annotation> {
        support::children(self.syntax())
    }
}

impl HasModifiers for ClassDecl {}
impl HasModifiers for InterfaceDecl {}
impl HasModifiers for EnumDecl {}
impl HasModifiers for MethodDecl {}
impl HasModifiers for ConstructorDecl {}
impl HasModifiers for FieldDecl {}
impl HasModifiers for PropertyDecl {}
impl HasModifiers for FormalParam {}

impl CompilationUnit {
    pub fn type_decl(&self) -> Option<TypeDecl> {
        support::child(self.syntax())
    }
}

impl TriggerUnit {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// The SObject the trigger fires on (`TRIGGER id ON <object> (...)`)
    /// -- a reference, not a declaration, so it's a bare token rather
    /// than a `Name` node (see `grammar::ids::expect_name`'s doc comment
    /// for that distinction).
    pub fn object_ref(&self) -> Option<SyntaxToken> {
        token_after(self.syntax(), SyntaxKind::On)
    }

    pub fn cases(&self) -> AstChildren<TriggerCase> {
        support::children(self.syntax())
    }

    pub fn block(&self) -> Option<TriggerBlock> {
        support::child(self.syntax())
    }
}

impl TriggerBlock {
    /// Only the declaration-shaped members (see `Member`'s doc comment);
    /// bare statements directly in a trigger body aren't reachable here.
    pub fn members(&self) -> AstChildren<Member> {
        support::children(self.syntax())
    }
}

impl ClassDecl {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// The single `extends` target, if any. Unambiguous as "the first
    /// `Type` child" because `implements`' types live inside a nested
    /// `TypeRefList`, never as direct `Type` children of `ClassDecl`
    /// itself.
    pub fn extends(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn implements(&self) -> Option<TypeRefList> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<ClassBody> {
        support::child(self.syntax())
    }
}

impl InterfaceDecl {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// Interfaces can extend several interfaces at once, hence a list
    /// even though there's no separate `implements`.
    pub fn extends(&self) -> Option<TypeRefList> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<InterfaceBody> {
        support::child(self.syntax())
    }
}

impl EnumDecl {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// `None` for a (legal but unusual) enum with zero constants -- the
    /// parser only opens an `EnumConstantList` node when at least one is
    /// present.
    pub fn constant_list(&self) -> Option<EnumConstantList> {
        support::child(self.syntax())
    }
}

impl EnumConstantList {
    pub fn constants(&self) -> AstChildren<Name> {
        support::children(self.syntax())
    }
}

impl ClassBody {
    pub fn members(&self) -> AstChildren<Member> {
        support::children(self.syntax())
    }
}

impl InterfaceBody {
    pub fn methods(&self) -> AstChildren<MethodDecl> {
        support::children(self.syntax())
    }
}

impl TypeRefList {
    pub fn types(&self) -> AstChildren<Type> {
        support::children(self.syntax())
    }
}

impl MethodDecl {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    /// `None` means `void` -- the parser never emits a `Type` node for
    /// the `VOID` keyword, so a missing return type and an explicit
    /// `void` are indistinguishable from this alone; use [`Self::is_void`]
    /// if that distinction matters (it always should, in practice, since
    /// this accessor is only ever `None` for a `void` method -- a
    /// well-formed tree never leaves a required return type out
    /// entirely).
    pub fn return_type(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn is_void(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Void).is_some()
    }

    pub fn params(&self) -> Option<FormalParamList> {
        support::child(self.syntax())
    }

    /// `None` for an interface method or an abstract/`extern` method
    /// declared with a trailing `;` instead of a body.
    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl ConstructorDecl {
    /// A constructor's "name" must equal its class's name, so the parser
    /// captures it the same way a return type would be parsed (see
    /// `grammar::declarations::member_decl_rest`) -- there's no separate
    /// `Name` node here, just the `Type` that turned out to be followed
    /// immediately by `(`.
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn params(&self) -> Option<FormalParamList> {
        support::child(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl FieldDecl {
    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn declarators(&self) -> AstChildren<VarDeclarator> {
        support::children(self.syntax())
    }
}

impl PropertyDecl {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }

    pub fn accessors(&self) -> AstChildren<PropertyAccessor> {
        support::children(self.syntax())
    }
}

impl PropertyAccessor {
    pub fn is_getter(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Get).is_some()
    }

    pub fn is_setter(&self) -> bool {
        support::token(self.syntax(), SyntaxKind::Set).is_some()
    }

    /// `None` for the common `get;`/`set;` auto-implemented form.
    pub fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

impl FormalParamList {
    pub fn params(&self) -> AstChildren<FormalParam> {
        support::children(self.syntax())
    }
}

impl FormalParam {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    pub fn type_ref(&self) -> Option<Type> {
        support::child(self.syntax())
    }
}

impl VarDeclarator {
    pub fn name(&self) -> Option<Name> {
        support::child(self.syntax())
    }

    pub fn init(&self) -> Option<Expr> {
        support::child(self.syntax())
    }
}

impl Modifier {
    /// The modifier keyword itself (`public`, `static`, ...), or -- for
    /// the `with`/`without`/`inherited sharing` form -- just the leading
    /// `with`/`without`/`inherited` token; call `.syntax().text()` for
    /// the whole `with sharing` phrase.
    pub fn keyword(&self) -> Option<SyntaxToken> {
        first_non_trivia_token(self.syntax())
    }
}

impl Annotation {
    /// The name follows `@` immediately; not wrapped in its own `Name`
    /// node since an annotation reference isn't a declaration.
    pub fn name(&self) -> Option<SyntaxToken> {
        token_after(self.syntax(), SyntaxKind::AtSign)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GreenNodeBuilder, SyntaxKind};

    /// Builds `class Foo { }` by hand and checks the declaration
    /// accessors (`type_decl`, `ClassDecl::name`, `::body`) resolve the
    /// right nodes -- `apex-parser`'s own tests exercise this against
    /// real parsed trees; this just checks the macro-generated
    /// `AstNode`/`support::child` wiring itself.
    #[test]
    fn class_decl_name_and_body_resolve() {
        let mut b = GreenNodeBuilder::new();
        b.start_node(SyntaxKind::CompilationUnit.into());
        b.start_node(SyntaxKind::ClassDecl.into());
        b.token(SyntaxKind::Class.into(), "class");
        b.token(SyntaxKind::Whitespace.into(), " ");
        b.start_node(SyntaxKind::DeclName.into());
        b.token(SyntaxKind::Identifier.into(), "Foo");
        b.token(SyntaxKind::Whitespace.into(), " ");
        b.finish_node(); // DeclName
        b.start_node(SyntaxKind::ClassBody.into());
        b.token(SyntaxKind::LBrace.into(), "{");
        b.token(SyntaxKind::Whitespace.into(), " ");
        b.token(SyntaxKind::RBrace.into(), "}");
        b.finish_node(); // ClassBody
        b.finish_node(); // ClassDecl
        b.finish_node(); // CompilationUnit

        let root = SyntaxNode::new_root(b.finish());
        let cu = CompilationUnit::cast(root).unwrap();
        let TypeDecl::Class(class) = cu.type_decl().unwrap() else {
            panic!("expected a ClassDecl");
        };
        assert_eq!(class.name().unwrap().text().unwrap(), "Foo");
        assert!(class.body().unwrap().members().next().is_none());
    }
}
