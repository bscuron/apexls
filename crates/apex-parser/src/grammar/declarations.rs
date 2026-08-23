//! Phase 3: top-level declaration grammar (classes/interfaces/enums,
//! triggers, members, modifiers, annotations), matching
//! `BaseApexParser.g4`'s `compilationUnit`/`typeDeclaration`/
//! `classDeclaration`/`memberDeclaration`/`modifier`/`triggerUnit` rules.

use crate::parser::{CompletedMarker, Parser};
use apex_syntax::SyntaxKind;

/// `compilationUnit: typeDeclaration EOF` -- the whole-`.cls`-file entry
/// point.
pub(crate) fn compilation_unit(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    type_decl(p);
    m.complete(p, SyntaxKind::CompilationUnit)
}

/// `typeDeclaration: modifier* (classDeclaration | enumDeclaration |
/// interfaceDeclaration)`. Also the shape of a *nested* type declaration
/// (`memberDeclaration`'s class/interface/enum alternatives) -- reused
/// identically there so modifiers end up as direct children of
/// `ClassDecl`/etc regardless of nesting depth.
pub(crate) fn type_decl(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    modifiers(p);
    let kind = match p.current() {
        SyntaxKind::Class => {
            class_decl_rest(p);
            SyntaxKind::ClassDecl
        }
        SyntaxKind::Interface => {
            interface_decl_rest(p);
            SyntaxKind::InterfaceDecl
        }
        SyntaxKind::Enum => {
            enum_decl_rest(p);
            SyntaxKind::EnumDecl
        }
        _ => {
            p.error(format!(
                "expected 'class', 'interface', or 'enum', found {:?}",
                p.current()
            ));
            SyntaxKind::ErrorNode
        }
    };
    m.complete(p, kind)
}

// ---- modifiers / annotations ----

pub(crate) fn at_modifier_start(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::AtSign
            | SyntaxKind::Global
            | SyntaxKind::Public
            | SyntaxKind::Protected
            | SyntaxKind::Private
            | SyntaxKind::Transient
            | SyntaxKind::Static
            | SyntaxKind::Abstract
            | SyntaxKind::Final
            | SyntaxKind::Webservice
            | SyntaxKind::Override
            | SyntaxKind::Virtual
            | SyntaxKind::Testmethod
            | SyntaxKind::With
            | SyntaxKind::Without
            | SyntaxKind::Inherited
    )
}

pub(crate) fn modifiers(p: &mut Parser<'_>) {
    while at_modifier_start(p) {
        modifier(p);
    }
}

fn modifier(p: &mut Parser<'_>) {
    match p.current() {
        SyntaxKind::AtSign => {
            annotation(p);
        }
        SyntaxKind::With | SyntaxKind::Without | SyntaxKind::Inherited => {
            let m = p.start();
            p.bump();
            p.expect(SyntaxKind::Sharing);
            m.complete(p, SyntaxKind::Modifier);
        }
        _ => {
            let m = p.start();
            p.bump();
            m.complete(p, SyntaxKind::Modifier);
        }
    }
}

fn annotation(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // @
    super::ids::expect_id(p);
    if p.at(SyntaxKind::LParen) {
        p.bump();
        if !p.at(SyntaxKind::RParen) {
            annotation_args(p);
        }
        p.expect(SyntaxKind::RParen);
    }
    m.complete(p, SyntaxKind::Annotation)
}

/// `elementValuePairs: elementValuePair (','? elementValuePair)*` (the
/// comma is deliberately optional per the reference grammar) or a single
/// bare `elementValue` (e.g. `@SuppressWarnings('PMD')`).
fn annotation_args(p: &mut Parser<'_>) {
    if super::ids::at_id(p) && p.nth(1) == SyntaxKind::Assign {
        let m = p.start();
        annotation_arg(p);
        loop {
            if p.at(SyntaxKind::Comma) {
                p.bump();
            }
            if super::ids::at_id(p) && p.nth(1) == SyntaxKind::Assign {
                annotation_arg(p);
            } else {
                break;
            }
        }
        m.complete(p, SyntaxKind::AnnotationArgList);
    } else {
        literal_value(p);
    }
}

fn annotation_arg(p: &mut Parser<'_>) {
    let m = p.start();
    super::ids::expect_id(p);
    p.expect(SyntaxKind::Assign);
    literal_value(p);
    m.complete(p, SyntaxKind::AnnotationArg);
}

/// `literal: IntegerLiteral | LongLiteral | NumberLiteral | StringLiteral
/// | MultilineStringLiteral | BooleanLiteral | NULL`.
fn literal_value(p: &mut Parser<'_>) {
    if matches!(
        p.current(),
        SyntaxKind::IntegerLiteral
            | SyntaxKind::LongLiteral
            | SyntaxKind::NumberLiteral
            | SyntaxKind::StringLiteral
            | SyntaxKind::MultilineStringLiteral
            | SyntaxKind::BooleanLiteral
            | SyntaxKind::Null
    ) {
        p.bump();
    } else {
        p.error("expected a literal value");
    }
}

// ---- class ----

