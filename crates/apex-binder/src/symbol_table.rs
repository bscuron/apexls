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

use crate::ci_key::{CiKey, CiMap, CiQuery};
use crate::file_id::FileId;
use crate::symbol::{Symbol, SymbolId, Visibility};
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use smol_str::SmolStr;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A `members_by_name` lookup query -- `(container, member name)`, the
/// borrowed-name counterpart to that map's stored `(SymbolId, CiKey)` key.
/// Can't reuse a bare tuple for this the way [`CiQuery`] alone works for
/// the single-string maps: `impl hashbrown::Equivalent<..> for (T, CiQuery)`
/// is rejected by Rust's orphan rule (tuples are always a foreign type,
/// regardless of what their fields are), so this small local wrapper
/// exists purely to give the query side a type this crate actually owns.
struct MemberQuery<'a>(SymbolId, CiQuery<'a>);

impl Hash for MemberQuery<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
        self.1.hash(state);
    }
}

impl hashbrown::Equivalent<(SymbolId, CiKey)> for MemberQuery<'_> {
    fn equivalent(&self, key: &(SymbolId, CiKey)) -> bool {
        self.0 == key.0 && self.1.equivalent(&key.1)
    }
}

#[derive(Debug, Default, Clone)]
struct Indices {
    members_of: FxHashMap<SymbolId, Vec<SymbolId>>,
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
    members_by_name: hashbrown::HashMap<(SymbolId, CiKey), Vec<SymbolId>, FxBuildHasher>,
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
    top_level: CiMap<SymbolId>,
    /// Lowercase simple name -> every symbol (type or member) with that
    /// name, project-wide. Backs `workspace/symbol`-style lookups and is
    /// the fallback candidate set for reference resolution that can't
    /// narrow further.
    by_name_ci: CiMap<Vec<SymbolId>>,
    /// Populated by Pass 1.5 (`crate::inherit`): each type symbol's own
    /// id followed by every `SymbolId` transitively reachable through
    /// `extends`/`implements`, cycle-guarded. Absent (empty slice) for
    /// non-type symbols and for types Pass 1.5 hasn't processed yet.
    inherited_chain: FxHashMap<SymbolId, Vec<SymbolId>>,
    /// Populated by Pass 1.5: a class symbol's direct `extends` target
    /// only (never an `implements` target), for `super`/`super(...)`
    /// resolution -- narrower than `inherited_chain`, whose flattened
    /// order doesn't reliably preserve "which ancestor was the direct
    /// base class" once interfaces are mixed in.
    direct_super: FxHashMap<SymbolId, SymbolId>,
    /// Populated by Pass 1.5: a class symbol's own raw `extends` name,
    /// but *only* when that name failed to resolve against this project's
    /// `SymbolTable` at all (`direct_super` above stays empty for that
    /// same symbol in that case -- the two are mutually exclusive per
    /// symbol). The overwhelmingly common reason: the name is real, but
    /// external (`extends Exception`, `extends DmlException`, ...) --
    /// `crate::inherit::resolve_inheritance`'s own doc comment already
    /// covers why an unresolvable supertype otherwise "simply drops"
    /// rather than aborting the chain. Kept, rather than discarded the
    /// way the rest of Pass 1's raw name strings are once Pass 1.5 is
    /// done with them, specifically so a project type's own member lookup
    /// (`crate::resolve::bind_method_call_expr`'s `Ty::Project` arm) can
    /// still fall back to a *real, documented* standard-library class of
    /// this exact name when this project's own inheritance chain has
    /// nothing by this member name -- see that fallback's own doc
    /// comment for the motivating case (a custom exception subclass
    /// calling an inherited `Exception` method like `setMessage`).
    unresolved_direct_super: FxHashMap<SymbolId, SmolStr>,
    /// Populated by Pass 1.5: the reverse of `inherited_chain` -- for a
    /// type symbol that has at least one, every `SymbolId` transitively
    /// `extends`/`implements`-ing *it*, cycle-guarded the same way.
    /// Backs dynamic-dispatch widening (`crate::resolve`'s
    /// `expand_dynamic_dispatch`): a call resolved against a
    /// `virtual`/`abstract`/interface method declared on this type could,
    /// at runtime, actually run any subtype's own same-name/arity
    /// override instead, so a reference credited to the statically-
    /// resolved declaration alone would under-count real callers of
    /// whichever subtype override actually executes. Absent (empty
    /// slice) for a type with no known subtypes, which is by far the
    /// common case -- most declared types are never extended/implemented
    /// at all.
    subtypes: FxHashMap<SymbolId, Vec<SymbolId>>,
}

