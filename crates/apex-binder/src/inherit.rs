//! Pass 1.5: inheritance chain resolution. Runs once, after every
//! file's declarations have been merged into one project-wide
//! `SymbolTable` (so a class's `extends` target -- which might live in
//! another file, or be declared later in the same file -- is safe to
//! look up). Resolves each type symbol's raw `extends`/`implements`
//! supertype names (collected in Pass 1 as plain strings, since
//! resolving them required the whole project to exist first) into a
//! flattened, cycle-guarded `inherited_chain`, which
//! `SymbolTable::lookup_member` walks for member-lookup fallthrough --
//! plus, separately, each class's direct `extends` target alone
//! (`direct_super`), for `super`/`super(...)` resolution.
//!
//! Every per-type computation here (resolving one type's direct
//! `extends`/`implements` names, flattening one type's chain) only ever
//! reads `table`, never writes it, so each is safe to run in parallel
//! (`rayon`) against a shared `&SymbolTable`; only the final
//! `set_inherited_chain`/`set_direct_super` calls need `&mut
//! SymbolTable`, so those stay a fast sequential pass over the
//! already-computed results.

use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

/// `raw_extends`: for each type symbol that declared at least one
/// `extends`/`implements` clause, its id plus the unresolved supertype
/// names as written (a dotted base name, e.g. `Outer.Inner` -- generic
/// arguments/array suffixes are already stripped by `Type::text()`).
#[hotpath::measure]
pub(crate) fn resolve_inheritance(
    table: &mut SymbolTable,
    raw_extends: &[(SymbolId, Vec<SmolStr>)],
    raw_super: &[(SymbolId, SmolStr)],
) {
    let direct_super: Vec<(SymbolId, SymbolId)> = raw_super
        .par_iter()
        .filter_map(|(type_id, name)| table.resolve_dotted_name(name).map(|super_id| (*type_id, super_id)))
        .collect();
    for (type_id, super_id) in direct_super {
        table.set_direct_super(type_id, super_id);
    }

    // Resolve each type's *direct* supertype names to `SymbolId`s first,
    // building a plain adjacency map. A name that doesn't resolve at all
    // (a system/library base class like `Exception`) is simply dropped:
    // an unresolvable supertype contributes nothing to the chain, it
    // doesn't abort collection.
    let direct: FxHashMap<SymbolId, Vec<SymbolId>> = raw_extends
        .par_iter()
        .map(|(type_id, names)| {
            let resolved = names.iter().filter_map(|n| table.resolve_dotted_name(n)).collect();
            (*type_id, resolved)
        })
        .collect();

    let chains: Vec<(SymbolId, Vec<SymbolId>)> = raw_extends
        .par_iter()
        .map(|(type_id, _)| (*type_id, flatten(&direct, *type_id)))
        .collect();
    for (type_id, chain) in chains {
        table.set_inherited_chain(type_id, chain);
    }

    // `subtypes` is `direct`'s reverse graph: every type that has at
    // least one direct subtype becomes a key, mapping to every type that
    // names *it* as a direct supertype -- then flattened transitively
    // the same way `inherited_chain` flattens the forward graph. Backs
    // dynamic-dispatch widening (`crate::resolve::expand_dynamic_dispatch`) --
    // see `SymbolTable`'s own `subtypes` field doc comment.
    let mut direct_subtypes: FxHashMap<SymbolId, Vec<SymbolId>> = FxHashMap::default();
    for (&type_id, supers) in &direct {
        for &super_id in supers {
            direct_subtypes.entry(super_id).or_default().push(type_id);
        }
    }
    let supertypes_with_subtypes: Vec<SymbolId> = direct_subtypes.keys().copied().collect();
    let subtypes: Vec<(SymbolId, Vec<SymbolId>)> = supertypes_with_subtypes
        .par_iter()
        .map(|&super_id| (super_id, flatten(&direct_subtypes, super_id)))
        .collect();
    for (type_id, chain) in subtypes {
        table.set_subtypes(type_id, chain);
    }
}

/// Every `SymbolId` transitively reachable from `type_id` via `direct`,
/// visited-set guarded so a malformed `class A extends B` / `class B
/// extends A` (or any longer cycle, or a diamond shape) can't
/// infinite-loop or duplicate an ancestor. Does not include `type_id`
/// itself -- callers wanting "this type or an ancestor" (like
/// `SymbolTable::lookup_member`) prepend it themselves.
fn flatten(direct: &FxHashMap<SymbolId, Vec<SymbolId>>, type_id: SymbolId) -> Vec<SymbolId> {
    let mut visited = FxHashSet::default();
    visited.insert(type_id);
    let mut chain = Vec::new();
    let mut stack: Vec<SymbolId> = direct.get(&type_id).cloned().unwrap_or_default();
    while let Some(next) = stack.pop() {
        if !visited.insert(next) {
            continue;
        }
        chain.push(next);
        if let Some(more) = direct.get(&next) {
            stack.extend(more.iter().copied());
        }
    }
    chain
}