fn class_decl_rest(p: &mut Parser<'_>) {
    p.bump(); // class
    super::ids::expect_name(p);
    if p.at(SyntaxKind::Extends) {
        p.bump();
        if !super::types::type_ref(p) {
            p.error("expected a type after 'extends'");
        }
    }
    if p.at(SyntaxKind::Implements) {
        p.bump();
        type_ref_list(p);
    }
    class_body(p);
}

fn type_ref_list(p: &mut Parser<'_>) {
    let m = p.start();
    if !super::types::type_ref(p) {
        p.error("expected a type");
    }
    while p.at(SyntaxKind::Comma) {
        p.bump();
        if !super::types::type_ref(p) {
            p.error("expected a type");
        }
    }
    m.complete(p, SyntaxKind::TypeRefList);
}

fn class_body(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    p.parse_list(|p| p.at(SyntaxKind::RBrace), class_body_decl);
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::ClassBody);
}

/// `classBodyDeclaration: ';' | STATIC? block | modifier* memberDeclaration`.
fn class_body_decl(p: &mut Parser<'_>) {
    if p.at(SyntaxKind::Semi) {
        p.bump();
        return;
    }
    if p.at(SyntaxKind::LBrace) {
        super::statements::block(p); // bare instance-initializer block
        return;
    }
    if p.at(SyntaxKind::Static) && p.nth(1) == SyntaxKind::LBrace {
        p.bump(); // static
        super::statements::block(p); // static initializer block
        return;
    }
    member_decl(p);
}

/// `modifier* memberDeclaration`. Nested type declarations are handled by
/// speculatively consuming modifiers, checking for a type keyword, and
/// rolling back to delegate to `type_decl` if so -- keeping its tree
/// shape (modifiers as direct children of ClassDecl/etc) identical
/// whether the type is top-level or nested, rather than special-casing.
fn member_decl(p: &mut Parser<'_>) {
    let checkpoint = p.checkpoint();
    modifiers(p);
    if matches!(
        p.current(),
        SyntaxKind::Class | SyntaxKind::Interface | SyntaxKind::Enum
    ) {
        p.rollback(checkpoint);
        type_decl(p);
        return;
    }
    p.rollback(checkpoint);

    let m = p.start();
    modifiers(p);
    let kind = member_decl_rest(p);
    m.complete(p, kind);
}

/// Dispatches `methodDeclaration | constructorDeclaration |
/// propertyDeclaration | fieldDeclaration` (modifiers already consumed
/// by the caller). `VOID` unambiguously means a method (the only
/// alternative that allows it); otherwise a `Type` is parsed and: `(`
/// immediately after means the type *was* the constructor's name;
/// otherwise an identifier follows, and one token of lookahead past that
/// (`(`, `{`, or neither) picks method / property / field.
fn member_decl_rest(p: &mut Parser<'_>) -> SyntaxKind {
    if p.at(SyntaxKind::Void) {
        p.bump();
        super::ids::expect_name(p);
        formal_parameters(p);
        method_body_or_semi(p);
        return SyntaxKind::MethodDecl;
    }

    if !super::types::type_ref(p) {
        p.error("expected a member declaration");
        return SyntaxKind::ErrorNode;
    }

    if p.at(SyntaxKind::LParen) {
        formal_parameters(p);
        super::statements::block(p);
        return SyntaxKind::ConstructorDecl;
    }

    if !super::ids::at_id(p) {
        p.error(format!("expected a member name, found {:?}", p.current()));
        return SyntaxKind::ErrorNode;
    }

    match p.nth(1) {
        SyntaxKind::LParen => {
            let name = p.start();
            p.bump(); // method name
            name.complete(p, SyntaxKind::DeclName);
            formal_parameters(p);
            method_body_or_semi(p);
            SyntaxKind::MethodDecl
        }
        SyntaxKind::LBrace => {
            let name = p.start();
            p.bump(); // property name
            name.complete(p, SyntaxKind::DeclName);
            property_body(p);
            SyntaxKind::PropertyDecl
        }
        _ => {
            super::statements::var_declarators(p);
            p.expect(SyntaxKind::Semi);
            SyntaxKind::FieldDecl
        }
    }
}

fn method_body_or_semi(p: &mut Parser<'_>) {
    if p.at(SyntaxKind::LBrace) {
        super::statements::block(p);
    } else {
        p.expect(SyntaxKind::Semi);
    }
}

fn formal_parameters(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(SyntaxKind::LParen);
    if !p.at(SyntaxKind::RParen) {
        formal_parameter(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            formal_parameter(p);
        }
    }
    p.expect(SyntaxKind::RParen);
    m.complete(p, SyntaxKind::FormalParamList);
}

fn formal_parameter(p: &mut Parser<'_>) {
    let m = p.start();
    modifiers(p);
    if !super::types::type_ref(p) {
        p.error("expected a parameter type");
    }
    super::ids::expect_name(p);
    m.complete(p, SyntaxKind::FormalParam);
}

