//! The whole-project declaration table: every [`Symbol`] collected in
//! Pass 1 (`crate::collect`), stored per file, plus the name/containment
//! indices Pass 1.5 (`crate::inherit`) and Pass 2 (`crate::resolve`)
//! look things up through.
//!
//! Storage is `by_file`-backed rather than one flat `Vec<Symbol>` because
//! `SymbolId` is now `{file, local}` (see `crate::symbol::SymbolId`'s
//! doc comment) -- a stable identity that survives across incremental
//! rebuilds. That stability is what lets [`SymbolTable`] itself persist
//! across `BoundProgram::from_files_cached` calls (owned by
//! `crate::BindCache`) and be patched file-by-file (`set_file_symbols`)
//! instead of rebuilt from nothing every call: an edit to file A can
//! replace *only* A's slice of `by_file` and leave every other file's
//! symbols, and every derived-index entry that only involves them,
//! completely untouched.
//!
//! **Every field is `Arc`-wrapped** (`by_file`'s values, and the whole
//! `Indices` bundle) specifically so `SymbolTable: Clone` -- needed once
//! per `BoundProgram::from_files_cached` call to hand the caller an
//! independent, owned snapshot -- is a bunch of refcount bumps, not a
//! deep copy of every `Symbol`'s `String` fields and every index entry.
//! An earlier plain (non-`Arc`) version of this type made that per-call
//! clone cost *more* than the incremental rebind it was supposed to make
//! cheap (measured: `corpus/warm_rebind_after_one_file_edit` regressed
//! from ~438ms to ~700ms) -- `Arc::make_mut`'s copy-on-write means a
//! file that's actually being patched this call still pays to clone (if
//! anything else -- e.g. a still-alive previous `BoundProgram` snapshot
//! -- holds a reference to its old `Arc`), but every *untouched* file's
//! data and the whole derived-index bundle (when declarations didn't
//! change project-wide) cost nothing but a pointer copy.
//!
//! The derived indices (`members_of`, `members_by_name`, `top_level`,
//! `by_name_ci`) are the exception to "patch file-by-file" -- they're
//! project-wide by nature (a name can be looked up against the whole
//! namespace), so they're not patched incrementally. Instead
//! [`SymbolTable::rebuild_indices`] throws the whole [`Indices`] bundle
//! away and rebuilds it from whatever's currently in `by_file` in one
//! pass, replacing it with a fresh `Arc`. That's still cheap relative to
//! parsing/binding (plain in-memory iteration over already-built
//! `Symbol` structs, no tree-walking) -- callers only need to pay for it
//! when a file's *declared shape* actually changed, not on every rebuild
//! (see `BoundProgram::from_files_cached`).

use crate::file_id::FileId;
use crate::symbol::{Symbol, SymbolId};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
struct Indices {
    members_of: HashMap<SymbolId, Vec<SymbolId>>,
    /// `(container, lowercase member name) -> every member of `container`
    /// with that name`. Exists purely so `lookup_member` -- called once
    /// per name reference anywhere in the program, the single hottest
    /// path in Pass 2 -- is an O(1)-average hash lookup per type in the
    /// inheritance chain instead of an O(members-of-that-type) linear
    /// string-comparison scan. Without this index, a large class whose
    /// methods reference each other extensively (its reference count
    /// scaling with its own member count M) would cost O(M) per lookup
    /// times O(M) lookups, i.e. trend toward O(M^2) in aggregate for
    /// that one class -- exactly the kind of accidental quadratic
    /// behavior a hash-indexed lookup avoids.
    members_by_name: HashMap<(SymbolId, String), Vec<SymbolId>>,
    /// Lowercase declared type name -> its `SymbolId`, project-wide (not
    /// per-file): unlike most languages this binder targets, Apex has no
    /// import/package system -- every top-level class/interface/enum in
    /// an org is visible from every other file unqualified, so a single
    /// flat namespace is the semantically correct model here, not a
    /// simplification. A real org would reject a genuine duplicate
    /// top-level name at deploy time; this binder resolves such a
    /// (necessarily already-invalid) conflict to whichever declaration is
    /// visited first during `rebuild_indices` (files visited in `FileId`
    /// order, for determinism) rather than trying to detect and report
    /// it -- out of scope for a binder whose job is resolution, not
    /// diagnostics, in v1.
    top_level: HashMap<String, SymbolId>,
    /// Lowercase simple name -> every symbol (type or member) with that
    /// name, project-wide. Backs `workspace/symbol`-style lookups and is
    /// the fallback candidate set for reference resolution that can't
    /// narrow further.
    by_name_ci: HashMap<String, Vec<SymbolId>>,
    /// Populated by Pass 1.5 (`crate::inherit`): each type symbol's own
    /// id followed by every `SymbolId` transitively reachable through
    /// `extends`/`implements`, cycle-guarded. Absent (empty slice) for
    /// non-type symbols and for types Pass 1.5 hasn't processed yet.
    inherited_chain: HashMap<SymbolId, Vec<SymbolId>>,
    /// Populated by Pass 1.5: a class symbol's direct `extends` target
    /// only (never an `implements` target), for `super`/`super(...)`
    /// resolution -- narrower than `inherited_chain`, whose flattened
    /// order doesn't reliably preserve "which ancestor was the direct
    /// base class" once interfaces are mixed in.
    direct_super: HashMap<SymbolId, SymbolId>,
}

