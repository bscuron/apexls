//! Reference-site resolution outcomes (Pass 2: `crate::resolve`/
//! `crate::soql`) and the table mapping a reference node's [`SyntaxPtr`]
//! to its [`Resolution`].

use crate::ptr::SyntaxPtr;
use crate::symbol::SymbolId;
use rowan::TextRange;
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

/// What a reference (an unqualified name, a `.member` access, a SOQL
/// object/field path segment, ...) resolved to. Deliberately more than
/// a plain `Option<SymbolId>` -- each variant is a genuinely distinct
/// outcome that must not collapse into another:
///
/// - `Resolved`/`Candidates` distinguish "picked exactly one" from "v1
///   doesn't attempt overload resolution, here's every same-name
///   member" -- collapsing the latter into `Resolved` would silently
///   claim a precision the binder doesn't have.
/// - `SchemaObject` and `UnknownSchema` both mean "this names a
///   Salesforce object/field, not a project-local symbol," but only
///   `SchemaObject` means `apex-metadata` actually has local schema for
///   it. `UnknownSchema` covers standard objects/fields (`Account`,
///   `Contact.Email`, ...) `apex-metadata` has no local metadata for by
///   design (see its module doc comment) -- a real, expected, constant
///   outcome for any repo that touches standard objects, not an error.
/// - `StdlibMember` means the reference names a real standard-library
///   class's method/property (`apex_stdlib::standard_classes`) -- known,
///   not an error, but (like `SchemaObject`/`UnknownSchema`) backed by
///   bundled documentation data rather than a `SymbolId`, so there's no
///   real declaration for goto-definition to point at.
/// - `Unresolved` is reserved for "looked, found nothing at all" --
///   a genuine error (or, before `StdlibMember` existed, *also* covered
///   any reference to the then-unmodeled Apex standard library --
///   that's no longer conflated now that a real stdlib match escapes
///   into `StdlibMember` instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObjectRef {
    pub object: SmolStr,
    pub field: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSchemaRef {
    pub object: Option<SmolStr>,
    pub field: Option<SmolStr>,
}

/// A standard-library class reference, or one of its method/property
/// members -- `member: None` for a bare class name used as a value in
/// its own right (a static-call receiver, e.g. the `String` in
/// `String.isBlank(...)`), mirroring `SchemaObjectRef::field`'s
/// identical `None`-for-the-bare-object-name shape. `member`'s presence
/// alone (no method-vs-property flag) is enough identity for a consumer
/// to look the rest back up via `StdlibIndex`, the same way
/// `SchemaObjectRef` doesn't distinguish a lookup field from a picklist
/// field either.
///
/// `arg_count` is the call site's own argument count for a method call
/// (`None` for a property access or a bare class-name reference, which
/// aren't calls at all) -- carried here specifically so a hover renderer
/// can narrow an overloaded method down to the arity-matching
/// overload(s) instead of always showing every one, the same arity-first
/// signal `crate::resolve::narrow_by_overload`/`narrow_stdlib_overload_type`
/// already use to narrow the *propagated type*. Deliberately just the
/// count, not the argument types themselves: unlike `Ty`, a `usize` is
/// cheap to carry on every `Resolution` and never goes stale relative to
/// the reference it describes, and arity alone already disambiguates the
/// overwhelming majority of real overload sets (different-arity is far
/// more common in the scraped data than same-arity-different-type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibMemberRef {
    pub namespace: Option<SmolStr>,
    pub class_name: SmolStr,
    pub member: Option<SmolStr>,
    pub arg_count: Option<usize>,
}

/// `SchemaObject`/`UnknownSchema`/`StdlibMember` box their payload so
/// their `SmolStr`-carrying fields don't force every other variant -- in
/// particular the by-far-most-common `Resolved`/`Unresolved`, one entry
/// per reference in the whole project -- to pay for the largest
/// variant's size (a real, measured cost: see `examples/mem_profile.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(SymbolId),
    Candidates(Vec<SymbolId>),
    SchemaObject(Box<SchemaObjectRef>),
    UnknownSchema(Box<UnknownSchemaRef>),
    StdlibMember(Box<StdlibMemberRef>),
    Unresolved,
}

impl Resolution {
    /// Applies `f` to every `SymbolId` this resolution references --
    /// used to translate a Pass-2 body's local sentinel ids
    /// (`crate::resolve::LOCAL_SENTINEL_BASE`) into real global ids once
    /// that body's newly-declared locals have been merged into the
    /// shared `SymbolTable` (`f` is the identity for any id that was
    /// already global).
    pub(crate) fn map_ids(self, f: &impl Fn(SymbolId) -> SymbolId) -> Resolution {
        match self {
            Resolution::Resolved(id) => Resolution::Resolved(f(id)),
            Resolution::Candidates(ids) => Resolution::Candidates(ids.into_iter().map(f).collect()),
            other => other,
        }
    }

