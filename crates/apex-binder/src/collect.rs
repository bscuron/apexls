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
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind, Visibility};
use apex_syntax::ast::decl::{
    ClassDecl, CompilationUnit, ConstructorDecl, EnumDecl, FieldDecl, FormalParamList,
    HasModifiers, InterfaceDecl, Member, MethodDecl, PropertyDecl, TriggerUnit, TypeDecl,
};
use apex_syntax::ast::Type;
use rowan::ast::AstNode;
use smol_str::SmolStr;

/// One file's collected declarations, already carrying final, stable
/// `SymbolId`s (see the module doc comment).
///
/// `PartialEq`/`Debug` (ticket 29) solely so `salsa_stage2_dual_run.rs`'s
/// corpus-wide dual-run test can `assert_eq!` two instances directly,
/// matching `SchemaIndex`/`LabelIndex`/`PageIndex`'s own ticket-26
/// precedent.
#[derive(Default, Clone, PartialEq, Debug)]
pub(crate) struct FileCollection {
    pub(crate) symbols: Vec<Symbol>,
    /// `(a type symbol's id, unresolved extends/implements supertype
    /// names as written)`, consumed by Pass 1.5 (`crate::inherit`) once
    /// every file's symbols have been merged into one project-wide
    /// `SymbolTable`, to build the member-lookup `inherited_chain`.
    pub(crate) raw_extends: Vec<(SymbolId, Vec<SmolStr>)>,
    /// `(a *class* symbol's id, its direct `extends` target's unresolved
    /// name)`, present only when that class actually declared `extends`
    /// -- narrower than `raw_extends` (which also folds in
    /// `implements`), kept separate because `super` resolution needs
    /// exactly "the one direct base class," not the whole flattened
    /// member-lookup chain (whose internal ordering doesn't preserve
    /// "which one was `extends`" once interfaces are mixed in).
    pub(crate) raw_super: Vec<(SymbolId, SmolStr)>,
    /// `(the class/interface symbol's id, one `extends`/`implements`
    /// supertype's own `Type` node)` -- one entry per supertype name (so
    /// a class with `implements A, B` gets two entries), consumed by a
    /// declaration-type-resolution step alongside Pass 2 to record a
    /// `Resolution` for each, the same goto-definition support a field's
    /// or parameter's own type reference gets (`Symbol::type_ref`).
    /// Kept separate from `raw_extends`/`raw_super` (which only need
    /// names, for `crate::inherit`'s chain-building) since this is a
    /// flat per-reference list, not grouped by symbol.
    pub(crate) supertype_ptrs: Vec<(SymbolId, AstPtr<Type>)>,
}

impl FileCollection {
    fn push(&mut self, file: FileId, symbol: Symbol) -> SymbolId {
        let id = SymbolId::new(file, self.symbols.len() as u32);
        self.symbols.push(symbol);
        id
    }
}

/// `(pointer, base name, type argument names)` for a declared type
/// reference, e.g. `List<Account>` -> `(ptr, "List", ["Account"])`. Type
/// argument *names* are cached eagerly here, the same reason `type_name`
/// itself is: a symbol declared in one file can be referenced while
/// binding a different file, which only has *that* file's `SyntaxNode`
/// root in hand, not the declaring file's -- re-deriving type arguments
/// from `type_ref.to_node(root)` later wouldn't work in general. Only
/// one level deep (an argument's *own* type arguments, e.g. the inner
/// `Account` of a hypothetical `List<List<Account>>`, aren't captured) --
/// the only shape Apex generics actually have is one level (`List<T>`/
/// `Map<K, V>`/`Set<T>`, never user-defined, never nested more than a
/// project actually chooses to nest collections), so going deeper here
/// wouldn't pay for its own complexity.
pub(crate) fn type_ptr_and_name(
    file: FileId,
    ty: Option<Type>,
) -> (Option<AstPtr<Type>>, Option<SmolStr>, Vec<SmolStr>) {
    match ty {
        Some(ty) => {
            let base_name = ty.text();
            // Legacy `Type[]` array sugar (`String[]`, `Object[]`, ...) is
            // Apex's own shorthand for `List<Type>` -- interchangeable
            // right down to overload resolution (real NPSP shape:
            // `UTIL_Query.withSelectFields` overloaded on `Set<String>`
            // vs. `String[]`, only disambiguable if `String[]` is compared
            // as `List<String>`, not as bare `String`). `ty.text()`/
            // `type_args()` already strip the `[]` suffix (see their own
            // doc comments) and it carries no `<...>` of its own, so
            // without this an array-sugared declared type cached here as
            // its bare element name -- indistinguishable from a genuinely
            // non-generic declaration of that same name. Applies the same
            // way one level down: a *type argument* can itself be
            // array-sugared (real NPSP shape: `Map<String, SObject[]>`),
            // and without this its name would collapse to the bare element
            // name (`SObject`) there too -- indistinguishable from a
            // `Map<String, SObject>` declaration, which is a real, different
            // type (confirmed false positive: `AdditionalObjectJSON_TEST`'s
            // `Map<String,SObject[]> widgetData = new
            // Map<String,SObject[]>();` was flagged as assigning a
            // `Map<String, List<SObject>>` value to a `Map<String,
            // SObject>`-typed variable before this).
            let args = ty
                .type_args()
                .map(|list| {
                    list.args()
                        .map(|a| if a.is_array() { SmolStr::new_static("List") } else { a.text() })
                        .collect()
                })
                .unwrap_or_default();
            if ty.is_array() {
                (Some(AstPtr::new(file, &ty)), Some(SmolStr::new_static("List")), vec![base_name])
            } else {
                (Some(AstPtr::new(file, &ty)), Some(base_name), args)
            }
        }
        None => (None, None, Vec::new()),
    }
}

