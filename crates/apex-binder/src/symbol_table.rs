//! The whole-project declaration table: an arena of every [`Symbol`]
//! collected in Pass 1 (`crate::collect`), plus the name/containment
//! indices Pass 1.5 (`crate::inherit`) and Pass 2 (`crate::resolve`)
//! look things up through.

use crate::symbol::{Symbol, SymbolId};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct SymbolTable {
    symbols: Vec<Symbol>,
    members_of: HashMap<SymbolId, Vec<SymbolId>>,
    /// Lowercase declared type name -> its `SymbolId`, project-wide (not
    /// per-file): unlike most languages this binder targets, Apex has no
    /// import/package system -- every top-level class/interface/enum in
    /// an org is visible from every other file unqualified, so a single
    /// flat namespace is the semantically correct model here, not a
    /// simplification. A real org would reject a genuine duplicate
    /// top-level name at deploy time; this binder resolves such a
    /// (necessarily already-invalid) conflict to whichever declaration
    /// was collected first rather than trying to detect and report it --
    /// out of scope for a binder whose job is resolution, not
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

impl SymbolTable {
    pub(crate) fn alloc(&mut self, symbol: Symbol) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        let lower = symbol.name.to_ascii_lowercase();

        self.by_name_ci.entry(lower.clone()).or_default().push(id);
        if symbol.container.is_none() && symbol.kind.is_type() {
            self.top_level.entry(lower).or_insert(id);
        }
        if let Some(container) = symbol.container {
            self.members_of.entry(container).or_default().push(id);
        }

        self.symbols.push(symbol);
        id
    }

    pub fn get(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.index()]
    }

    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &Symbol)> {
        self.symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (SymbolId(i as u32), s))
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Direct (non-inherited) members declared on `container`.
    pub fn members_of(&self, container: SymbolId) -> &[SymbolId] {
        self.members_of
            .get(&container)
            .map_or(&[], |v| v.as_slice())
    }

    pub fn top_level(&self, name: &str) -> Option<SymbolId> {
        self.top_level.get(&name.to_ascii_lowercase()).copied()
    }

    pub fn by_name_ci(&self, name: &str) -> &[SymbolId] {
        self.by_name_ci
            .get(&name.to_ascii_lowercase())
            .map_or(&[], |v| v.as_slice())
    }

    pub(crate) fn set_inherited_chain(&mut self, id: SymbolId, chain: Vec<SymbolId>) {
        self.inherited_chain.insert(id, chain);
    }

    pub fn inherited_chain(&self, id: SymbolId) -> &[SymbolId] {
        self.inherited_chain.get(&id).map_or(&[], |v| v.as_slice())
    }

    pub(crate) fn set_direct_super(&mut self, id: SymbolId, super_id: SymbolId) {
        self.direct_super.insert(id, super_id);
    }

    pub fn direct_super(&self, id: SymbolId) -> Option<SymbolId> {
        self.direct_super.get(&id).copied()
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
    /// type visited, self before ancestors.
    pub fn lookup_member(&self, type_id: SymbolId, name: &str) -> Vec<SymbolId> {
        let mut found = Vec::new();
        for chain_id in
            std::iter::once(type_id).chain(self.inherited_chain(type_id).iter().copied())
        {
            for &member in self.members_of(chain_id) {
                if self.get(member).name.eq_ignore_ascii_case(name) {
                    found.push(member);
                }
            }
        }
        found
    }
}