    /// Every `SymbolId` this resolution touches -- `Resolved`'s one id,
    /// `Candidates`' whole set, nothing for the schema/unresolved
    /// variants (no `SymbolId` to touch). What `ReferenceTable`'s reverse
    /// index (`by_symbol`) is built from: each id here gets this
    /// resolution's `SyntaxPtr` recorded against it.
    fn symbol_ids(&self) -> &[SymbolId] {
        match self {
            Resolution::Resolved(id) => std::slice::from_ref(id),
            Resolution::Candidates(ids) => ids,
            Resolution::SchemaObject(_)
            | Resolution::UnknownSchema(_)
            | Resolution::StdlibMember(_)
            | Resolution::Unresolved => &[],
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReferenceTable {
    resolutions: FxHashMap<SyntaxPtr, Resolution>,
    /// Reverse of `resolutions`: every `SyntaxPtr` whose `Resolution`
    /// touches a given `SymbolId`, maintained incrementally by `set`/
    /// `map_ids_into` as this table is built rather than recomputed at
    /// query time -- the `textDocument/references`/`textDocument/documentHighlight`
    /// lookup (`BACKLOG.md` §3) needs an O(1) hash lookup per file, not
    /// an O(references) scan on every request. Safe to build this way
    /// because a `ReferenceTable` is always populated exactly once, from
    /// empty, per file rebind (`crate::incremental::FileBodies` is
    /// replaced wholesale, never patched in place -- see its doc
    /// comment) -- there's no persistent-across-rebuilds mutation or
    /// stale-entry cleanup to reason about, same as `resolutions` itself.
    by_symbol: FxHashMap<SymbolId, Vec<SyntaxPtr>>,
    /// Narrower highlight range for a reference whose own `SyntaxPtr::range()`
    /// spans more than just its identifying token -- a method/constructor
    /// call node's range covers target-through-closing-paren, a
    /// `FieldExpr`'s covers receiver-through-member. Populated only by
    /// `set_with_highlight` (`MethodCallExpr`/`CallExpr`/`FieldExpr`/a
    /// `NewExpr` constructor call, in `resolve.rs`); every other
    /// reference kind's own node/token range already is just its
    /// identifier, so `highlight_range` falls back to `reference.range()`
    /// itself when this map has no entry. Keyed by the same full-node
    /// `SyntaxPtr` as `resolutions`/`by_symbol`, not by `SymbolId`, so
    /// `map_ids_into` can carry it over with a plain merge -- no
    /// `SymbolId` remapping needed, since nothing here is keyed by one.
    highlight_ranges: FxHashMap<SyntaxPtr, TextRange>,
}

impl ReferenceTable {
    pub(crate) fn set(&mut self, reference: SyntaxPtr, resolution: Resolution) {
        for &id in resolution.symbol_ids() {
            self.by_symbol.entry(id).or_default().push(reference);
        }
        self.resolutions.insert(reference, resolution);
    }

    /// Like [`Self::set`], but additionally records `highlight_range` as
    /// the narrower range `documentHighlight`/`references` should report
    /// for this reference instead of `reference.range()` -- see
    /// `Self::highlight_range`.
    pub(crate) fn set_with_highlight(
        &mut self,
        reference: SyntaxPtr,
        highlight_range: TextRange,
        resolution: Resolution,
    ) {
        self.highlight_ranges.insert(reference, highlight_range);
        self.set(reference, resolution);
    }

    pub fn get(&self, reference: SyntaxPtr) -> Option<&Resolution> {
        self.resolutions.get(&reference)
    }

    /// The narrow identifier range eagerly recorded via `set_with_highlight`
    /// for `reference`, if any -- only ever populated for a call/field-
    /// access-shaped reference (`MethodCallExpr`/`CallExpr`/`FieldExpr`/
    /// `NewExpr`), where the node's own range spans well past the
    /// identifier (target through closing paren, say). Every other
    /// reference kind's narrow range -- including the common `NameExpr`/
    /// `Type`/`QualifiedName` case, where the node range is only ever off
    /// by trailing trivia -- is computed on demand by
    /// `BoundProgram::highlight_range` instead of stored here: those
    /// kinds are common enough in real code that eagerly storing a second
    /// range per reference measurably regressed bind time when tried
    /// (see `crate::resolve::bind_name_expr`'s doc comment).
    pub fn stored_highlight_range(&self, reference: SyntaxPtr) -> Option<TextRange> {
        self.highlight_ranges.get(&reference).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SyntaxPtr, &Resolution)> {
        self.resolutions.iter()
    }

    pub fn len(&self) -> usize {
        self.resolutions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resolutions.is_empty()
    }

    /// Every `SyntaxPtr` whose `Resolution` touches `id` -- an O(1) hash
    /// lookup against `by_symbol`, not a scan over `resolutions`.
    pub fn references_to(&self, id: SymbolId) -> &[SyntaxPtr] {
        self.by_symbol.get(&id).map_or(&[], |v| v.as_slice())
    }

    /// Consumes this table, applying `f` to every `SymbolId` any entry
    /// references (the whole-table counterpart to `Resolution::map_ids`),
    /// and folds the result directly into `target` -- used to merge one
    /// Pass-2 body's reference fragment into the project-wide table
    /// during `BoundProgram::from_files_cached`'s sequential merge.
    /// Remaps and inserts in one pass rather than building a whole
    /// intermediate `ReferenceTable` (a second full `FxHashMap` the same
    /// size as `self`, immediately drained into `target` and dropped) --
    /// see the `hotpath`-measured finding in `BACKLOG.md` §2 that
    /// motivated this: this exact intermediate-then-merge pattern, run
    /// once per body project-wide, was a real share of a cold bind's
    /// allocation. Updates `target.by_symbol` in the same pass, for the
    /// same reason.
    pub(crate) fn map_ids_into(
        self,
        f: &impl Fn(SymbolId) -> SymbolId,
        target: &mut ReferenceTable,
    ) {
        target.resolutions.reserve(self.resolutions.len());
        target.highlight_ranges.extend(self.highlight_ranges);
        for (ptr, res) in self.resolutions {
            let remapped = res.map_ids(f);
            for &id in remapped.symbol_ids() {
                target.by_symbol.entry(id).or_default().push(ptr);
            }
            target.resolutions.insert(ptr, remapped);
        }
    }
}
