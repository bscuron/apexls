//! Pass 1: declaration-site collection. Walks one file's
//! `CompilationUnit`/`TriggerUnit` (`apex_syntax::ast::decl::TypeDecl` ->
//! `Member`, recursing into nested classes/interfaces/enums for free
//! since they're just `Member` variants) into `Symbol`s.
//!
//! A pure function of a single file's already-parsed tree -- no
//! cross-file lookups happen here (a class's `extends` target might not
//! be collected yet, might live in another file, or might not exist at
//! all), so collection is embarrassingly parallel across files. Every
//! `Symbol` gets its final, stable `SymbolId` right here (`file` is
//! already known, and `local` is just "the Nth symbol collected from
//! this file") -- unlike the old flat-index scheme, there's no later
//! remapping step: `crate::BoundProgram::from_files_cached` merges each
//! file's `FileCollection` into the shared `SymbolTable` by just moving
//! the data, since the ids it contains are already final. See
//! `crate::symbol::SymbolId`'s doc comment for why that stability
//! matters.

use crate::file_id::FileId;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind};
use apex_syntax::ast::decl::{
    ClassDecl, CompilationUnit, ConstructorDecl, EnumDecl, FieldDecl, FormalParamList,
    HasModifiers, InterfaceDecl, Member, MethodDecl, PropertyDecl, TriggerUnit, TypeDecl,
};
use apex_syntax::ast::Type;
use rowan::ast::AstNode;

/// One file's collected declarations, already carrying final, stable
/// `SymbolId`s (see the module doc comment).
#[derive(Default, Clone)]
pub(crate) struct FileCollection {
    pub(crate) symbols: Vec<Symbol>,
    /// `(a type symbol's id, unresolved extends/implements supertype
    /// names as written)`, consumed by Pass 1.5 (`crate::inherit`) once
    /// every file's symbols have been merged into one project-wide
    /// `SymbolTable`, to build the member-lookup `inherited_chain`.
    pub(crate) raw_extends: Vec<(SymbolId, Vec<String>)>,
    /// `(a *class* symbol's id, its direct `extends` target's unresolved
    /// name)`, present only when that class actually declared `extends`
    /// -- narrower than `raw_extends` (which also folds in
    /// `implements`), kept separate because `super` resolution needs
    /// exactly "the one direct base class," not the whole flattened
    /// member-lookup chain (whose internal ordering doesn't preserve
    /// "which one was `extends`" once interfaces are mixed in).
    pub(crate) raw_super: Vec<(SymbolId, String)>,
}

impl FileCollection {
    fn push(&mut self, file: FileId, symbol: Symbol) -> SymbolId {
        let id = SymbolId::new(file, self.symbols.len() as u32);
        self.symbols.push(symbol);
        id
    }
}

fn type_ptr_and_name(file: FileId, ty: Option<Type>) -> (Option<AstPtr<Type>>, Option<String>) {
    match ty {
        Some(ty) => (Some(AstPtr::new(file, &ty)), Some(ty.text())),
        None => (None, None),
    }
}

/// Collects every declaration in one parsed `.cls` `CompilationUnit`.
pub(crate) fn collect_compilation_unit(file: FileId, cu: &CompilationUnit) -> FileCollection {
    let mut out = FileCollection::default();
    if let Some(type_decl) = cu.type_decl() {
        collect_type_decl(&mut out, file, &type_decl, None);
    }
    out
}

/// Collects every declaration in one parsed `.trigger` `TriggerUnit`. A
/// trigger has no `extends`/`implements` of its own, but its body can
/// declare helper members the same way a class body can.
pub(crate) fn collect_trigger_unit(file: FileId, tu: &TriggerUnit) -> FileCollection {
    let mut out = FileCollection::default();
    let Some(name) = tu.name() else {
        return out;
    };
    let Some(name_text) = name.text() else {
        return out;
    };
    let trigger_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Trigger,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, tu.syntax()),
            name_range: name.syntax().text_range(),
            container: None,
            type_ref: None,
            type_name: None,
            modifiers: ModifierSet::default(),
        },
    );

    if let Some(block) = tu.block() {
        for member in block.members() {
            collect_member(&mut out, file, &member, trigger_id);
        }
    }
    out
}

fn collect_type_decl(
    out: &mut FileCollection,
    file: FileId,
    type_decl: &TypeDecl,
    container: Option<SymbolId>,
) {
    match type_decl {
        TypeDecl::Class(class) => collect_class(out, file, class, container),
        TypeDecl::Interface(iface) => collect_interface(out, file, iface, container),
        TypeDecl::Enum(en) => collect_enum(out, file, en, container),
    }
}

fn collect_class(
    out: &mut FileCollection,
    file: FileId,
    class: &ClassDecl,
    container: Option<SymbolId>,
) {
    let Some(name) = class.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let class_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Class,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, class.syntax()),
            name_range: name.syntax().text_range(),
            container,
            type_ref: None,
            type_name: None,
            modifiers: ModifierSet::from_modifiers(class.modifiers()),
        },
    );

    let mut supertypes = Vec::new();
    if let Some(extends) = class.extends() {
        out.raw_super.push((class_id, extends.text()));
        supertypes.push(extends.text());
    }
    if let Some(implements) = class.implements() {
        supertypes.extend(implements.types().map(|t| t.text()));
    }
    if !supertypes.is_empty() {
        out.raw_extends.push((class_id, supertypes));
    }

    if let Some(body) = class.body() {
        for member in body.members() {
            collect_member(out, file, &member, class_id);
        }
    }
}

