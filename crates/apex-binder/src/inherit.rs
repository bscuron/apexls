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

use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use std::collections::{HashMap, HashSet};

/// `raw_extends`: for each type symbol that declared at least one
/// `extends`/`implements` clause, its id plus the unresolved supertype
/// names as written (a dotted base name, e.g. `Outer.Inner` -- generic
/// arguments/array suffixes are already stripped by `Type::text()`).
pub(crate) fn resolve_inheritance(
    table: &mut SymbolTable,
    raw_extends: &[(SymbolId, Vec<String>)],
    raw_super: &[(SymbolId, String)],
) {
    for (type_id, super_name) in raw_super {
        if let Some(super_id) = table.top_level(super_name) {
            table.set_direct_super(*type_id, super_id);
        }
    }

    // Resolve each type's *direct* supertype names to `SymbolId`s first,
    // building a plain adjacency map. A name that doesn't resolve to any
    // project-local top-level type (a system/library base class like
    // `Exception`, or a qualified nested-type reference -- `top_level`
    // is keyed by simple declared name only, v1 doesn't resolve those)
    // is simply dropped: an unresolvable supertype contributes nothing
    // to the chain, it doesn't abort collection.
    let direct: HashMap<SymbolId, Vec<SymbolId>> = raw_extends
        .iter()
        .map(|(type_id, names)| {
            let resolved = names.iter().filter_map(|n| table.top_level(n)).collect();
            (*type_id, resolved)
        })
        .collect();

    for (type_id, _) in raw_extends {
        table.set_inherited_chain(*type_id, flatten(&direct, *type_id));
    }
}

/// Every `SymbolId` transitively reachable from `type_id` via `direct`,
/// visited-set guarded so a malformed `class A extends B` / `class B
/// extends A` (or any longer cycle, or a diamond shape) can't
/// infinite-loop or duplicate an ancestor. Does not include `type_id`
/// itself -- callers wanting "this type or an ancestor" (like
/// `SymbolTable::lookup_member`) prepend it themselves.
fn flatten(direct: &HashMap<SymbolId, Vec<SymbolId>>, type_id: SymbolId) -> Vec<SymbolId> {
    let mut visited = HashSet::new();
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
