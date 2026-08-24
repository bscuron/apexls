//! Reference-site resolution outcomes (Pass 2: `crate::resolve`/
//! `crate::soql`) and the table mapping a reference node's [`SyntaxPtr`]
//! to its [`Resolution`].

use crate::ptr::SyntaxPtr;
use crate::symbol::SymbolId;
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
/// - `Unresolved` is reserved for "looked, found nothing at all" --
///   likely a genuine error, or a reference to the (currently
///   unmodeled) Apex standard library.
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

/// `SchemaObject`/`UnknownSchema` box their payload so their two
/// `SmolStr`-carrying fields don't force every other variant -- in
/// particular the by-far-most-common `Resolved`/`Unresolved`, one entry
/// per reference in the whole project -- to pay for the largest
/// variant's size (a real, measured cost: see `examples/mem_profile.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(SymbolId),
    Candidates(Vec<SymbolId>),
    SchemaObject(Box<SchemaObjectRef>),
    UnknownSchema(Box<UnknownSchemaRef>),
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
            Resolution::SchemaObject(_) | Resolution::UnknownSchema(_) | Resolution::Unresolved => {
                &[]
            }
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
}

impl ReferenceTable {
    pub(crate) fn set(&mut self, reference: SyntaxPtr, resolution: Resolution) {
        for &id in resolution.symbol_ids() {
            self.by_symbol.entry(id).or_default().push(reference);
        }
        self.resolutions.insert(reference, resolution);
    }

    pub fn get(&self, reference: SyntaxPtr) -> Option<&Resolution> {
        self.resolutions.get(&reference)
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
        for (ptr, res) in self.resolutions {
            let remapped = res.map_ids(f);
            for &id in remapped.symbol_ids() {
                target.by_symbol.entry(id).or_default().push(ptr);
            }
            target.resolutions.insert(ptr, remapped);
        }
    }
}