#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    by_file: FxHashMap<FileId, Arc<Vec<Symbol>>>,
    /// Each file's declared-symbol count as of its last [`Self::set_file_symbols`]
    /// call -- i.e. `by_file[file]`'s length *before* any
    /// [`Self::append_file_symbols`] call added that rebind's locals on
    /// top. Needed because Pass 2 can rebind a file's bodies (and so call
    /// `append_file_symbols` for it again) on a call where that file
    /// *wasn't* Pass-1-dirty (`BoundProgram::from_files_cached`'s
    /// conservative "some other file's declarations changed, rebind
    /// every file's bodies" fallback) -- without this, `append_file_symbols`
    /// would have no way to tell "already has last call's locals on the
    /// end" from "freshly declared, no locals yet" and would append onto
    /// the previous call's locals instead of replacing them, growing
    /// `by_file[file]` without bound over repeated calls.
    declared_len: FxHashMap<FileId, usize>,
    indices: Arc<Indices>,
}

impl SymbolTable {
    /// Replaces `file`'s entire slice of declared symbols (Pass 1's
    /// per-file output, already carrying final `SymbolId`s -- see
    /// `crate::collect`'s module doc comment). Doesn't touch any derived
    /// index; call [`Self::rebuild_indices`] afterward once every dirty
    /// file's symbols have been set for this rebuild.
    pub(crate) fn set_file_symbols(&mut self, file: FileId, symbols: Vec<Symbol>) {
        self.declared_len.insert(file, symbols.len());
        self.by_file.insert(file, Arc::new(symbols));
    }

    /// Drops `file`'s symbols entirely (a file removed from the project
    /// since the last rebuild). Same caller contract as
    /// [`Self::set_file_symbols`]: call [`Self::rebuild_indices`]
    /// afterward.
    pub(crate) fn remove_file(&mut self, file: FileId) {
        self.by_file.remove(&file);
        self.declared_len.remove(&file);
    }

