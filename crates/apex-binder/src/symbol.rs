//! The declaration-site data model: one [`Symbol`] per declared class,
//! interface, enum, enum constant, method, constructor, field, property,
//! parameter, or local binding (locals/catch-vars/for-each-vars/switch
//! bindings are added in Pass 2 -- see `crate::resolve` -- since they
//! only exist inside a body, not at declaration-collection time).

use crate::file_id::FileId;
use crate::ptr::{AstPtr, SyntaxPtr};
use apex_syntax::ast::decl::Modifier;
use apex_syntax::ast::Type;
use apex_syntax::SyntaxKind;
use rowan::TextRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub(crate) u32);

impl SymbolId {
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    EnumConstant,
    Trigger,
    Method,
    Constructor,
    Field,
    Property,
    Parameter,
    LocalVar,
    CatchVar,
    ForEachVar,
    SwitchBindingVar,
}

impl SymbolKind {
    /// Whether this symbol can itself contain members (and so can be a
    /// `top_level`/`inherited_chain` entry).
    pub fn is_type(self) -> bool {
        matches!(
            self,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        )
    }
}

/// A declaration's access-level modifier. Defaults to `Private` --
/// Apex's real default for a member with no explicit visibility keyword
/// (top-level types default the same way, though a type with no
/// visibility at all is unusual in practice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Private,
    Protected,
    Public,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    With,
    Without,
    Inherited,
}

/// The parsed-once interpretation of a declaration's `HasModifiers`
/// tokens. This is a semantic reading of syntax (which keyword means
/// "this member is static," not just "here is a `Modifier` node"), so it
/// belongs in the binder rather than in `apex-syntax` -- `apex-syntax`
/// stays a pure structural layer with no opinion on what a modifier
/// keyword *means*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModifierSet {
    pub visibility: Visibility,
    pub is_static: bool,
    pub is_final: bool,
    pub is_virtual: bool,
    pub is_override: bool,
    pub is_abstract: bool,
    pub is_testmethod: bool,
    pub is_transient: bool,
    pub is_webservice: bool,
    pub sharing: Option<Sharing>,
}

impl ModifierSet {
    pub(crate) fn from_modifiers(modifiers: impl Iterator<Item = Modifier>) -> Self {
        let mut set = ModifierSet::default();
        for modifier in modifiers {
            let Some(tok) = modifier.keyword() else {
                continue;
            };
            match tok.kind() {
                SyntaxKind::Public => set.visibility = Visibility::Public,
                SyntaxKind::Protected => set.visibility = Visibility::Protected,
                SyntaxKind::Private => set.visibility = Visibility::Private,
                SyntaxKind::Global => set.visibility = Visibility::Global,
                SyntaxKind::Static => set.is_static = true,
                SyntaxKind::Final => set.is_final = true,
                SyntaxKind::Virtual => set.is_virtual = true,
                SyntaxKind::Override => set.is_override = true,
                SyntaxKind::Abstract => set.is_abstract = true,
                SyntaxKind::Testmethod => set.is_testmethod = true,
                SyntaxKind::Transient => set.is_transient = true,
                SyntaxKind::Webservice => set.is_webservice = true,
                SyntaxKind::With => set.sharing = Some(Sharing::With),
                SyntaxKind::Without => set.sharing = Some(Sharing::Without),
                SyntaxKind::Inherited => set.sharing = Some(Sharing::Inherited),
                _ => {}
            }
        }
        set
    }
}

/// One declared name: a class, interface, enum, enum constant, method,
/// constructor, field, property, parameter, or (added in Pass 2) local
/// binding.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub kind: SymbolKind,
    /// As declared, case preserved -- Apex identifiers are
    /// case-insensitive for lookup purposes (`SymbolTable`'s name maps
    /// are lowercase-keyed), but hover/goto-definition text must show
    /// the real declared spelling.
    pub name: String,
    pub file: FileId,
    /// The whole declaration node (`ClassDecl`, `MethodDecl`, ...).
    /// Untyped (`SyntaxPtr`, not `AstPtr<N>`) because `Symbol` is
    /// deliberately homogeneous across heterogeneous declaration kinds.
    pub ptr: SyntaxPtr,
    /// Just the identifier token's range -- `ptr.range()` covers the
    /// whole declaration (e.g. a method's signature and body), which is
    /// too broad for a goto-definition target.
    pub name_range: TextRange,
    /// The enclosing type or method/constructor this symbol was
    /// declared inside, if any. `None` for a top-level `ClassDecl`/
    /// `InterfaceDecl`/`EnumDecl`/`TriggerUnit`.
    pub container: Option<SymbolId>,
    /// The raw, unresolved declared type (a field/property/parameter/
    /// local's type, or a method's return type). `None` for symbols with
    /// no type of their own (types themselves, enum constants, `void`
    /// methods, constructors).
    pub type_ref: Option<AstPtr<Type>>,
    /// `type_ref`'s dotted name text (`Type::text()`), cached eagerly at
    /// collection time rather than re-derived later via
    /// `type_ref.to_node(root)`. A symbol declared in one file can be
    /// referenced while binding a *different* file (an inherited field
    /// accessed through `this.x` in a subclass elsewhere), and Pass 2
    /// only ever has the file it's currently walking's `SyntaxNode` root
    /// in hand -- re-resolving another file's `AstPtr` would need that
    /// file's root too. Caching the name text once avoids that
    /// cross-file lookup entirely; `type_ref` itself is kept for callers
    /// that *do* have the right root and want the real `Type` node (e.g.
    /// goto-definition on the type reference itself).
    pub type_name: Option<String>,
    pub modifiers: ModifierSet,
}
