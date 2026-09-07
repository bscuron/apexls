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

use crate::stdlib_index::StdlibIndex;
use crate::symbol::SymbolId;
use crate::symbol_table::SymbolTable;
use apex_stdlib::{StdlibClass, StdlibKind};
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
    stdlib: &StdlibIndex,
) {
    let resolved_super: Vec<(SymbolId, Result<SymbolId, SmolStr>)> = raw_super
        .par_iter()
        .map(|(type_id, name)| {
            (*type_id, resolve_supertype_name(table, *type_id, name).ok_or_else(|| name.clone()))
        })
        .collect();
    for (type_id, resolved) in resolved_super {
        match resolved {
            Ok(super_id) => table.set_direct_super(type_id, super_id),
            Err(name) => table.set_unresolved_direct_super(type_id, name),
        }
    }

    // Resolve each type's *direct* supertype names to `SymbolId`s first,
    // building a plain adjacency map. A name that doesn't resolve at all
    // as project-local is either a real stdlib interface (kept alongside,
    // in `direct_stdlib`, see that binding's own comment below) or a
    // genuine system/library name this binder has no data for at all
    // (e.g. `extends Exception`) -- either way it's simply dropped from
    // `direct` itself: an unresolvable supertype contributes nothing to
    // `inherited_chain`, it doesn't abort collection.
    let direct_and_stdlib: Vec<(SymbolId, Vec<SymbolId>, Vec<&'static StdlibClass>)> = raw_extends
        .par_iter()
        .map(|(type_id, names)| {
            let mut resolved = Vec::new();
            let mut stdlib_direct = Vec::new();
            for name in names {
                match resolve_supertype_name(table, *type_id, name) {
                    Some(id) => resolved.push(id),
                    None => stdlib_direct.extend(resolve_stdlib_interface(stdlib, name)),
                }
            }
            (*type_id, resolved, stdlib_direct)
        })
        .collect();

    let direct: FxHashMap<SymbolId, Vec<SymbolId>> = direct_and_stdlib
        .iter()
        .map(|(type_id, resolved, _)| (*type_id, resolved.clone()))
        .collect();
    // Every type's own *directly*-declared stdlib interfaces (before
    // transitively unioning in ancestors' own, below) -- a type with none
    // is simply absent, so `flatten`-style ancestor lookups below default
    // to empty rather than needing an `Option` everywhere.
    let direct_stdlib: FxHashMap<SymbolId, Vec<&'static StdlibClass>> = direct_and_stdlib
        .into_iter()
        .filter(|(_, _, stdlib_direct)| !stdlib_direct.is_empty())
        .map(|(type_id, _, stdlib_direct)| (type_id, stdlib_direct))
        .collect();

    let chains: Vec<(SymbolId, Vec<SymbolId>)> = raw_extends
        .par_iter()
        .map(|(type_id, _)| (*type_id, flatten(&direct, *type_id)))
        .collect();

    // `stdlib_implements`: `direct_stdlib`, unioned transitively across
    // the exact same flattened ancestor set `chains` just computed for
    // `inherited_chain` -- so a class implementing a project-local
    // interface that itself extends a stdlib interface still gets that
    // stdlib interface's exemption, the same transitivity
    // `inherited_chain` itself already has.
    let stdlib_implements: Vec<(SymbolId, Vec<&'static StdlibClass>)> = chains
        .par_iter()
        .map(|(type_id, chain)| {
            let mut classes: Vec<&'static StdlibClass> =
                direct_stdlib.get(type_id).cloned().unwrap_or_default();
            for ancestor in chain {
                if let Some(more) = direct_stdlib.get(ancestor) {
                    classes.extend(more.iter().copied());
                }
            }
            (*type_id, classes)
        })
        // Unconditional, even when `classes` is empty -- matching
        // `inherited_chain`'s own unconditional `set_inherited_chain` call
        // below. `SymbolTable::rebuild_indices` carries `stdlib_implements`
        // forward unchanged across a rebuild that skips this pass
        // entirely, so a type that *used to* implement a stdlib interface
        // but no longer does (e.g. its `implements` clause was edited to
        // drop it) must still get a fresh, empty entry written here to
        // overwrite the stale one -- filtering it out would leave the old
        // value in place forever.
        .collect();

    for (type_id, chain) in chains {
        table.set_inherited_chain(type_id, chain);
    }
    for (type_id, classes) in stdlib_implements {
        table.set_stdlib_implements(type_id, classes);
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

/// Resolves one `extends`/`implements` name declared *on* `type_id`
/// itself. Checks `type_id`'s own directly-declared nested types first
/// (a real, confirmed gap this closes: `resolve_dotted_name_from`'s own
/// upward walk starts at `type_id`'s *container*, so a top-level type
/// implementing an interface declared as its own nested member --
/// `class Foo implements Inner { interface Inner {...} }`, real NPSP
/// shape in `fflib_Inheritor`/`fflib_Criteria`/`fflib_MyList` -- was
/// previously indistinguishable from a genuinely unresolvable name,
/// since a top-level type's own `container` is `None`, so the walk
/// never even starts). Deliberately checked via the direct,
/// `inherited_chain`-independent [`SymbolTable::nested_type`] rather
/// than [`SymbolTable::nested_type_visible_from`] (which additionally
/// consults `inherited_chain`): `inherited_chain` is exactly what this
/// function's caller, [`resolve_inheritance`], is in the middle of
/// computing, so it's empty for every type at this point in the
/// pipeline -- reaching for it here would silently no-op, not search
/// ancestors. Falls back to the existing `resolve_dotted_name_from`
/// walk (the type's *container* chain, then a project-wide top-level
/// lookup) exactly as before when `type_id` has no matching nested type
/// of its own, or `name` is dotted (a nested type is only ever named
/// unqualified from within its own declaring type).
fn resolve_supertype_name(table: &SymbolTable, type_id: SymbolId, name: &str) -> Option<SymbolId> {
    if !name.contains('.') {
        if let Some(id) = table.nested_type(type_id, name) {
            return Some(id);
        }
    }
    let from = table.get(type_id).container;
    table.resolve_dotted_name_from(name, from)
}

/// Checks a supertype name that failed to resolve as project-local
/// against `apex-stdlib`'s bundled data, keeping the match only when it's
/// specifically a stdlib *interface* (`Class`/`Enum` matches are a
/// different, already-handled shape -- see `SymbolTable::unresolved_direct_super`'s
/// own doc comment for the `extends`-a-stdlib-*class* fallback this isn't).
/// A namespace-qualified name (`Database.Batchable`) splits on its first
/// `.` into namespace + tail; a bare name (`Comparable`, `StubProvider`)
/// goes straight to [`StdlibIndex::class`], which already picks the right
/// entry for the handful of real namespace collisions. Only tried for
/// exactly two segments -- a further-dotted tail (three or more segments)
/// is out of scope, the same restriction `resolve.rs`'s own
/// `class_in_namespace` call sites already apply (`resolve.rs:1627`,
/// `:1653`), since every real stdlib interface name is `Namespace.Type`.
fn resolve_stdlib_interface(stdlib: &StdlibIndex, name: &str) -> Option<&'static StdlibClass> {
    let class = match name.split_once('.') {
        Some((namespace, tail)) if !tail.contains('.') => stdlib.class_in_namespace(namespace, tail),
        Some(_) => None,
        None => stdlib.class(name),
    }?;
    (class.kind == StdlibKind::Interface).then_some(class)
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