    /// Appends `extra` (Pass 2's newly-declared locals for bodies in
    /// `file`, already remapped to real `SymbolId`s -- see
    /// `crate::resolve::remap_local_id`) after `file`'s declared symbols,
    /// first truncating away any locals a *previous* call already
    /// appended (see [`Self::declared_len`]'s doc comment -- this file
    /// need not have gone through [`Self::set_file_symbols`] this call
    /// for that stale tail to exist). Deliberately doesn't touch any
    /// derived index -- unlike Pass 1 declarations, a local variable is
    /// never looked up through `members_of`/`by_name_ci`/`top_level`
    /// (lexical-scope lookup via `crate::scope::ScopeTree` handles locals
    /// entirely separately), so there's nothing for those indices to gain
    /// from including it.
    pub(crate) fn append_file_symbols(&mut self, file: FileId, mut extra: Vec<Symbol>) {
        let declared_len = self.declared_len.get(&file).copied();
        let entry = self.by_file.entry(file).or_default();
        let owned = Arc::make_mut(entry);
        if let Some(declared_len) = declared_len {
            owned.truncate(declared_len);
        }
        owned.append(&mut extra);
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
    /// [`Indices`] bundle with a fresh `Arc` in one atomic swap.
    /// `inherited_chain`/`direct_super`/`unresolved_direct_super`/
    /// `subtypes` are **carried forward unchanged** from the previous
    /// bundle rather than reset -- those are Pass 1.5's job
    /// (`crate::inherit::resolve_inheritance`), which the caller may or
    /// may not run right after this (Wayfinder `apex-diagnostics` map,
    /// ticket 30/31: `resolve_inheritance` now has its own, narrower
    /// trigger than this method's `declarations_changed` gate, so the two
    /// no longer always run back to back the way they used to). Carrying
    /// the old values forward is what makes that safe: whenever this
    /// rebuild's caller decides the inheritance data is still valid and
    /// skips `resolve_inheritance`, that still-valid data survives this
    /// call instead of silently going empty. When the caller *does* run
    /// `resolve_inheritance` right after, it overwrites these same
    /// fields with fresh values via `set_inherited_chain`/`set_direct_super`/
    /// `set_unresolved_direct_super`/`set_subtypes`, exactly as before --
    /// this carry-forward is invisible to that path.
    pub(crate) fn rebuild_indices(&mut self) {
        let mut fresh = Indices {
            inherited_chain: self.indices.inherited_chain.clone(),
            direct_super: self.indices.direct_super.clone(),
            unresolved_direct_super: self.indices.unresolved_direct_super.clone(),
            subtypes: self.indices.subtypes.clone(),
            ..Indices::default()
        };

        let mut files: Vec<FileId> = self.by_file.keys().copied().collect();
        files.sort();
        for file in files {
            let Some(symbols) = self.by_file.get(&file) else {
                continue;
            };
            // Declared symbols only (`declared_symbols_of_file`, not the
            // full `by_file[file]`) -- a file's locals tail is truncated
            // and re-appended by `append_file_symbols` on *every*
            // body-only rebind, independent of this method, which only
            // reruns when a *declaration* changed project-wide. Indexing
            // a local's `SymbolId` here would let `members_of`/
            // `members_by_name` hold one that a later, index-rebuild-free
            // `append_file_symbols` call truncates away entirely --
            // exactly the stale-id-past-the-end-of-`by_file[file]` shape
            // that made `SymbolTable::get` panic (`local`s never belong
            // in these indices anyway, per `append_file_symbols`'s own
            // doc comment: lexical-scope lookup via `crate::scope::ScopeTree`
            // handles locals entirely separately).
            let declared_len = self.declared_len.get(&file).copied().unwrap_or(symbols.len());
            for (local, symbol) in symbols.iter().enumerate().take(declared_len) {
                let id = SymbolId::new(file, local as u32);
                let key = CiKey::from(symbol.name.as_str());

                fresh.by_name_ci.entry(key.clone()).or_default().push(id);
                if symbol.container.is_none() && symbol.kind.is_type() {
                    fresh.top_level.entry(key.clone()).or_insert(id);
                }
                if let Some(container) = symbol.container {
                    fresh.members_of.entry(container).or_default().push(id);
                    fresh
                        .members_by_name
                        .entry((container, key))
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
    /// `SymbolId::local`). Includes any locals a prior [`Self::append_file_symbols`]
    /// call appended on top -- use [`Self::declared_symbols_of_file`]/
    /// [`Self::declared_len`] instead when only the Pass-1 declared
    /// portion is wanted.
    pub(crate) fn symbols_of_file(&self, file: FileId) -> &[Symbol] {
        self.by_file.get(&file).map_or(&[], |v| v.as_slice())
    }

    /// `file`'s declared symbols only -- excludes any locals a prior
    /// [`Self::append_file_symbols`] call appended, unlike
    /// [`Self::symbols_of_file`]. This is what a fresh Pass 1 collection
    /// for `file` must be compared against to detect a real declaration
    /// change; comparing against [`Self::symbols_of_file`] instead would
    /// spuriously "detect" a change every time purely because that file
    /// carries locals from its last bind and Pass 1 output never does.
    pub(crate) fn declared_symbols_of_file(&self, file: FileId) -> &[Symbol] {
        let len = self.declared_len(file);
        self.by_file.get(&file).map_or(&[], |v| &v[..len])
    }

    /// `file`'s declared-symbol count as of its last [`Self::set_file_symbols`]
    /// call (`0` for a file that's never had one). See the `declared_len`
    /// field's doc comment for why this must stay separate from
    /// `by_file[file].len()`.
    pub(crate) fn declared_len(&self, file: FileId) -> usize {
        self.declared_len.get(&file).copied().unwrap_or(0)
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
        self.indices.top_level.get(&CiQuery(name)).copied()
    }

    pub fn by_name_ci(&self, name: &str) -> &[SymbolId] {
        self.indices
            .by_name_ci
            .get(&CiQuery(name))
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

    pub(crate) fn set_unresolved_direct_super(&mut self, id: SymbolId, name: SmolStr) {
        Arc::make_mut(&mut self.indices)
            .unresolved_direct_super
            .insert(id, name);
    }

    /// `id`'s own raw `extends` name, when it failed to resolve against
    /// this project at all (see [`Indices::unresolved_direct_super`]'s own
    /// doc comment). `None` either when `id` has no `extends` clause, or
    /// when it does and it resolved fine (check [`Self::direct_super`]
    /// instead).
    pub fn unresolved_direct_super(&self, id: SymbolId) -> Option<&str> {
        self.indices.unresolved_direct_super.get(&id).map(SmolStr::as_str)
    }

    pub(crate) fn set_subtypes(&mut self, id: SymbolId, chain: Vec<SymbolId>) {
        Arc::make_mut(&mut self.indices).subtypes.insert(id, chain);
    }

    /// Every `SymbolId` transitively `extends`/`implements`-ing `id`,
    /// i.e. the reverse of [`Self::inherited_chain`] -- see `Indices::subtypes`'s
    /// own doc comment for why `crate::resolve` needs this.
    pub fn subtypes(&self, id: SymbolId) -> &[SymbolId] {
        self.indices.subtypes.get(&id).map_or(&[], |v| v.as_slice())
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

    /// Walks `container` up to the outermost enclosing type. Apex nested
    /// classes share their outer class's `private` access (compiling, in
    /// effect, as one nesting-aware unit) -- `is_visible_from`'s
    /// `Private` case needs the *top-level* declaring type, not just the
    /// immediately-enclosing one, to get that right.
    fn top_level_of(&self, id: SymbolId) -> SymbolId {
        let mut current = id;
        while let Some(parent) = self.get(current).container {
            current = parent;
        }
        current
    }

    /// Whether `candidate` (a member returned by `lookup_member`) is
    /// visible from `from` -- the reference site's enclosing type, `None`
    /// for a context with no enclosing type at all (a trigger's top-level
    /// body). `Public`/`Global` collapse to "always visible": v1 has no
    /// namespace model, the same simplification this binder already
    /// makes everywhere else a namespace boundary would otherwise matter.
    /// `@TestVisible` collapses the same way for a `Private`/`Protected`
    /// candidate, ahead of either's own same-type check: real Apex only
    /// actually grants that widened access to test-context callers, not
    /// every `from`, but this binder has no model of "is `from` itself
    /// test code" to check precisely, and under-widening is the costlier
    /// mistake here -- it's exactly what silently broke dead-code
    /// detection for a real `@TestVisible` member called only from a
    /// `_TEST` class in another file (never linked into
    /// `BoundProgram::references_to` at all, since resolution rejected
    /// the reference before it ever got that far). Collapsing to "always
    /// visible" is the same simplification `Public`/`Global` already make
    /// here, just narrower in scope (only `@TestVisible`-annotated
    /// members, not every `Private`/`Protected` one).
    /// `Private` (without `@TestVisible`) requires the same top-level type
    /// (`Self::top_level_of`); `Protected` additionally allows any type
    /// that *is*, `extends`, or `implements` the member's declaring type
    /// (`self.get(candidate).container`, already that type directly --
    /// every `lookup_member` candidate's `container` is its immediately-
    /// enclosing type, never a method/parameter, since only type-owned
    /// declarations are indexed by `members_by_name` at all).
    pub fn is_visible_from(&self, candidate: SymbolId, from: Option<SymbolId>) -> bool {
        let candidate_symbol = self.get(candidate);
        match candidate_symbol.modifiers.visibility {
            Visibility::Public | Visibility::Global => true,
            Visibility::Private => {
                if candidate_symbol.modifiers.is_test_visible {
                    return true;
                }
                let Some(from) = from else {
                    return false;
                };
                self.top_level_of(candidate) == self.top_level_of(from)
            }
            Visibility::Protected => {
                if candidate_symbol.modifiers.is_test_visible {
                    return true;
                }
                let Some(from) = from else {
                    return false;
                };
                if self.top_level_of(candidate) == self.top_level_of(from) {
                    return true;
                }
                let Some(declaring) = candidate_symbol.container else {
                    return false;
                };
                from == declaring || self.inherited_chain(from).contains(&declaring)
            }
        }
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
    ///
    /// An `override` member shadows -- doesn't sit alongside -- the
    /// ancestor member it overrides: once a more-derived level's
    /// candidate is marked `is_override`, any later (more-ancestral)
    /// same-name candidate at the *same arity* is excluded rather than
    /// reported as a separate overload. This is exact, not a heuristic --
    /// Apex requires an `override` method's signature (arity included) to
    /// exactly match what it overrides, a compile error otherwise, the
    /// same "arity alone is authoritative for user-defined methods"
    /// guarantee `crate::resolve::narrow_by_overload` already relies on.
    /// Only ever compares candidates *across* chain levels -- two
    /// same-arity overloads legitimately declared side by side at the
    /// *same* level (e.g. `foo(Integer)` and `foo(String)`, arity 1 each,
    /// on the same class) are never affected by each other, since a
    /// level's own overrides are only folded into the shadow set after
    /// that whole level has already been processed.
    pub fn lookup_member(&self, type_id: SymbolId, name: &str) -> Vec<SymbolId> {
        let mut found = Vec::new();
        let mut overridden_arities: FxHashSet<usize> = FxHashSet::default();
        for chain_id in
            std::iter::once(type_id).chain(self.inherited_chain(type_id).iter().copied())
        {
            let Some(members) = self
                .indices
                .members_by_name
                .get(&MemberQuery(chain_id, CiQuery(name)))
            else {
                continue;
            };
            let mut newly_overridden = Vec::new();
            // A field/property (or nested type) has no overload concept
            // at all -- unlike a method, where a same-arity ancestor
            // match is only excluded once an explicit `override` marks
            // it superseded, a *non-method* member declared at this
            // level unconditionally shadows anything of the same name
            // declared anywhere further up the chain, real Apex field-
            // hiding semantics (confirmed against a real deploy: two
            // *unrelated* classes both declaring a `static` property
            // named identically, one `extends`-ing the other, compiles
            // and resolves to the more-derived one, not an ambiguity --
            // real fflib shape, `fflib_SObjectDomain extends fflib_SObjects`,
            // both declaring their own unrelated `static ... Errors`
            // property). Without this, `lookup_member` kept walking every
            // further ancestor level regardless, so a same-named field/
            // property/nested-type match anywhere up the chain always
            // joined the self-declared one as a second, spurious
            // `Resolution::Candidates` entry -- permanently ambiguous,
            // even though a real compiler never sees any ambiguity here.
            let mut shadowed_by_non_method = false;
            for &id in members {
                let arity = self.params(id).len();
                if overridden_arities.contains(&arity) {
                    continue;
                }
                found.push(id);
                if self.get(id).modifiers.is_override {
                    newly_overridden.push(arity);
                }
                if !matches!(
                    self.get(id).kind,
                    crate::symbol::SymbolKind::Method | crate::symbol::SymbolKind::Constructor
                ) {
                    shadowed_by_non_method = true;
                }
            }
            overridden_arities.extend(newly_overridden);
            if shadowed_by_non_method {
                break;
            }
        }
        found
    }

    /// A *direct* (non-inherited) nested type declared on `container`
    /// named `name` -- the `Inner` of a qualified `Outer.Inner` type
    /// reference (`crate::resolve::resolve_type_ref`'s dotted-name case).
    /// Deliberately not [`Self::lookup_member`]: that also walks the
    /// inherited chain and applies override-arity elimination, both
    /// meant for methods -- a qualified type reference names exactly
    /// what `container` itself declares as a nested type, not something
    /// inherited or overload-shaped.
    pub fn nested_type(&self, container: SymbolId, name: &str) -> Option<SymbolId> {
        self.indices
            .members_by_name
            .get(&MemberQuery(container, CiQuery(name)))?
            .iter()
            .copied()
            .find(|&id| {
                matches!(
                    self.get(id).kind,
                    crate::symbol::SymbolKind::Class
                        | crate::symbol::SymbolKind::Interface
                        | crate::symbol::SymbolKind::Enum
                )
            })
    }

    /// Like [`Self::nested_type`], but also checks `container`'s
    /// inherited chain -- an *unqualified* reference to a nested type
    /// from within `container` itself, or a subclass, doesn't need to
    /// qualify it by whichever ancestor actually declared it, the same
    /// way an inherited field/method doesn't. The fallback
    /// `crate::resolve::resolve_type_ref` reaches for once a plain
    /// `SymbolTable::top_level` lookup fails on a single-segment name --
    /// real NPSP shape: `TDTM_Runnable`'s own abstract `run` method
    /// returns `List<DmlWrapper>`, not `List<TDTM_Runnable.DmlWrapper>`.
    pub fn nested_type_visible_from(&self, container: SymbolId, name: &str) -> Option<SymbolId> {
        std::iter::once(container)
            .chain(self.inherited_chain(container).iter().copied())
            .find_map(|id| self.nested_type(id, name))
    }

    /// Resolves a plain (already generics/array-suffix-stripped) dotted
    /// type-name *string* -- e.g. a `Symbol::type_name`, or an
    /// `extends`/`implements` supertype name as collected in Pass 1 --
    /// to the `SymbolId` it names: the first segment via [`Self::top_level`],
    /// then each further segment as a nested type declared directly on
    /// the previous one via [`Self::nested_type`]. The single-segment
    /// case (`Account`) is just the one `top_level` lookup, unchanged.
    ///
    /// The string counterpart to `crate::resolve::resolve_dotted_top_level`,
    /// which does the identical walk but over a `Type` node's own
    /// `base_name_tokens()` (and additionally records each segment's own
    /// goto-definition resolution as it goes) -- reach for *this* one
    /// instead whenever the only thing on hand is the plain name as a
    /// `&str`, with no `Type` AST node to re-derive tokens from and no
    /// per-segment reference recording to do: a field's/local's/
    /// parameter's own cached `type_name`, one of its own cached generic
    /// `type_args`, an overload-narrowing parameter-type comparison
    /// (`crate::conversions`), or one of Pass 1's collected
    /// `extends`/`implements` names. Every one of those call sites used
    /// to call `top_level` directly on the whole dotted string instead,
    /// which -- `top_level` being keyed by simple declared name only --
    /// silently failed for a self-referencing or sibling-nested qualified
    /// name (real NPSP shape: `UTIL_CurrencyCache.CurrencyData currData
    /// = ...;` inside `UTIL_CurrencyCache` itself, whose `currData.IsoCode
    /// = ...` assignments then never resolved, wrongly flagging `IsoCode`/
    /// `defaultRate` as dead despite being genuinely written to).
    pub fn resolve_dotted_name(&self, name: &str) -> Option<SymbolId> {
        let mut segments = name.split('.');
        let mut current = self.top_level(segments.next()?)?;
        for seg in segments {
            current = self.nested_type(current, seg)?;
        }
        Some(current)
    }

    /// Like [`Self::resolve_dotted_name`], but for a caller that also has
    /// an enclosing-type starting point (`from`) to retry an otherwise-
    /// unresolvable *single-segment* name against -- the same "unqualified
    /// nested-type reference visible from this lexical context" fallback
    /// [`Self::nested_type_visible_from`]'s own doc comment already
    /// describes for an ordinary type reference, extended here to
    /// `extends`/`implements` clause resolution specifically
    /// (`crate::inherit::resolve_inheritance`), which -- working from
    /// Pass 1's plain collected strings, not a live `Type` AST node --
    /// otherwise has no notion of "which type declared this name" context
    /// at all (every other `resolve_dotted_name` caller with such a
    /// context -- `crate::resolve::type_of_symbol`'s own declared-type
    /// case -- climbs its own enclosing chain the identical way, and in
    /// the identical order, rather than calling this). Real NPSP shape
    /// this fixes: `class ObjectError extends Error` where `Error` is a
    /// *sibling* nested class, both declared directly inside the same
    /// enclosing `fflib_SObjectDomain` -- `resolve_dotted_name("Error")`
    /// alone only ever checks `top_level`, which a nested class is never
    /// keyed under, so `Error`'s own inherited fields (`message`,
    /// `domain`) stayed permanently unreachable from `ObjectError`. Only
    /// tried for a genuinely single-segment name, matching every other
    /// version of this fallback's identical restriction: a dotted name
    /// names a real (if unresolvable) top-level type as its first
    /// segment, not an unqualified nested-type reference to retry here.
    pub fn resolve_dotted_name_from(&self, name: &str, from: Option<SymbolId>) -> Option<SymbolId> {
        // Checked before the plain `resolve_dotted_name` fallback below,
        // not after: real Apex resolves an unqualified name through the
        // lexically enclosing scope first, only falling back to an
        // unrelated top-level type of the same name -- confirmed
        // empirically against a real org (`sf apex run`), and the same
        // ordering fix `crate::resolve::type_of_symbol`/
        // `crate::resolve::resolve_type_ref_base` both needed for the
        // identical reason.
        if !name.contains('.') {
            let mut current = from;
            while let Some(container) = current {
                if let Some(id) = self.nested_type_visible_from(container, name) {
                    return Some(id);
                }
                current = self.get(container).container;
            }
        }
        self.resolve_dotted_name(name)
    }
}