/// Collects every declaration in one parsed `.cls` `CompilationUnit`.
#[hotpath::measure]
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
#[hotpath::measure]
pub(crate) fn collect_trigger_unit(file: FileId, tu: &TriggerUnit) -> FileCollection {
    let mut out = FileCollection::default();
    let Some(name) = tu.name() else {
        return out;
    };
    let Some(name_text) = name.text() else {
        return out;
    };
    // The trigger's own `ON <object>` reference, reused as `type_name`
    // purely to carry the declared object's name through to
    // `crate::resolve::BodyBinder`'s `Trigger.new`/`.old`/`.newMap`/
    // `.oldMap` narrowing (see its own doc comment) -- not a real
    // "declared type" the way it is for a field/parameter, but the field
    // already exists on every `Symbol` for exactly this "declared type
    // text" purpose, so no new field is needed.
    let object_name = tu.object_ref().map(|t| SmolStr::new(t.text()));
    let trigger_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Trigger,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, tu.syntax()),
            name_range: name.ident_range(),
            container: None,
            type_ref: None,
            type_name: object_name,
            type_args: Vec::new(),
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
            name_range: name.ident_range(),
            container,
            type_ref: None,
            type_name: None,
            type_args: Vec::new(),
            modifiers: ModifierSet::from_modifiers(class.modifiers()),
        },
    );

    let mut supertypes = Vec::new();
    if let Some(extends) = class.extends() {
        out.raw_super.push((class_id, extends.text()));
        out.supertype_ptrs
            .push((class_id, AstPtr::new(file, &extends)));
        supertypes.push(extends.text());
    }
    if let Some(implements) = class.implements() {
        for t in implements.types() {
            out.supertype_ptrs.push((class_id, AstPtr::new(file, &t)));
            supertypes.push(t.text());
        }
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
            name_range: name.ident_range(),
            container,
            type_ref: None,
            type_name: None,
            type_args: Vec::new(),
            modifiers: ModifierSet::from_modifiers(iface.modifiers()),
        },
    );

    if let Some(extends) = iface.extends() {
        let mut supertypes = Vec::new();
        for t in extends.types() {
            out.supertype_ptrs.push((iface_id, AstPtr::new(file, &t)));
            supertypes.push(t.text());
        }
        if !supertypes.is_empty() {
            out.raw_extends.push((iface_id, supertypes));
        }
    }

    if let Some(body) = iface.body() {
        for method in body.methods() {
            collect_method(out, file, &method, iface_id, true);
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
    let modifiers = ModifierSet::from_modifiers(en.modifiers());
    let enum_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Enum,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, en.syntax()),
            name_range: name.ident_range(),
            container,
            type_ref: None,
            type_name: None,
            type_args: Vec::new(),
            modifiers,
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
                    name_range: constant.ident_range(),
                    container: Some(enum_id),
                    type_ref: None,
                    type_name: None,
                    type_args: Vec::new(),
                    // Apex gives an enum's *values* no visibility syntax
                    // of their own at all -- `enum Integration { A, B }`
                    // has no per-constant modifier to write -- so a
                    // constant is visible exactly wherever the enum
                    // *type* itself is, never independently `Private`
                    // (`ModifierSet::default()`'s default, correct for a
                    // genuinely-unmarked member but wrong here: it made
                    // `is_visible_from` reject every enum constant
                    // referenced from outside the enum's own top-level
                    // declaring type, e.g. `Outer.SomeEnum.VALUE` used
                    // from any other class).
                    modifiers,
                },
            );
        }
    }
}

fn collect_member(out: &mut FileCollection, file: FileId, member: &Member, container: SymbolId) {
    match member {
        Member::Method(m) => collect_method(out, file, m, container, false),
        Member::Constructor(c) => collect_constructor(out, file, c, container),
        Member::Field(f) => collect_field(out, file, f, container),
        Member::Property(p) => collect_property(out, file, p, container),
        Member::NestedClass(c) => collect_class(out, file, c, Some(container)),
        Member::NestedInterface(i) => collect_interface(out, file, i, Some(container)),
        Member::NestedEnum(e) => collect_enum(out, file, e, Some(container)),
    }
}

