//! Allocation-free case-insensitive `hashbrown::HashMap` lookup: Apex
//! identifiers and Salesforce API names are case-insensitive for lookup
//! purposes, but the naive way to support that -- lowercasing the query
//! string before every `.get()` call -- allocates a new `String` on every
//! single reference resolution in the whole project, not just once per
//! declaration. `lookup_member` (`crate::symbol_table`) and
//! `SchemaIndex::object`/`field` (`crate::schema_index`) are exactly that
//! kind of call, run once per name *reference*, so this matters far more
//! than the (already-fixed) allocation on the *declaration* side.
//!
//! [`CiKey`] (the stored, owned map key) and [`CiQuery`] (the borrowed
//! lookup query, built from a reference site's original-case text with no
//! allocation at all) both hash through the same [`hash_ascii_lower`]
//! function, so the two are *guaranteed* to agree on a value's hash by
//! construction -- not by coincidentally matching two independently
//! written `Hash` impls, which is exactly the inconsistency risk that
//! made an earlier `unicase`-based approach unsound (verified against
//! `unicase`'s actual docs: it has no `Borrow`/`Equivalent`-style relation
//! between an owned key and a borrowed query, so it couldn't have
//! delivered an allocation-free lookup here at all). `CiKey`'s own
//! `PartialEq` is *also* case-insensitive (not derived from the wrapped
//! `SmolStr`'s raw bytes) so it agrees with its own `Hash` -- this is
//! also what makes two declarations spelled with different case (`Foo`,
//! `foo`) still coalesce into the same map entry the way today's
//! pre-lowered `String` keys do, without the caller needing to lowercase
//! before constructing a `CiKey` at all.

use rustc_hash::FxBuildHasher;
use smol_str::SmolStr;
use std::hash::{Hash, Hasher};

/// A case-insensitively-keyed map, shared by every index that needs one
/// ([`crate::symbol_table::SymbolTable`]'s `top_level`/`by_name_ci`,
/// [`crate::schema_index::SchemaIndex`]'s `objects`/`fields`).
pub(crate) type CiMap<V> = hashbrown::HashMap<CiKey, V, FxBuildHasher>;

/// Feeds `bytes` to `state` one ASCII-lowercased byte at a time, plus a
/// trailing terminator byte (`0xff`, not a valid ASCII/UTF-8 continuation
/// byte) so that e.g. `"ab"` + `"c"` and `"a"` + `"bc"` -- which could
/// otherwise hash identically if two adjacent fields were ever hashed
/// through this same function back to back -- can't collide. The only
/// thing that matters about this function is that [`CiKey`] and
/// [`CiQuery`] both call it identically; the specific algorithm is
/// otherwise arbitrary.
fn hash_ascii_lower<H: Hasher>(bytes: &[u8], state: &mut H) {
    for &b in bytes {
        state.write_u8(b.to_ascii_lowercase());
    }
    state.write_u8(0xff);
}

/// A case-insensitive map's stored key. Keeps the name's declared
/// spelling (never displayed from these maps -- only ever used as a
/// lookup identity, so there's no reason to force a case-folding copy at
/// insert time either).
#[derive(Debug, Clone)]
pub(crate) struct CiKey(pub(crate) SmolStr);

impl Hash for CiKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_ascii_lower(self.0.as_bytes(), state);
    }
}

impl PartialEq for CiKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}
impl Eq for CiKey {}

impl From<&str> for CiKey {
    fn from(s: &str) -> Self {
        CiKey(SmolStr::new(s))
    }
}

/// A case-insensitive lookup query: the reference site's original-case
/// text, borrowed -- never allocated, unlike building a lowercased
/// `String`/`SmolStr` on every lookup would be.
pub(crate) struct CiQuery<'a>(pub(crate) &'a str);

impl Hash for CiQuery<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_ascii_lower(self.0.as_bytes(), state);
    }
}

impl hashbrown::Equivalent<CiKey> for CiQuery<'_> {
    fn equivalent(&self, key: &CiKey) -> bool {
        self.0.eq_ignore_ascii_case(&key.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxBuildHasher;

    fn map() -> hashbrown::HashMap<CiKey, i32, FxBuildHasher> {
        let mut m = hashbrown::HashMap::default();
        m.insert(CiKey::from("account"), 1);
        m.insert(CiKey::from("x"), 2);
        m
    }

    #[test]
    fn differing_case_queries_find_the_same_entry() {
        let m = map();
        assert_eq!(m.get(&CiQuery("account")), Some(&1));
        assert_eq!(m.get(&CiQuery("Account")), Some(&1));
        assert_eq!(m.get(&CiQuery("ACCOUNT")), Some(&1));
        assert_eq!(m.get(&CiQuery("AcCoUnT")), Some(&1));
    }

    #[test]
    fn single_character_names_are_found() {
        let m = map();
        assert_eq!(m.get(&CiQuery("x")), Some(&2));
        assert_eq!(m.get(&CiQuery("X")), Some(&2));
    }

    #[test]
    fn a_missing_key_returns_none_without_panicking() {
        let m = map();
        assert_eq!(m.get(&CiQuery("")), None);
        assert_eq!(m.get(&CiQuery("nonexistent")), None);
    }

    #[test]
    fn an_empty_map_lookup_returns_none() {
        let m: hashbrown::HashMap<CiKey, i32, FxBuildHasher> = hashbrown::HashMap::default();
        assert_eq!(m.get(&CiQuery("anything")), None);
    }

    #[test]
    fn two_declarations_differing_only_by_case_coalesce_into_one_entry() {
        // Matches today's pre-lowered-`String`-key behavior: `CiKey`'s
        // own `Eq` is case-insensitive, so inserting under two spellings
        // Apex treats as the same identifier lands in the same bucket --
        // the second `insert` overwrites the first's value rather than
        // creating a second entry.
        let mut m: hashbrown::HashMap<CiKey, i32, FxBuildHasher> = hashbrown::HashMap::default();
        m.insert(CiKey::from("Foo"), 1);
        m.insert(CiKey::from("foo"), 2);
        assert_eq!(m.get(&CiQuery("FOO")), Some(&2));
        assert_eq!(m.len(), 1);
    }
}