#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    by_file: HashMap<FileId, Arc<Vec<Symbol>>>,
    indices: Arc<Indices>,
}

impl SymbolTable {
    /// Replaces `file`'s entire slice of declared symbols (Pass 1's
    /// per-file output, already carrying final `SymbolId`s -- see
    /// `crate::collect`'s module doc comment). Doesn't touch any derived
    /// index; call [`Self::rebuild_indices`] afterward once every dirty
    /// file's symbols have been set for this rebuild.
    pub(crate) fn set_file_symbols(&mut self, file: FileId, symbols: Vec<Symbol>) {
        self.by_file.insert(file, Arc::new(symbols));
    }

    /// Drops `file`'s symbols entirely (a file removed from the project
    /// since the last rebuild). Same caller contract as
    /// [`Self::set_file_symbols`]: call [`Self::rebuild_indices`]
    /// afterward.
    pub(crate) fn remove_file(&mut self, file: FileId) {
        self.by_file.remove(&file);
    }

    /// Appends `extra` (Pass 2's newly-declared locals for bodies in
    /// `file`, already remapped to real `SymbolId`s -- see
    /// `crate::resolve::remap_local_id`) after `file`'s existing
    /// declared symbols. Deliberately doesn't touch any derived index --
    /// unlike Pass 1 declarations, a local variable is never looked up
    /// through `members_of`/`by_name_ci`/`top_level` (lexical-scope
    /// lookup via `crate::scope::ScopeTree` handles locals entirely
    /// separately), so there's nothing for those indices to gain from
    /// including it. Always called right after [`Self::set_file_symbols`]
    /// for the same `file` within one `BoundProgram::from_files_cached`
    /// call, so the `Arc::make_mut` below never actually clones in
    /// practice -- nothing else has had a chance to clone this file's
    /// brand-new `Arc` yet.
    pub(crate) fn append_file_symbols(&mut self, file: FileId, mut extra: Vec<Symbol>) {
        let entry = self.by_file.entry(file).or_default();
        Arc::make_mut(entry).append(&mut extra);
    }

    pub(crate) fn has_file(&self, file: FileId) -> bool {
        self.by_file.contains_key(&file)
    }