fn collect_method(
    out: &mut FileCollection,
    file: FileId,
    m: &MethodDecl,
    container: SymbolId,
    in_interface: bool,
) {
    let Some(name) = m.name() else {
        return;
    };
    let Some(name_text) = name.text() else {
        return;
    };
    let (type_ref, type_name, type_args) = type_ptr_and_name(file, m.return_type());
    let mut modifiers = ModifierSet::from_modifiers_and_annotations(m.modifiers(), m.annotations());
    if in_interface && modifiers.visibility == Visibility::Private {
        // Apex interface methods can't carry an explicit access modifier
        // at all -- every interface member is implicitly public, unlike
        // a class member's modifier-less default, which really is
        // Private (Apex's real default there, see `Visibility`'s doc
        // comment). Only overrides the untouched default, never an
        // explicit modifier the grammar might permit (e.g. a `global
        // interface`'s methods).
        modifiers.visibility = Visibility::Public;
    }
    let method_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Method,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, m.syntax()),
            name_range: name.ident_range(),
            container: Some(container),
            type_ref,
            type_name,
            type_args,
            modifiers,
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
    // class's name -- see `ConstructorDecl::type_ref`'s doc comment). A
    // constructor's own name is never dotted, so its last (only)
    // `base_name_tokens()` entry is exactly the identifier -- same fix as
    // `Name::ident_range()`, `Type` has no single-token wrapper of its
    // own to hang an equivalent method off of, so this is inlined once
    // here rather than adding a whole new accessor for one call site.
    let Some(type_ref) = c.type_ref() else {
        return;
    };
    let name_range = type_ref
        .base_name_tokens()
        .last()
        .map(|t| t.text_range())
        .unwrap_or_else(|| type_ref.syntax().text_range());
    let ctor_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Constructor,
            name: type_ref.text(),
            file,
            ptr: SyntaxPtr::new(file, c.syntax()),
            name_range,
            container: Some(container),
            type_ref: None,
            type_name: None,
            type_args: Vec::new(),
            modifiers: ModifierSet::from_modifiers_and_annotations(c.modifiers(), c.annotations()),
        },
    );

    if let Some(params) = c.params() {
        collect_params(out, file, &params, ctor_id);
    }
}

fn collect_field(out: &mut FileCollection, file: FileId, f: &FieldDecl, container: SymbolId) {
    let (type_ref, type_name, type_args) = type_ptr_and_name(file, f.type_ref());
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
                name_range: name.ident_range(),
                container: Some(container),
                type_ref,
                type_name: type_name.clone(),
                type_args: type_args.clone(),
                modifiers: ModifierSet::from_modifiers_and_annotations(f.modifiers(), f.annotations()),
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
    let (type_ref, type_name, type_args) = type_ptr_and_name(file, p.type_ref());
    let property_id = out.push(
        file,
        Symbol {
            kind: SymbolKind::Property,
            name: name_text,
            file,
            ptr: SyntaxPtr::new(file, p.syntax()),
            name_range: name.ident_range(),
            container: Some(container),
            type_ref,
            type_name: type_name.clone(),
            type_args: type_args.clone(),
            modifiers: ModifierSet::from_modifiers_and_annotations(p.modifiers(), p.annotations()),
        },
    );
    // A custom `set { ... }` accessor body can reference `value`, an
    // implicit parameter of the property's own type that Apex declares
    // for it -- never written in source (real NPSP shape:
    // `fflib_ApexMocks.DoThrowWhenExceptions`'s setter assigning
    // `methodReturnValueRecorder.DoThrowWhenExceptions = value;`). Modeled
    // as an ordinary `Parameter` symbol so `SymbolKind::Property`'s own
    // body-binding pass (`crate::lib`) can seed it into the setter body's
    // scope exactly like a real method parameter, via `table.params`
    // (which already filters `members_of(container)` down to `Parameter`
    // kind) -- keyed under the *property's* id as container, not the
    // enclosing class's, so it stays invisible to ordinary member lookup
    // and arity checks on the class itself. Skipped for the common
    // `set;` auto-implemented form (no body to bind at all) and for a
    // `get` accessor (which has no `value` of its own).
    for accessor in p.accessors() {
        if !accessor.is_setter() {
            continue;
        }
        let Some(body) = accessor.body() else {
            continue;
        };
        let name_range = accessor
            .keyword_range()
            .unwrap_or_else(|| accessor.syntax().text_range());
        out.push(
            file,
            Symbol {
                kind: SymbolKind::Parameter,
                name: SmolStr::new_static("value"),
                file,
                ptr: SyntaxPtr::new(file, body.syntax()),
                name_range,
                container: Some(property_id),
                type_ref,
                type_name: type_name.clone(),
                type_args: type_args.clone(),
                modifiers: ModifierSet::default(),
            },
        );
    }
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
        let (type_ref, type_name, type_args) = type_ptr_and_name(file, param.type_ref());
        out.push(
            file,
            Symbol {
                kind: SymbolKind::Parameter,
                name: name_text,
                file,
                ptr: SyntaxPtr::new(file, param.syntax()),
                name_range: name.ident_range(),
                container: Some(container),
                type_ref,
                type_name,
                type_args,
                modifiers: ModifierSet::default(),
            },
        );
    }
}