fn collect_interface(
    out: &mut FileCollection,
    file: FileId,
    iface: &InterfaceDecl,
    container: Option<SymbolId>,
) {
    let Some(name) = iface.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let iface_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Interface,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, iface.syntax()),
            name_range: name.syntax().text_range(),
            container,
            type_ref: None,
            type_name: None,
            modifiers: ModifierSet::from_modifiers(iface.modifiers()),
        },
    );

    if let Some(extends) = iface.extends() {
        let supertypes: Vec<String> = extends.types().map(|t| t.text()).collect();
        if !supertypes.is_empty() {
            out.raw_extends.push((iface_id, supertypes));
        }
    }

    if let Some(body) = iface.body() {
        for method in body.methods() {
            collect_method(out, file, &method, iface_id);
        }
    }
}

fn collect_enum(
    out: &mut FileCollection,
    file: FileId,
    en: &EnumDecl,
    container: Option<SymbolId>,
) {
    let Some(name) = en.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let enum_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Enum,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, en.syntax()),
            name_range: name.syntax().text_range(),
            container,
            type_ref: None,
            type_name: None,
            modifiers: ModifierSet::from_modifiers(en.modifiers()),
        },
    );

    if let Some(constants) = en.constant_list() {
        for constant in constants.constants() {
            let Some(constant_text) = constant.text() else {
                continue;
            };
            out.push(
                file,
                Symbol {
                    kind: SymbolKind::EnumConstant,
                    name: constant_text,
                    file,
                    ptr: SyntaxPtr::new(file, constant.syntax()),
                    name_range: constant.syntax().text_range(),
                    container: Some(enum_id),
                    type_ref: None,
                    type_name: None,
                    modifiers: ModifierSet::default(),
                },
            );
        }
    }
}

fn collect_member(out: &mut FileCollection, file: FileId, member: &Member, container: SymbolId) {
    match member {
        Member::Method(m) => collect_method(out, file, m, container),
        Member::Constructor(c) => collect_constructor(out, file, c, container),
        Member::Field(f) => collect_field(out, file, f, container),
        Member::Property(p) => collect_property(out, file, p, container),
        Member::NestedClass(c) => collect_class(out, file, c, Some(container)),
        Member::NestedInterface(i) => collect_interface(out, file, i, Some(container)),
        Member::NestedEnum(e) => collect_enum(out, file, e, Some(container)),
    }
}

fn collect_method(out: &mut FileCollection, file: FileId, m: &MethodDecl, container: SymbolId) {
    let Some(name) = m.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let (type_ref, type_name) = type_ptr_and_name(file, m.return_type());
    let method_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Method,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, m.syntax()),
            name_range: name.syntax().text_range(),
            container: Some(container),
            type_ref,
            type_name,
            modifiers: ModifierSet::from_modifiers(m.modifiers()),
        },
    );

    if let Some(params) = m.params() {
        collect_params(out, file, &params, method_id);
    }
}

fn collect_constructor(
    out: &mut FileCollection,
    file: FileId,
    c: &ConstructorDecl,
    container: SymbolId,
) {
    // A constructor's "name" reuses the `Type` slot (must equal its
    // class's name -- see `ConstructorDecl::type_ref`'s doc comment).
    let Some(type_ref) = c.type_ref() else {
        return;
    };
    let ctor_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Constructor,
            name: type_ref.text(),
            file,
            ptr: SyntaxPtr::new(file, c.syntax()),
            name_range: type_ref.syntax().text_range(),
            container: Some(container),
            type_ref: None,
            type_name: None,
            modifiers: ModifierSet::from_modifiers(c.modifiers()),
        },
    );

    if let Some(params) = c.params() {
        collect_params(out, file, &params, ctor_id);
    }
}

fn collect_field(out: &mut FileCollection, file: FileId, f: &FieldDecl, container: SymbolId) {
    let (type_ref, type_name) = type_ptr_and_name(file, f.type_ref());
    for declarator in f.declarators() {
        let Some(name) = declarator.name() else {
            continue;
        };
        let Some(name_text) = name.text() else {
            continue;
        };
        out.push(
            file,
            Symbol {
                kind: SymbolKind::Field,
                name: name_text,
                file,
                ptr: SyntaxPtr::new(file, declarator.syntax()),
                name_range: name.syntax().text_range(),
                container: Some(container),
                type_ref,
                type_name: type_name.clone(),
                modifiers: ModifierSet::from_modifiers(f.modifiers()),
            },
        );
    }
}

fn collect_property(out: &mut FileCollection, file: FileId, p: &PropertyDecl, container: SymbolId) {
    let Some(name) = p.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let (type_ref, type_name) = type_ptr_and_name(file, p.type_ref());
    out.push(
        file,
        Symbol {
            kind: SymbolKind::Property,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, p.syntax()),
            name_range: name.syntax().text_range(),
            container: Some(container),
            type_ref,
            type_name,
            modifiers: ModifierSet::from_modifiers(p.modifiers()),
        },
    );
}

fn collect_params(
    out: &mut FileCollection,
    file: FileId,
    params: &FormalParamList,
    container: SymbolId,
) {
    for param in params.params() {
        let Some(name) = param.name() else {
            continue;
        };
        let Some(name_text) = name.text() else {
            continue;
        };
        let (type_ref, type_name) = type_ptr_and_name(file, param.type_ref());
        out.push(
            file,
            Symbol {
                kind: SymbolKind::Parameter,
                name: name_text,
                file,
                ptr: SyntaxPtr::new(file, param.syntax()),
                name_range: name.syntax().text_range(),
                container: Some(container),
                type_ref,
                type_name,
                modifiers: ModifierSet::default(),
            },
        );
    }
}