    pub(crate) fn known_files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.by_file.keys().copied()
    }

    /// Rebuilds every project-wide derived index (`members_of`,
    /// `members_by_name`, `top_level`, `by_name_ci`) from scratch against
    /// whatever's currently in `by_file`, replacing [`Self`]'s whole
    /// [`Indices`] bundle with a fresh `Arc` in one atomic swap. Does
    /// **not** touch `inherited_chain`/`direct_super` -- those are Pass
    /// 1.5's job (`crate::inherit::resolve_inheritance`), run separately
    /// by the caller only when this rebuild indicates declarations
    /// actually changed.
    pub(crate) fn rebuild_indices(&mut self) {
        let mut fresh = Indices::default();

        let mut files: Vec<FileId> = self.by_file.keys().copied().collect();
        files.sort();
        for file in files {
            let Some(symbols) = self.by_file.get(&file) else {
                continue;
            };
            for (local, symbol) in symbols.iter().enumerate() {
                let id = SymbolId::new(file, local as u32);
                let lower = symbol.name.to_ascii_lowercase();

                fresh.by_name_ci.entry(lower.clone()).or_default().push(id);
                if symbol.container.is_none() && symbol.kind.is_type() {
                    fresh.top_level.entry(lower.clone()).or_insert(id);
                }
                if let Some(container) = symbol.container {
                    fresh.members_of.entry(container).or_default().push(id);
                    fresh
                        .members_by_name
                        .entry((container, lower))
                        .or_default()
                        .push(id);
                }
            }
        }
        self.indices = Arc::new(fresh);
    }

    pub fn get(&self, id: SymbolId) -> &Symbol {
        &self.by_file[&id.file][id.local as usize]
    }

    /// Every symbol declared in `file`, in declaration order (so a
    /// symbol's position in this slice, cast to `u32`, is exactly its
    /// `SymbolId::local`).
    pub(crate) fn symbols_of_file(&self, file: FileId) -> &[Symbol] {
        self.by_file.get(&file).map_or(&[], |v| v.as_slice())
    }

    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &Symbol)> {
        self.by_file.iter().flat_map(|(&file, symbols)| {
            symbols
                .iter()
                .enumerate()
                .map(move |(i, s)| (SymbolId::new(file, i as u32), s))
        })
    }

    pub fn len(&self) -> usize {
        self.by_file.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_file.values().all(|v| v.is_empty())
    }

    /// Direct (non-inherited) members declared on `container`.
    pub fn members_of(&self, container: SymbolId) -> &[SymbolId] {
        self.indices
            .members_of
            .get(&container)
            .map_or(&[], |v| v.as_slice())
    }

    pub fn top_level(&self, name: &str) -> Option<SymbolId> {
        self.indices
            .top_level
            .get(&name.to_ascii_lowercase())
            .copied()
    }

    pub fn by_name_ci(&self, name: &str) -> &[SymbolId] {
        self.indices
            .by_name_ci
            .get(&name.to_ascii_lowercase())
            .map_or(&[], |v| v.as_slice())
    }

    pub(crate) fn set_inherited_chain(&mut self, id: SymbolId, chain: Vec<SymbolId>) {
        Arc::make_mut(&mut self.indices)
            .inherited_chain
            .insert(id, chain);
    }

    pub fn inherited_chain(&self, id: SymbolId) -> &[SymbolId] {
        self.indices
            .inherited_chain
            .get(&id)
            .map_or(&[], |v| v.as_slice())
    }

    pub(crate) fn set_direct_super(&mut self, id: SymbolId, super_id: SymbolId) {
        Arc::make_mut(&mut self.indices)
            .direct_super
            .insert(id, super_id);
    }

    pub fn direct_super(&self, id: SymbolId) -> Option<SymbolId> {
        self.indices.direct_super.get(&id).copied()
    }

    /// Every `Parameter` symbol directly contained by `container` (a
    /// `Method`/`Constructor`), in declaration order -- used both to
    /// seed a body's root scope (`crate::resolve`) and to check a call's
    /// arity against a candidate during overload resolution.
    pub fn params(&self, container: SymbolId) -> Vec<SymbolId> {
        self.members_of(container)
            .iter()
            .copied()
            .filter(|&id| self.get(id).kind == crate::symbol::SymbolKind::Parameter)
            .collect()
    }

    /// Every member named `name` (case-insensitive) directly on
    /// `type_id` or anywhere in its resolved `extends`/`implements`
    /// chain -- the candidate set an unqualified member reference
    /// resolves against. Declaration order is preserved within each
    /// type visited, self before ancestors. An O(1)-average hash lookup
    /// per type in the chain (via `members_by_name`), not a linear scan
    /// over that type's members -- see `members_by_name`'s doc comment
    /// for why that distinction matters (this is Pass 2's single hottest
    /// path, called once per name reference in the whole program).
    pub fn lookup_member(&self, type_id: SymbolId, name: &str) -> Vec<SymbolId> {
        let lower = name.to_ascii_lowercase();
        let mut found = Vec::new();
        for chain_id in
            std::iter::once(type_id).chain(self.inherited_chain(type_id).iter().copied())
        {
            if let Some(members) = self.indices.members_by_name.get(&(chain_id, lower.clone())) {
                found.extend(members.iter().copied());
            }
        }
        found
    }
}
