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
}

#[derive(Debug, Default, Clone)]
pub struct ReferenceTable {
    resolutions: FxHashMap<SyntaxPtr, Resolution>,
}

impl ReferenceTable {
    pub(crate) fn set(&mut self, reference: SyntaxPtr, resolution: Resolution) {
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
    /// allocation.
    pub(crate) fn map_ids_into(
        self,
        f: &impl Fn(SymbolId) -> SymbolId,
        target: &mut ReferenceTable,
    ) {
        target.resolutions.reserve(self.resolutions.len());
        target.resolutions.extend(
            self.resolutions
                .into_iter()
                .map(|(ptr, res)| (ptr, res.map_ids(f))),
        );
    }
}