/// `propertyDeclaration`'s body: `'{' propertyBlock* '}'`, where
/// `propertyBlock: modifier* (getter | setter)`.
fn property_body(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::LBrace);
    p.parse_list(|p| p.at(SyntaxKind::RBrace), property_block);
    p.expect(SyntaxKind::RBrace);
}

fn property_block(p: &mut Parser<'_>) {
    let m = p.start();
    modifiers(p);
    if matches!(p.current(), SyntaxKind::Get | SyntaxKind::Set) {
        p.bump();
        if p.at(SyntaxKind::LBrace) {
            super::statements::block(p);
        } else {
            p.expect(SyntaxKind::Semi);
        }
    } else {
        p.error(format!("expected 'get' or 'set', found {:?}", p.current()));
    }
    m.complete(p, SyntaxKind::PropertyAccessor);
}

// ---- interface ----

fn interface_decl_rest(p: &mut Parser<'_>) {
    p.bump(); // interface
    super::ids::expect_name(p);
    if p.at(SyntaxKind::Extends) {
        p.bump();
        type_ref_list(p);
    }
    interface_body(p);
}

fn interface_body(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    p.parse_list(|p| p.at(SyntaxKind::RBrace), interface_method_decl);
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::InterfaceBody);
}

/// `interfaceMethodDeclaration: modifier* (typeRef|VOID) id
/// formalParameters ';'` -- interface methods never have a body.
fn interface_method_decl(p: &mut Parser<'_>) {
    let m = p.start();
    modifiers(p);
    if p.at(SyntaxKind::Void) {
        p.bump();
    } else if !super::types::type_ref(p) {
        p.error("expected a return type");
    }
    super::ids::expect_name(p);
    formal_parameters(p);
    p.expect(SyntaxKind::Semi);
    m.complete(p, SyntaxKind::MethodDecl);
}

// ---- enum ----

fn enum_decl_rest(p: &mut Parser<'_>) {
    p.bump(); // enum
    super::ids::expect_name(p);
    p.expect(SyntaxKind::LBrace);
    if !p.at(SyntaxKind::RBrace) {
        let m = p.start();
        super::ids::expect_name(p);
        while p.at(SyntaxKind::Comma) {
            p.bump();
            super::ids::expect_name(p);
        }
        m.complete(p, SyntaxKind::EnumConstantList);
    }
    p.expect(SyntaxKind::RBrace);
}

// ---- trigger ----

/// `triggerUnit: TRIGGER id ON id '(' triggerCase (',' triggerCase)* ')'
/// triggerBlock`.
pub(crate) fn trigger_unit(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // trigger
    super::ids::expect_name(p);
    p.expect(SyntaxKind::On);
    super::ids::expect_id(p); // SObject reference, not a declared name
    p.expect(SyntaxKind::LParen);
    trigger_case(p);
    while p.at(SyntaxKind::Comma) {
        p.bump();
        trigger_case(p);
    }
    p.expect(SyntaxKind::RParen);
    trigger_block(p);
    m.complete(p, SyntaxKind::TriggerUnit)
}

fn trigger_case(p: &mut Parser<'_>) {
    let m = p.start();
    if matches!(p.current(), SyntaxKind::Before | SyntaxKind::After) {
        p.bump();
    } else {
        p.error("expected 'before' or 'after'");
    }
    if matches!(
        p.current(),
        SyntaxKind::Insert | SyntaxKind::Update | SyntaxKind::Delete | SyntaxKind::Undelete
    ) {
        p.bump();
    } else {
        p.error("expected 'insert', 'update', 'delete', or 'undelete'");
    }
    m.complete(p, SyntaxKind::TriggerCase);
}

fn trigger_block(p: &mut Parser<'_>) {
    let m = p.start();
    p.expect(SyntaxKind::LBrace);
    p.parse_list(|p| p.at(SyntaxKind::RBrace), trigger_block_member);
    p.expect(SyntaxKind::RBrace);
    m.complete(p, SyntaxKind::TriggerBlock);
}

/// `triggerBlockMember: modifier* triggerMemberDeclaration | statement`.
/// `triggerMemberDeclaration` is the same alternative set as
/// `memberDeclaration` minus constructors (which don't make sense
/// outside a class); a bare `{ ... }` block is already covered by
/// `statement -> block`.
///
/// A pragmatic simplification for the genuinely ambiguous no-modifier
/// case: since `Type id ...` with no leading modifier could in principle
/// be a modifier-less method/property/field declaration *or* a local
/// variable declaration statement (structurally near-identical, and both
/// grammar-legal), and inline method/property declarations directly in a
/// trigger body (rather than in a proper handler class) are vanishingly
/// rare in practice, only a leading modifier keyword or `void`/`class`/
/// `interface`/`enum` routes into member-declaration parsing here;
/// everything else goes through `statement`, which already resolves the
/// local-var-decl-vs-expression-statement ambiguity on its own.
fn trigger_block_member(p: &mut Parser<'_>) {
    if at_modifier_start(p)
        || matches!(
            p.current(),
            SyntaxKind::Void | SyntaxKind::Class | SyntaxKind::Interface | SyntaxKind::Enum
        )
    {
        member_decl(p);
    } else {
        super::statements::statement(p);
    }
}
