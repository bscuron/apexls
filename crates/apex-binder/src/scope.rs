//! Statement-level lexical scopes, one [`ScopeTree`] per method/
//! constructor/property-accessor/initializer body -- not project-wide,
//! which keeps each independently disposable/rebuildable per-file and
//! keeps lookup ranges small. Built alongside Pass 2's statement/
//! expression walk (`crate::resolve`).

use crate::symbol::SymbolId;
use rowan::TextRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// The method/constructor/accessor body's own root scope --
    /// parameters live here.
    Body,
    Block,
    /// A `for`/`for-each` loop's own scope, wrapping its body -- keeps
    /// the loop variable(s) from leaking into the enclosing scope.
    For,
    /// One `catch (Type e) { ... }` clause's scope, wrapping just its
    /// own body.
    Catch,
    /// One `when Type binding { ... }` switch clause's scope.
    Switch,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    /// `(declared name, symbol)` in declaration order. A small `Vec`,
    /// not a `HashMap`: block-local variable counts are tiny (single
    /// digits to low tens), so a linear scan beats hashing, and it
    /// naturally supports "most recent wins" if ever queried mid-walk.
    bindings: Vec<(String, SymbolId)>,
}

impl Scope {
    fn new(parent: Option<ScopeId>, kind: ScopeKind) -> Self {
        Scope {
            parent,
            kind,
            bindings: Vec::new(),
        }
    }

    pub fn bindings(&self) -> &[(String, SymbolId)] {
        &self.bindings
    }

    /// The most-recently-added binding named `name` (case-insensitive),
    /// if any -- doesn't look at parent scopes; see
    /// [`ScopeTree::resolve_local`] for the full chain walk.
    fn lookup_local(&self, name: &str) -> Option<SymbolId> {
        self.bindings
            .iter()
            .rev()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, id)| *id)
    }
}

/// One method/constructor/accessor/initializer body's scope chain, plus
/// a position index (each scope's own defining range) for "what's the
/// innermost scope at this position" queries -- useful beyond Pass 2's
/// own resolution (which always knows its current scope directly while
/// walking) for later position-driven LSP requests like completion.
#[derive(Debug, Default)]
pub struct ScopeTree {
    scopes: Vec<Scope>,
    /// Each scope's own defining range (the `Block`/loop-body/catch-
    /// body/switch-clause-body node it was opened for) alongside its id.
    scope_ranges: Vec<(TextRange, ScopeId)>,
}

impl ScopeTree {
    pub(crate) fn new_root(kind: ScopeKind, range: TextRange) -> (Self, ScopeId) {
        let mut tree = ScopeTree::default();
        let id = tree.push(None, kind, range);
        (tree, id)
    }

    pub(crate) fn push(
        &mut self,
        parent: Option<ScopeId>,
        kind: ScopeKind,
        range: TextRange,
    ) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope::new(parent, kind));
        self.scope_ranges.push((range, id));
        id
    }

    pub(crate) fn bind(&mut self, scope: ScopeId, name: String, symbol: SymbolId) {
        self.scopes[scope.0 as usize].bindings.push((name, symbol));
    }

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    /// Walks `scope` and its ancestors, innermost first, looking for a
    /// binding named `name` -- the lexical-scope half of unqualified
    /// name resolution; member/inherited-chain lookup, the other half,
    /// happens in `crate::resolve` once this returns `None`.
    pub fn resolve_local(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = self.scope(id);
            if let Some(found) = s.lookup_local(name) {
                return Some(found);
            }
            current = s.parent;
        }
        None
    }

    /// The innermost scope whose own defining range contains `range` --
    /// the smallest containing range is always unambiguous since scopes
    /// nest properly (each child scope's range is fully inside its
    /// parent's).
    pub fn scope_at(&self, range: TextRange) -> Option<ScopeId> {
        self.scope_ranges
            .iter()
            .filter(|(r, _)| r.contains_range(range))
            .min_by_key(|(r, _)| r.len())
            .map(|(_, id)| *id)
    }
}
