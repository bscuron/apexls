//! Pass 2: scope building + reference resolution. Walks one method/
//! constructor/property-accessor body (or a bare field/property
//! initializer expression) building a [`ScopeTree`] alongside a
//! [`Stmt`]/[`Expr`] walk, resolving references as they're encountered.
//!
//! Runs only after Pass 1.5 (`crate::inherit`) has finished, so member/
//! inherited-chain lookups against the whole project are always safe.
//! Like Pass 1, each body is bound independently against a read-only
//! `&SymbolTable` and is safe to run in parallel (`crate::BoundProgram::from_files`
//! does, via `rayon`) -- a body's newly-declared locals (`LocalVar`/
//! `CatchVar`/`ForEachVar`/`SwitchBindingVar`) are allocated into a
//! body-local arena (`BodyBinder::pending_locals`) tagged with a
//! sentinel `SymbolId` (see [`LOCAL_SENTINEL_BASE`]) rather than the
//! shared table directly, and merged in afterward by a fast sequential
//! pass -- the same local-collect-then-merge trick Pass 1 uses for
//! symbols, applied here to keep concurrent bodies from needing to
//! coordinate a single shared mutable arena.
//!
//! Scope of what gets resolved, and how -- see the project's design
//! plan for the full rationale, summarized here:
//! - Unqualified names (locals/params/fields/enclosing-type members)
//!   resolve via lexical scope, then member lookup through the
//!   enclosing type's `extends`/`implements` chain -- both steps need
//!   no type inference, since Apex guarantees an unqualified name
//!   resolves unambiguously (or not at all) by simple name.
//! - `this`/`super` are a cheap one-hop special case: `this` is always
//!   the enclosing type, `super` is always its direct `extends` target
//!   (`SymbolTable::direct_super`).
//! - A qualified reference (`target.member`) only resolves past
//!   `Unresolved` when `target`'s type is *already* known without any
//!   inference -- a `NameExpr`/`this`/`super`/`new` expression whose
//!   declared type is itself a project-local class. Anything requiring
//!   real type inference (the result of an arbitrary method call, a
//!   binary expression, ...) stays `Unresolved` rather than guessing.
//! - Method/constructor calls (qualified or not, including `new`/
//!   `this(...)`/`super(...)`) go through [`narrow_by_overload`]: an
//!   exact arity filter (real Apex semantics -- there's no varargs or
//!   default parameter values for user-defined methods, so arity alone
//!   is authoritative, not a heuristic), then elimination by argument
//!   type (`crate::conversions::type_compatible` -- project-local
//!   subtyping *and* Apex's implicit-conversion rules for a curated set
//!   of system types: numeric widening, `Object`, `List`/`Set`/`Map`),
//!   then a most-specific tiebreak (`crate::conversions::is_more_specific`)
//!   among whatever still survives. A call resolves to a single
//!   `Resolved` symbol when exactly one candidate survives elimination,
//!   or when the tiebreak finds a unique most-specific one; otherwise
//!   it's `Candidates` (genuinely ambiguous, or argument/parameter types
//!   outside `crate::conversions`'s curated set) or `Unresolved` (no
//!   same-name member at all).

use crate::conversions;
use crate::file_id::FileId;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::reference_table::{ReferenceTable, Resolution, SchemaObjectRef, UnknownSchemaRef};
use crate::schema_index::{relationship_field_api_name, SchemaIndex};
use crate::scope::{ScopeId, ScopeKind, ScopeTree};
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind};
use crate::symbol_table::SymbolTable;
use crate::ty::Ty;
use apex_syntax::ast::decl::TriggerBlock;
use apex_syntax::ast::expr::{CallExpr, FieldExpr, Initializer, MethodCallExpr, NameExpr, NewExpr};
use apex_syntax::ast::stmt::Block;
use apex_syntax::ast::{Expr, Name, Stmt, Type};
use apex_syntax::SyntaxKind;
use rowan::ast::AstNode;
use smol_str::SmolStr;

/// Narrows a same-name candidate set (methods, or constructors for a
/// `new`/`this(...)`/`super(...)` call) using real Apex overload-
/// resolution rules where they're checkable without a full type system.
///
/// First, an exact arity filter: Apex has no varargs or default
/// parameter values for user-defined methods/constructors, so a
/// candidate whose parameter count doesn't equal `arg_types.len()`
/// genuinely cannot be the one called -- this is exact, not a
/// heuristic. If nothing survives arity filtering at all (a real
/// compile error, or a gap in this binder's own counting), the original
/// full candidate set is reported rather than claiming nothing matched.
///
/// Second, among same-arity survivors, elimination by argument type via
/// `crate::conversions::type_compatible`: project-local exact-match-or-
/// upcast (as before), plus Apex's implicit-conversion rules for a
/// curated set of system types (numeric widening, `Object`, `List`/
/// `Set`/`Map`) -- see that module's doc comment for exactly what's
/// covered and how it was verified. A candidate is ruled out only when
/// some argument's inferred type is *positively* incompatible with the
/// corresponding parameter's declared type; anything outside the
/// curated set (an argument whose type isn't known at all, an unmodeled
/// system type, ...) can never rule a candidate out: "can't prove
/// wrong" always wins over "assume wrong."
///
/// Third, if more than one candidate survives elimination, a
/// most-specific tiebreak (`crate::conversions::is_more_specific`) --
/// real Apex overload resolution doesn't stop at "eliminate the
/// impossible ones," it picks the most specific applicable candidate
/// (e.g. `foo(Integer)` over `foo(Object)` for an `Integer` argument),
/// the same way Java does. Only resolves to `Resolved` when exactly one
/// candidate is at least as specific as every other survivor in every
/// parameter position, with a strict improvement in at least one
/// position; otherwise the set stays genuinely `Candidates` (real
/// ambiguity, rare in code that actually compiles).
fn narrow_by_overload(
    table: &SymbolTable,
    candidates: Vec<SymbolId>,
    arg_types: &[Option<Ty>],
) -> Resolution {
    if candidates.is_empty() {
        return Resolution::Unresolved;
    }

    let by_arity: Vec<SymbolId> = candidates
        .iter()
        .copied()
        .filter(|&id| table.params(id).len() == arg_types.len())
        .collect();
    let pool = if by_arity.is_empty() {
        candidates
    } else {
        by_arity
    };
    if pool.len() == 1 {
        return Resolution::Resolved(pool[0]);
    }

    let by_type: Vec<SymbolId> = pool
        .iter()
        .copied()
        .filter(|&id| is_argument_type_compatible(table, id, arg_types))
        .collect();
    match by_type.len() {
        1 => Resolution::Resolved(by_type[0]),
        // Over-eliminated (every candidate ruled itself out, which given
        // the conservative rule above should only happen if `pool`
        // itself was already empty -- defensive, not expected) or still
        // ambiguous: report the honest pre-type-filter pool either way.
        0 => Resolution::Candidates(pool),
        _ => match most_specific_candidate(table, &by_type) {
            Some(winner) => Resolution::Resolved(winner),
            None => Resolution::Candidates(by_type),
        },
    }
}

/// True for a `Method` symbol Apex could dispatch dynamically at
/// runtime to a *different*, more-derived declaration than the one a
/// static, declared-type-only lookup finds: any method declared directly
/// on an `Interface` (implicitly abstract -- Apex forbids a method body
/// there), or any `virtual`/`abstract`/`override` method on a class. A
/// plain concrete method with none of those modifiers can never be
/// further overridden in Apex, so it's the *only* case a resolved
/// candidate is provably final -- excluding it here is what keeps
/// [`expand_dynamic_dispatch`] a no-op (skips the `subtypes` walk
/// entirely) for the overwhelming majority of real method calls.
fn is_dynamically_dispatchable(table: &SymbolTable, id: SymbolId) -> bool {
    let symbol = table.get(id);
    if symbol.kind != SymbolKind::Method || symbol.modifiers.is_static {
        return false;
    }
    let m = &symbol.modifiers;
    if m.is_virtual || m.is_override || m.is_abstract {
        return true;
    }
    symbol
        .container
        .is_some_and(|c| table.get(c).kind == SymbolKind::Interface)
}

/// Widens one resolved method candidate to every dynamically-reachable
/// override/implementation, for the scenario [`is_dynamically_dispatchable`]
/// identifies: Apex (like Java) dispatches an instance method call
/// against the receiver's *actual runtime type*, not the reference's
/// declared static type, so a `virtual`/`abstract`/interface method's own
/// declaration is never provably the only real target -- any subtype in
/// [`SymbolTable::subtypes`] that declares a same-name (case-insensitive),
/// same-arity method is an equally valid one, and must be credited with
/// this call site the same way the statically-resolved declaration is
/// (this is exactly what makes `crate::dead_code`'s reference counting
/// correct for a method reached only through interface-typed or
/// base-typed dispatch, never called by its own concrete type directly).
/// Returns just `base` (as a single-element `Vec`) unchanged when `base`
/// isn't dispatchable at all, or has no known overriding/implementing
/// subtype -- both the overwhelmingly common case, so this never
/// allocates more than that one element for a call that turns out not to
/// need widening.
fn expand_dynamic_dispatch(table: &SymbolTable, base: SymbolId) -> Vec<SymbolId> {
    if !is_dynamically_dispatchable(table, base) {
        return vec![base];
    }
    let symbol = table.get(base);
    let Some(container) = symbol.container else {
        return vec![base];
    };
    let arity = table.params(base).len();
    let mut targets = vec![base];
    for &sub in table.subtypes(container) {
        for &member_id in table.members_of(sub) {
            let member = table.get(member_id);
            if member.kind == SymbolKind::Method
                && member.name.eq_ignore_ascii_case(symbol.name.as_str())
                && table.params(member_id).len() == arity
            {
                targets.push(member_id);
            }
        }
    }
    targets
}

/// Applies [`expand_dynamic_dispatch`] to every `SymbolId` a method-call
/// `Resolution` already names, widening a `Resolved` into `Candidates`
/// (or widening an already-`Candidates` set further) whenever dynamic
/// dispatch could reach more than one declaration. Never applied to a
/// constructor resolution (`new`/`this(...)`/`super(...)`): a
/// constructor call always instantiates the exact named type, and Apex
/// has no virtual constructors to dispatch across, so widening one would
/// only manufacture false candidates.
fn widen_for_dynamic_dispatch(table: &SymbolTable, resolution: Resolution) -> Resolution {
    let ids: &[SymbolId] = match &resolution {
        Resolution::Resolved(id) => std::slice::from_ref(id),
        Resolution::Candidates(ids) => ids.as_slice(),
        _ => return resolution,
    };
    let mut expanded: Vec<SymbolId> = Vec::new();
    for &id in ids {
        for target in expand_dynamic_dispatch(table, id) {
            if !expanded.contains(&target) {
                expanded.push(target);
            }
        }
    }
    match expanded.as_slice() {
        [] => resolution,
        [one] => Resolution::Resolved(*one),
        _ => Resolution::Candidates(expanded),
    }
}

fn is_argument_type_compatible(
    table: &SymbolTable,
    candidate: SymbolId,
    arg_types: &[Option<Ty>],
) -> bool {
    for (param, arg_type) in table.params(candidate).iter().zip(arg_types.iter()) {
        let Some(arg_type) = arg_type else {
            continue; // an argument whose type isn't known at all never eliminates
        };
        let param_symbol = table.get(*param);
        let Some(param_type_name) = param_symbol.type_name.as_deref() else {
            continue;
        };
        if conversions::type_compatible(table, param_type_name, &param_symbol.type_args, arg_type)
            == Some(false)
        {
            return false;
        }
    }
    true
}

/// The unique candidate that's at least as specific as every other
/// `candidates` entry in every parameter position, with a strict
/// improvement in at least one position -- `None` if no such candidate
/// exists (a real ambiguity between two candidates neither more specific
/// than the other, or -- shouldn't arise once arity is already equal
/// across `candidates` -- a parameter-count mismatch).
fn most_specific_candidate(table: &SymbolTable, candidates: &[SymbolId]) -> Option<SymbolId> {
    candidates
        .iter()
        .copied()
        .find(|&c| {
            candidates
                .iter()
                .all(|&d| d == c || dominates(table, c, d))
        })
}

/// Whether `a`'s declared parameter list is at least as specific as `b`'s
/// in every position, with a strict improvement in at least one --
/// compares only the two candidates' own declared signatures, independent
/// of the actual call's arguments (both are already known-applicable).
fn dominates(table: &SymbolTable, a: SymbolId, b: SymbolId) -> bool {
    let a_params = table.params(a);
    let b_params = table.params(b);
    if a_params.len() != b_params.len() {
        return false;
    }
    let mut any_strict = false;
    for (&ap, &bp) in a_params.iter().zip(b_params.iter()) {
        let a_sym = table.get(ap);
        let b_sym = table.get(bp);
        let (Some(a_name), Some(b_name)) = (a_sym.type_name.as_deref(), b_sym.type_name.as_deref())
        else {
            return false;
        };
        let same_position = a_name.eq_ignore_ascii_case(b_name)
            && a_sym.type_args.len() == b_sym.type_args.len()
            && a_sym
                .type_args
                .iter()
                .zip(b_sym.type_args.iter())
                .all(|(x, y)| x.eq_ignore_ascii_case(y));
        if same_position {
            continue;
        }
        if conversions::is_more_specific(table, a_name, &a_sym.type_args, b_name, &b_sym.type_args) {
            any_strict = true;
        } else {
            return false;
        }
    }
    any_strict
}

/// The reserved `SymbolId` range `declare_local` allocates from during
/// one body's binding pass, disjoint from any real project `SymbolId`
/// (no real Apex org has ~2 billion top-level declarations). A
/// placeholder, remapped to a real global id once every body's local
/// batch has been merged into the shared `SymbolTable`
/// (`crate::BoundProgram::from_files`). Using a disjoint numbering range
/// -- rather than an `enum` wrapper around every `SymbolId`-typed field
/// -- keeps `Resolution`/`Scope` unchanged: the remap step is a single
/// `SymbolId -> SymbolId` function, a no-op below this threshold,
/// applied uniformly without needing to track which ids came from where.
pub(crate) const LOCAL_SENTINEL_BASE: u32 = u32::MAX / 2;

/// `base` is the *file's own* declared-symbol count (`FileCollection::symbols.len()`
/// for the file this sentinel id's `id.file` names) -- not a project-wide
/// count. Since a body's sentinel ids already carry the correct `file`
/// (set once at `declare_local` time), remapping only ever needs to
/// translate `local`, never `file`.
pub(crate) fn remap_local_id(id: SymbolId, base: u32) -> SymbolId {
    if id.local >= LOCAL_SENTINEL_BASE {
        SymbolId::new(id.file, base + (id.local - LOCAL_SENTINEL_BASE))
    } else {
        id
    }
}

/// One body's complete Pass 2 output, still containing sentinel ids
/// (see [`LOCAL_SENTINEL_BASE`]) for any symbol it locally declared.
/// `crate::BoundProgram::from_files` merges `pending_locals` into the
/// shared `SymbolTable` and remaps `scopes`/`refs` accordingly, once per
/// body -- safe to do for many bodies' outputs in any order, including
/// interleaved from multiple threads, since each body's own remap only
/// touches its own data.
pub(crate) struct BoundBody {
    pub(crate) scopes: ScopeTree,
    pub(crate) pending_locals: Vec<Symbol>,
    pub(crate) refs: ReferenceTable,
}

/// One method/constructor/property-accessor body's binding context:
/// shared read-only project state (`table`, `schema`), this body's own
/// local write targets (`refs`, `scopes`, `pending_locals`), all owned
/// so binding never needs to coordinate with any other body running
/// concurrently.
pub(crate) struct BodyBinder<'a> {
    pub(crate) table: &'a SymbolTable,
    pub(crate) schema: &'a SchemaIndex,
    pub(crate) refs: ReferenceTable,
    pub(crate) scopes: ScopeTree,
    pending_locals: Vec<Symbol>,
    pub(crate) file: FileId,
    /// The enclosing type, for `this`/`super`/unqualified member lookup
    /// fallthrough once local-scope lookup misses. `None` when binding a
    /// trigger's top-level body (a `TriggerUnit` isn't itself a type
    /// symbol with members to fall through to).
    enclosing_type: Option<SymbolId>,
    /// The enclosing method/constructor, if any -- becomes the
    /// `container` of any local symbol declared inside. `None` when
    /// binding a field/property initializer expression directly (no
    /// enclosing method; `declare_local` is never reachable from a bare
    /// expression walk anyway, since only statements declare locals).
    enclosing_member: Option<SymbolId>,
}

/// Binds one method/constructor/property-accessor `Block` body: builds
/// its `ScopeTree` seeded with `params` in the root scope, walks every
/// statement/expression, and returns the (still sentinel-tagged) result.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
pub(crate) fn bind_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    enclosing_member: Option<SymbolId>,
    params: &[SymbolId],
    block: &Block,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member,
    };
    for &p in params {
        let name = binder.table.get(p).name.clone();
        binder.scopes.bind(root_scope, name, p);
    }
    binder.bind_block_stmts(root_scope, block);
    binder.into_bound_body()
}

/// Binds a `TriggerBlock`'s bare top-level statements -- the trigger's
/// actual executable body, which `TriggerBlock::members()` (declaration-
/// shaped children only) never reaches. Walks direct `Stmt` children
/// alongside (interleaved with, in source order) the `Member`
/// declarations Pass 1 already collected separately.
#[hotpath::measure]
pub(crate) fn bind_trigger_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    block: &TriggerBlock,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member: None,
    };
    for child in block.syntax().children() {
        if let Some(stmt) = Stmt::cast(child) {
            binder.bind_stmt(root_scope, &stmt);
        }
    }
    binder.into_bound_body()
}

/// Binds a bare expression with no enclosing statement context (a
/// field/property initializer) -- member lookup only, no locals, no
/// `ScopeTree` worth keeping around afterward.
#[hotpath::measure]
pub(crate) fn bind_initializer(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    expr: &Expr,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, expr.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member: None,
    };
    binder.bind_expr(root_scope, expr);
    binder.into_bound_body()
}

/// Resolves a single bare object-name reference (a trigger's `ON
/// <object>`) against `schema`, wrapped as a `BoundBody` purely so it
/// merges through the same sequential path as every other Pass 2
/// result -- there's no body/scope involved, so `scopes`/`pending_locals`
/// are empty and this entry is never inserted into
/// `BoundProgram`'s scope-tree map (its caller passes `key: None`).
pub(crate) fn bind_object_ref(schema: &SchemaIndex, ptr: SyntaxPtr, name: &str) -> BoundBody {
    let mut refs = ReferenceTable::default();
    crate::schema_index::resolve_object(schema, &mut refs, ptr, name);
    let (scopes, _) = ScopeTree::new_root(ScopeKind::Body, rowan::TextRange::empty(0.into()));
    BoundBody {
        scopes,
        pending_locals: Vec::new(),
        refs,
    }
}

/// Resolves a plain Apex `Type` reference (a field/param/local/return
/// type, a `new`/`instanceof`/cast target, an `extends`/`implements`
/// supertype, ...) against project-local types first, then
/// `apex-metadata`'s schema (an SObject-typed declaration, e.g. `Account
/// a;`) -- recursively resolving (and registering `refs` resolutions
/// for) any type arguments along the way regardless of which case the
/// base name itself falls into, since `List<Account>`'s `Account` is a
/// real, independently-resolvable reference in its own right. Returns
/// the resulting [`Ty`] either way -- the "one hop" other resolvers
/// chain through, now never losing the type entirely just because it
/// isn't project-local (see [`Ty`]'s own doc comment).
///
/// A free function, not a `BodyBinder` method, specifically so a
/// declaration's own type (a field/property/parameter/method-return
/// type, an `extends`/`implements` clause) can share this exact
/// resolution rule even though none of those have a body to walk, and
/// so no `BodyBinder` to be bound through -- see [`bind_type_ref`].
pub(crate) fn resolve_type_ref(
    table: &SymbolTable,
    schema: &SchemaIndex,
    refs: &mut ReferenceTable,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    ty: &Type,
) -> Option<Ty> {
    let name = ty.text();
    let ptr = SyntaxPtr::new(file, ty.syntax());
    let segments = ty.base_name_tokens();
    // `ty.syntax().text_range()` can be wider than the type name itself
    // (this parser attaches trailing trivia -- almost always at least
    // one space, e.g. before a variable's own name in `Widget w` -- as a
    // child *inside* the `Type` node), but this doesn't pre-record a
    // narrowed range the way `bind_new_expr` does for a `new` call's own
    // type: `Type` references are common enough in real code that eager
    // per-reference storage here measurably regressed bind time (see
    // `bind_name_expr`'s doc comment for the same reasoning, which found
    // this the hard way). `BoundProgram::highlight_range` computes the
    // last dotted segment's own token range on demand instead, only when
    // a documentHighlight/references/rename request actually asks.
    if segments.len() > 1 {
        record_qualified_segments(table, refs, file, &segments);
    }
    if let Some(id) = resolve_dotted_top_level(table, &segments) {
        refs.set(ptr, Resolution::Resolved(id));
        return Some(Ty::Project(id));
    }
    // A bare, unqualified reference to a *nested* type from within its
    // own enclosing type (or a subclass) doesn't need qualifying --
    // real NPSP shape: `TDTM_Runnable`'s own abstract `run` method
    // returns `List<DmlWrapper>`, not `List<TDTM_Runnable.DmlWrapper>`.
    // `resolve_dotted_top_level` above only ever checks `SymbolTable::top_level`
    // (top-level type names only), so this never resolved without a
    // separate check against the *lexically* enclosing type's own (and
    // inherited) nested types -- the same fallback unqualified member
    // lookup already gets via `SymbolTable::lookup_member`. Only
    // attempted for a single-segment name: a partially-qualified deeper
    // path (`Outer.Inner` referenced from three levels of nesting down)
    // is a rarer shape not covered here.
    //
    // Walks outward through the *whole* lexical nesting chain, not just
    // the reference site's immediate enclosing type -- a reference from
    // one nested class to a *sibling* nested class (both declared
    // directly in a shared outer class, neither one being the outer
    // class itself nor extending the other -- real NPSP shape:
    // `fflib_SObjectDomain.TestSObjectDomainConstructor.construct`
    // referencing `TestSObjectDomain` unqualified) has to check each
    // enclosing level's own (and inherited) nested types in turn, since
    // real Apex resolves an unqualified name through the full enclosing-
    // scope chain, not just the one immediate container.
    if segments.len() == 1 {
        let mut current = enclosing_type;
        while let Some(container) = current {
            if let Some(id) = table.nested_type_visible_from(container, &name) {
                refs.set(ptr, Resolution::Resolved(id));
                return Some(Ty::Project(id));
            }
            current = table.get(container).container;
        }
    }
    let args: Vec<Ty> = ty
        .type_args()
        .map(|list| {
            list.args()
                .filter_map(|arg| resolve_type_ref(table, schema, refs, file, enclosing_type, &arg))
                .collect()
        })
        .unwrap_or_default();
    if schema.object(&name).is_some() {
        refs.set(
            ptr,
            Resolution::SchemaObject(Box::new(SchemaObjectRef {
                object: name.clone(),
                field: None,
            })),
        );
        return Some(Ty::system_owned(name, args));
    }
    // Could be a standard object this repo never locally extended
    // (indistinguishable, using `apex-metadata` alone, from a genuinely
    // nonexistent name), or an unmodeled system/library type (`String`,
    // `List`, an `Exception` subtype, ...) -- v1 can't tell these apart,
    // so both land here as `Unresolved` rather than one of them being
    // misreported as `UnknownSchema`. The name is still real, though, so
    // the returned `Ty` keeps it.
    refs.set(ptr, Resolution::Unresolved);
    Some(Ty::system_owned(name, args))
}

/// Resolves a type's dotted base-name path (`Outer.Inner` -- the first
/// segment via `SymbolTable::top_level`, then each further segment as a
/// nested type declared directly on the previous one via
/// `SymbolTable::nested_type`) one segment at a time. The overwhelmingly
/// common single-segment case (`Account`) is just the one `top_level`
/// lookup, unchanged from before qualified nested-type references
/// (`fflib_Application.UnitOfWorkFactory`, a common Enterprise-pattern
/// shape) were handled at all -- previously `resolve_type_ref` looked up
/// the *whole* dotted string as one name via `top_level`, which only
/// indexes top-level (undotted) type names, so any qualified reference
/// to a nested type silently fell all the way through to `Unresolved`.
/// `segments` comes from `Type::base_name_tokens`, so array brackets and
/// any `TypeArgList` are already excluded.
fn resolve_dotted_top_level(
    table: &SymbolTable,
    segments: &[apex_syntax::SyntaxToken],
) -> Option<SymbolId> {
    let mut iter = segments.iter();
    let mut current = table.top_level(iter.next()?.text())?;
    for seg in iter {
        current = table.nested_type(current, seg.text())?;
    }
    Some(current)
}

/// A qualified `Outer.Inner` type reference is one flat `Type` node --
/// there's no separate node for `Outer` to independently resolve
/// through the way a `FieldExpr`'s receiver gets its own `NameExpr`
/// (see `resolve_dotted_top_level`'s doc comment). Without this,
/// clicking anywhere in `TDTM_Runnable.DmlWrapper` -- whichever segment
/// -- resolved to whatever the *whole* path resolved to (`DmlWrapper`,
/// the last segment), never `TDTM_Runnable` itself. This records each
/// segment's own *prefix* resolution instead (`TDTM_Runnable` ->
/// `TDTM_Runnable` itself, `TDTM_Runnable.DmlWrapper` -> `DmlWrapper`),
/// keyed by that segment's own token (`SyntaxPtr::for_token`), which
/// `BoundProgram::resolution_at` checks before falling back to the
/// whole-node lookup `resolve_type_ref` still also records. A prefix
/// that itself failed to resolve makes every later segment `Unresolved`
/// too, not silently unrecorded -- hovering `Bogus` in `Bogus.Inner`
/// should say so, not fall through to whatever the whole node says.
/// Only called for an actually-dotted path: the single-segment case
/// (the overwhelming majority of type references) would otherwise
/// double `ReferenceTable`'s size with a redundant entry at an
/// identical range under a different `SyntaxKind`.
fn record_qualified_segments(
    table: &SymbolTable,
    refs: &mut ReferenceTable,
    file: FileId,
    segments: &[apex_syntax::SyntaxToken],
) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    let mut current = table.top_level(first.text());
    refs.set(
        SyntaxPtr::for_token(file, first),
        current.map_or(Resolution::Unresolved, Resolution::Resolved),
    );
    for seg in rest {
        current = current.and_then(|prev| table.nested_type(prev, seg.text()));
        refs.set(
            SyntaxPtr::for_token(file, seg),
            current.map_or(Resolution::Unresolved, Resolution::Resolved),
        );
    }
}

/// Binds a single declaration-site type reference with no enclosing
/// body at all -- a field/property/parameter's own type, a method's
/// return type, or one `extends`/`implements` supertype name. Wrapped
/// as a `BoundBody` purely so it merges through the same sequential
/// path as every other Pass 2 result, same idea as [`bind_object_ref`].
pub(crate) fn bind_type_ref(
    table: &SymbolTable,
    schema: &SchemaIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    ty: &Type,
) -> BoundBody {
    let mut refs = ReferenceTable::default();
    resolve_type_ref(table, schema, &mut refs, file, enclosing_type, ty);
    let (scopes, _) = ScopeTree::new_root(ScopeKind::Body, rowan::TextRange::empty(0.into()));
    BoundBody {
        scopes,
        pending_locals: Vec::new(),
        refs,
    }
}

impl<'a> BodyBinder<'a> {
    fn into_bound_body(self) -> BoundBody {
        BoundBody {
            scopes: self.scopes,
            pending_locals: self.pending_locals,
            refs: self.refs,
        }
    }

    fn declare_local(
        &mut self,
        kind: SymbolKind,
        name: &Name,
        type_ref: Option<&Type>,
    ) -> SymbolId {
        let (type_ptr, type_name, type_args) = match type_ref {
            Some(ty) => {
                let args = ty
                    .type_args()
                    .map(|list| list.args().map(|a| a.text()).collect())
                    .unwrap_or_default();
                (Some(AstPtr::new(self.file, ty)), Some(ty.text()), args)
            }
            None => (None, None, Vec::new()),
        };
        let symbol = Symbol {
            kind,
            name: name.text().unwrap_or_default(),
            file: self.file,
            ptr: SyntaxPtr::new(self.file, name.syntax()),
            name_range: name.ident_range(),
            container: self.enclosing_member,
            type_ref: type_ptr,
            type_name,
            type_args,
            modifiers: ModifierSet::default(),
        };
        let id = SymbolId::new(
            self.file,
            LOCAL_SENTINEL_BASE + self.pending_locals.len() as u32,
        );
        self.pending_locals.push(symbol);
        id
    }

    /// `id` resolved against this body's own pending local arena if it's
    /// a sentinel id (see [`LOCAL_SENTINEL_BASE`]), otherwise against the
    /// shared, already-final `SymbolTable` -- the two id spaces a body
    /// can encounter (a symbol it just locally declared, vs. any
    /// pre-existing project symbol) need this indirection since the
    /// shared table doesn't contain this body's locals yet.
    fn get_symbol(&self, id: SymbolId) -> &Symbol {
        if id.local >= LOCAL_SENTINEL_BASE {
            &self.pending_locals[(id.local - LOCAL_SENTINEL_BASE) as usize]
        } else {
            self.table.get(id)
        }
    }

    /// `symbol`'s declared type, as a [`Ty`] -- the "type of this
    /// expression" chaining `FieldExpr`/`MethodCallExpr` target
    /// resolution needs. `Ty::Project` when the declared type is a
    /// project-local class/interface/enum; otherwise `Ty::System` naming
    /// whatever was declared (a schema object, an unmodeled system type,
    /// a generic collection with its type argument(s) substituted in --
    /// see `Symbol::type_args`), never lost to `None` just because it
    /// isn't project-local. `None` only when the symbol has no type of
    /// its own at all (an enum constant, a `void` method, a constructor).
    ///
    /// A type declaration itself (`Class`/`Interface`/`Enum`) is *not*
    /// one of those `None` cases, even though it has no `type_name` of
    /// its own the way a field does: resolving a chain link to a type
    /// (`UTIL_IntegrationConfig.Integration` inside `...Integration.ArchiveBridge`,
    /// `TDTM_Runnable.DmlWrapper`) means *that type itself* is now the
    /// receiver for whatever comes next, exactly like `resolve_type_ref`
    /// already treats a resolved type name as `Ty::Project(id)`. Without
    /// this, chaining past a nested type/enum used as a qualifier always
    /// went straight to `Unresolved` from there on, no matter how
    /// visible the final member was.
    fn type_of_symbol(&self, id: SymbolId) -> Option<Ty> {
        let symbol = self.get_symbol(id);
        if matches!(
            symbol.kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        ) {
            return Some(Ty::Project(id));
        }
        let type_name = symbol.type_name.as_deref()?;
        // `resolve_dotted_name` handles both the overwhelmingly common
        // single-segment case (`Account`) and a fully-qualified nested
        // one (`Outer.Inner`, resolved from a real top-level `Outer`) in
        // one call -- a plain `top_level(type_name)` here used to miss
        // every qualified case, since `top_level` is keyed by simple
        // declared name only. Real NPSP shape this fixed:
        // `UTIL_CurrencyCache.CurrencyData currData = ...;` declared
        // *inside* `UTIL_CurrencyCache` itself -- `currData`'s own
        // `type_name` is the whole dotted string, and without this,
        // every `currData.someField = ...` assignment stayed
        // `Unresolved`, wrongly flagging genuinely-written-to fields/
        // properties (`IsoCode`, `defaultRate`) as dead.
        if let Some(project_id) = self.table.resolve_dotted_name(type_name) {
            return Some(Ty::Project(project_id));
        }
        // A member (field/property/parameter) declared with an
        // *unqualified* reference to a nested type as its own type --
        // same gap `resolve_type_ref`'s single-segment fallback had, and
        // the same fix: walk outward through the whole lexical nesting
        // chain starting from `symbol`'s own enclosing type, not just
        // `top_level`. Real NPSP shape: `fflib_SObjectDomain`'s
        // `Configuration` property is declared with its sibling-nested
        // `Configuration` class as its type -- without this,
        // `Configuration.OldOnUpdateValidateBehaviour` resolved
        // `Configuration` fine (a plain member lookup) but could never
        // chain to `OldOnUpdateValidateBehaviour` after the dot, since
        // this method never produced a `Ty::Project` to look members up
        // against. Only attempted for a single-segment name, matching
        // `resolve_type_ref`'s own identical restriction -- a dotted
        // name that already failed `resolve_dotted_name` above named a
        // real (if unresolvable) top-level type as its first segment,
        // not an unqualified nested-type reference to retry here.
        if !type_name.contains('.') {
            let mut current = crate::enclosing_type_of(self.table, symbol);
            while let Some(container) = current {
                if let Some(id) = self.table.nested_type_visible_from(container, type_name) {
                    return Some(Ty::Project(id));
                }
                current = self.table.get(container).container;
            }
        }
        let args = symbol
            .type_args
            .iter()
            .map(|name| match self.table.resolve_dotted_name(name) {
                Some(id) => Ty::Project(id),
                None => Ty::system_owned(name.clone(), Vec::new()),
            })
            .collect();
        Some(Ty::system_owned(type_name.to_string(), args))
    }

    /// A body-context type reference (a local/param/return type, a
    /// `new`/`instanceof`/cast target, ...) -- delegates to the free
    /// function [`resolve_type_ref`], which also backs declaration-site
    /// type resolution (a field/property/parameter/method-return type,
    /// an `extends`/`implements` supertype) that has no body to walk and
    /// so no `BodyBinder` to be a method on.
    pub(crate) fn resolve_type_ref(&mut self, ty: &Type) -> Option<Ty> {
        resolve_type_ref(
            self.table,
            self.schema,
            &mut self.refs,
            self.file,
            self.enclosing_type,
            ty,
        )
    }

    fn bind_block_stmts(&mut self, scope: ScopeId, block: &Block) {
        for stmt in block.statements() {
            self.bind_stmt(scope, &stmt);
        }
    }

    fn bind_child_block(&mut self, parent: ScopeId, block: &Block) {
        let child = self
            .scopes
            .push(Some(parent), ScopeKind::Block, block.syntax().text_range());
        self.bind_block_stmts(child, block);
    }

    fn bind_stmt(&mut self, scope: ScopeId, stmt: &Stmt) {
        match stmt {
            Stmt::Block(b) => self.bind_child_block(scope, b),
            Stmt::If(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                if let Some(t) = s.then_branch() {
                    self.bind_stmt(scope, &t);
                }
                if let Some(e) = s.else_branch() {
                    self.bind_stmt(scope, &e);
                }
            }
            Stmt::Switch(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                for when in s.when_clauses() {
                    let range = when.syntax().text_range();
                    let child = self.scopes.push(Some(scope), ScopeKind::Switch, range);
                    if let Some(value) = when.value() {
                        if let Some(ty) = value.type_ref() {
                            self.resolve_type_ref(&ty);
                        }
                        if let Some(binding) = value.binding() {
                            let sym = self.declare_local(
                                SymbolKind::SwitchBindingVar,
                                &binding,
                                value.type_ref().as_ref(),
                            );
                            if let Some(text) = binding.text() {
                                self.scopes.bind(child, text, sym);
                            }
                        }
                    }
                    if let Some(body) = when.body() {
                        self.bind_block_stmts(child, &body);
                    }
                }
            }
            Stmt::For(s) => {
                let child = self
                    .scopes
                    .push(Some(scope), ScopeKind::For, s.syntax().text_range());
                if let Some(init) = s.init() {
                    if let Some(ty) = init.type_ref() {
                        self.resolve_type_ref(&ty);
                    }
                    for d in init.declarators() {
                        if let Some(e) = d.init() {
                            self.bind_expr(child, &e);
                        }
                        if let Some(name) = d.name() {
                            let sym = self.declare_local(
                                SymbolKind::LocalVar,
                                &name,
                                init.type_ref().as_ref(),
                            );
                            if let Some(text) = name.text() {
                                self.scopes.bind(child, text, sym);
                            }
                        }
                    }
                    for e in init.exprs() {
                        self.bind_expr(child, &e);
                    }
                }
                if let Some(c) = s.condition() {
                    self.bind_expr(child, &c);
                }
                if let Some(update) = s.update() {
                    for e in update.exprs() {
                        self.bind_expr(child, &e);
                    }
                }
                if let Some(body) = s.body() {
                    self.bind_stmt(child, &body);
                }
            }
            Stmt::ForEach(s) => {
                let child = self
                    .scopes
                    .push(Some(scope), ScopeKind::For, s.syntax().text_range());
                if let Some(ty) = s.type_ref() {
                    self.resolve_type_ref(&ty);
                }
                if let Some(iterable) = s.iterable() {
                    self.bind_expr(child, &iterable);
                }
                if let Some(name) = s.name() {
                    let sym =
                        self.declare_local(SymbolKind::ForEachVar, &name, s.type_ref().as_ref());
                    if let Some(text) = name.text() {
                        self.scopes.bind(child, text, sym);
                    }
                }
                if let Some(body) = s.body() {
                    self.bind_stmt(child, &body);
                }
            }
            Stmt::While(s) => {
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
                if let Some(b) = s.body() {
                    self.bind_stmt(scope, &b);
                }
            }
            Stmt::DoWhile(s) => {
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
                if let Some(c) = s.condition() {
                    self.bind_expr(scope, &c);
                }
            }
            Stmt::Try(s) => {
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
                for catch in s.catch_clauses() {
                    if let Some(ty) = catch.exception_type() {
                        let ptr = SyntaxPtr::new(self.file, ty.syntax());
                        // `QualifiedName` (not `Type`) -- resolve by its
                        // whole-text base name against project types
                        // only; unlike `resolve_type_ref`, exception
                        // types are never SObject-shaped, so there's no
                        // schema fallback to attempt here.
                        //
                        // `ty.syntax().text_range()` can be wider than the
                        // name (trailing trivia; see `bind_name_expr`'s
                        // doc comment) -- `BoundProgram::highlight_range`
                        // trims it on demand instead of pre-storing a
                        // narrowed range for this rare a reference kind.
                        // The *name string* itself needs the same
                        // trimming: `ty.syntax().text()` includes that
                        // same trailing trivia (confirmed empirically --
                        // `"MyException "`, trailing space and all, for
                        // `catch (MyException e)`), so this must trim
                        // before ever looking it up, or *every* catch-
                        // clause exception type -- dotted or not --
                        // always missed `top_level`/`resolve_dotted_name`
                        // and stayed permanently `Unresolved`.
                        let name = ty.syntax().text().to_string();
                        match self.table.resolve_dotted_name(name.trim()) {
                            Some(id) => self.refs.set(ptr, Resolution::Resolved(id)),
                            None => self.refs.set(ptr, Resolution::Unresolved),
                        }
                    }
                    let child = self.scopes.push(
                        Some(scope),
                        ScopeKind::Catch,
                        catch.syntax().text_range(),
                    );
                    if let Some(name) = catch.name() {
                        let sym = self.declare_local(SymbolKind::CatchVar, &name, None);
                        if let Some(text) = name.text() {
                            self.scopes.bind(child, text, sym);
                        }
                    }
                    if let Some(body) = catch.body() {
                        self.bind_block_stmts(child, &body);
                    }
                }
                if let Some(f) = s.finally_clause() {
                    if let Some(body) = f.body() {
                        self.bind_child_block(scope, &body);
                    }
                }
            }
            Stmt::Return(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Throw(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::Insert(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Update(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Delete(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Undelete(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::Upsert(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
                // The external-ID field reference isn't resolved against
                // schema in v1 -- it needs the DML target expression's
                // *type* (the SObject being upserted) known first, which
                // is exactly the inference v1 doesn't attempt.
            }
            Stmt::Merge(s) => {
                if let Some(e) = s.master() {
                    self.bind_expr(scope, &e);
                }
                if let Some(e) = s.duplicate() {
                    self.bind_expr(scope, &e);
                }
            }
            Stmt::RunAs(s) => {
                for e in s.args() {
                    self.bind_expr(scope, &e);
                }
                if let Some(b) = s.body() {
                    self.bind_child_block(scope, &b);
                }
            }
            Stmt::LocalVarDecl(s) => {
                if let Some(ty) = s.type_ref() {
                    self.resolve_type_ref(&ty);
                }
                for d in s.declarators() {
                    if let Some(e) = d.init() {
                        self.bind_expr(scope, &e);
                    }
                    if let Some(name) = d.name() {
                        let sym =
                            self.declare_local(SymbolKind::LocalVar, &name, s.type_ref().as_ref());
                        if let Some(text) = name.text() {
                            self.scopes.bind(scope, text, sym);
                        }
                    }
                }
            }
            Stmt::Expr(s) => {
                if let Some(e) = s.expr() {
                    self.bind_expr(scope, &e);
                }
            }
        }
    }

    /// Walks `expr` and every subexpression, resolving references along
    /// the way. Returns `expr`'s inferred [`Ty`], when known -- `None`
    /// otherwise (an expression this binder genuinely can't type at all,
    /// e.g. the result of an unresolved call). A call expression's type
    /// is its resolved method/constructor's declared return type (only
    /// available when overload resolution narrowed to exactly one
    /// candidate).
    pub(crate) fn bind_expr(&mut self, scope: ScopeId, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Literal(lit) => lit.token().and_then(|t| Ty::for_literal(t.kind())),
            Expr::Name(n) => self.bind_name_expr(scope, n),
            Expr::This(t) => {
                // `this` itself gets a recorded `Resolution` (pointing at
                // the enclosing type) distinct from whatever `this` is
                // qualifying (e.g. `this.field`'s `FieldExpr`, or
                // `this.method()`'s `MethodCallExpr`) -- without this, a
                // click on the bare `this` token had nothing of its own
                // to resolve to, so `BoundProgram::resolution_at`'s
                // ancestor climb fell through to the nearest enclosing
                // node that *did* have one, landing on the member being
                // accessed instead of `this` itself.
                let enclosing = self.enclosing_type;
                self.refs.set(
                    SyntaxPtr::new(self.file, t.syntax()),
                    enclosing.map_or(Resolution::Unresolved, Resolution::Resolved),
                );
                enclosing.map(Ty::Project)
            }
            Expr::Super(s) => {
                // Same idea as `this` above, but resolving to the direct
                // `extends` target instead of the enclosing type itself.
                let direct_super = self.enclosing_type.and_then(|t| self.table.direct_super(t));
                self.refs.set(
                    SyntaxPtr::new(self.file, s.syntax()),
                    direct_super.map_or(Resolution::Unresolved, Resolution::Resolved),
                );
                direct_super.map(Ty::Project)
            }
            Expr::Paren(p) => p.inner().and_then(|inner| self.bind_expr(scope, &inner)),
            Expr::Cast(c) => {
                if let Some(op) = c.operand() {
                    self.bind_expr(scope, &op);
                }
                c.type_ref().and_then(|t| self.resolve_type_ref(&t))
            }
            Expr::Bin(b) => {
                let lhs_ty = b.lhs().and_then(|l| self.bind_expr(scope, &l));
                let rhs_ty = b.rhs().and_then(|r| self.bind_expr(scope, &r));
                let op_text: String = b
                    .operator_tokens()
                    .iter()
                    .map(|t| t.text())
                    .collect::<Vec<_>>()
                    .join("");
                Ty::for_bin_op(&op_text, lhs_ty, rhs_ty)
            }
            Expr::Unary(u) => {
                let operand_ty = u.operand().and_then(|o| self.bind_expr(scope, &o));
                match u.operator().map(|t| t.text().to_string()).as_deref() {
                    Some("!") => Some(Ty::boolean()),
                    // `~`/unary `+`/`-`/prefix `++`/`--` all preserve the
                    // operand's own type.
                    _ => operand_ty,
                }
            }
            Expr::Postfix(p) => {
                // `++`/`--` only -- always preserves the operand's type.
                p.operand().and_then(|o| self.bind_expr(scope, &o))
            }
            Expr::Ternary(t) => {
                if let Some(c) = t.condition() {
                    self.bind_expr(scope, &c);
                }
                let then_ty = t.then_branch().and_then(|e| self.bind_expr(scope, &e));
                let else_ty = t.else_branch().and_then(|e| self.bind_expr(scope, &e));
                // No common-supertype inference when both branches are
                // known but disagree (that needs the same generics/
                // stdlib depth this step deliberately isn't building) --
                // just prefer `then`, falling back to `else` only when
                // `then`'s own type isn't known at all.
                then_ty.or(else_ty)
            }
            Expr::Instanceof(i) => {
                if let Some(o) = i.operand() {
                    self.bind_expr(scope, &o);
                }
                if let Some(t) = i.type_ref() {
                    self.resolve_type_ref(&t);
                }
                Some(Ty::boolean())
            }
            Expr::Field(f) => self.bind_field_expr(scope, f),
            Expr::Index(idx) => {
                if let Some(t) = idx.target() {
                    self.bind_expr(scope, &t);
                }
                if let Some(i) = idx.index() {
                    self.bind_expr(scope, &i);
                }
                // v1 doesn't model List<T>/Map<K,V> element-type
                // inference for indexing syntax (only for the
                // `crate::generics` method-call table `[...]` doesn't go
                // through), so an indexing expression's own type is
                // always unknown.
                None
            }
            Expr::Call(c) => self.bind_call_expr(scope, c),
            Expr::MethodCall(mc) => self.bind_method_call_expr(scope, mc),
            Expr::New(ne) => self.bind_new_expr(scope, ne),
            Expr::Soql(sq) => {
                crate::soql::bind_soql(self, scope, sq);
                None
            }
            Expr::Sosl(ss) => {
                crate::soql::bind_sosl(self, scope, ss);
                None
            }
        }
    }

    fn bind_name_expr(&mut self, scope: ScopeId, n: &NameExpr) -> Option<Ty> {
        if let Some(ty) = n.type_ref() {
            // The `List<Foo>.class` reflection form -- a type reference,
            // not a name lookup.
            return self.resolve_type_ref(&ty);
        }
        let tok = n.name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, n.syntax());
        // The whole `NameExpr` node's own range can be wider than the
        // identifier it wraps (this parser attaches trailing trivia --
        // almost always at least one space -- as a child *inside* the
        // node, not as leading trivia of whatever follows), but unlike
        // `bind_field_expr`/`bind_method_call_expr`/`bind_call_expr`/
        // `bind_new_expr`'s `set_with_highlight` calls, this doesn't
        // pre-record a narrowed range here: `NameExpr` is by far the most
        // common reference kind in real code (every local/field/param
        // *read*, not just calls), so eagerly storing a second range per
        // reference measurably regressed cold/warm bind time (~20-30%,
        // not just the ~expected storage-doubling cost the far rarer
        // call/field kinds already accepted). `BoundProgram::highlight_range`
        // instead computes this on demand, only when a documentHighlight/
        // references/rename request actually asks for it.
        if let Some(local) = self.scopes.resolve_local(scope, name) {
            self.refs.set(ptr, Resolution::Resolved(local));
            return self.type_of_symbol(local);
        }

        // Walks outward through the whole lexical nesting chain, not just
        // the reference site's immediate enclosing type -- a nested class
        // referencing its outer class's static field/constant unqualified
        // (real Apex: nested classes see the enclosing type's members the
        // same way a qualified reference to it would) otherwise never
        // resolved past the nested class's own (and inherited) members.
        // Mirrors `resolve_type_ref`'s and `type_of_symbol`'s identical
        // climb for the type-reference and declared-type cases.
        let mut enclosing_chain = self.enclosing_type;
        while let Some(container) = enclosing_chain {
            // Methods/constructors are excluded from plain-name
            // resolution: a bare `NameExpr` naming a method with no call
            // wouldn't compile in real Apex, and including them here
            // would let a field and a same-named method collide in the
            // candidate set for no reason -- `CallExpr`/`MethodCallExpr`
            // handle the call-position case separately.
            let members: Vec<SymbolId> = self
                .table
                .lookup_member(container, name)
                .into_iter()
                .filter(|&id| {
                    !matches!(
                        self.table.get(id).kind,
                        SymbolKind::Method | SymbolKind::Constructor
                    ) && self.table.is_visible_from(id, self.enclosing_type)
                })
                .collect();
            // A field/property named identically to a *nested type* also
            // declared in the same enclosing class is a real, idiomatic
            // Apex pattern (NPSP shape: `fflib_SObjectDomain`'s
            // `Configuration Configuration { get; private set; }`) --
            // `lookup_member` above finds both (nested types are indexed
            // as members alongside fields/properties, with no kind
            // partitioning), which used to land in the `Candidates` arm
            // below and give up before ever inferring a type to chain
            // `.member` off of. In expression position the *value* always
            // wins over the same-named type -- the same "instance member
            // shadows a type name" precedence `static_member_access.rs`
            // already documents for the separate top-level-type fallback,
            // extended here to when both candidates come from member
            // lookup itself. Only actually drops anything when a
            // non-type alternative exists; a lone nested-type match (the
            // common "bare reference to a sibling nested type" case) is
            // untouched.
            let members = if members.len() > 1 {
                let non_type: Vec<SymbolId> = members
                    .iter()
                    .copied()
                    .filter(|&id| {
                        !matches!(
                            self.table.get(id).kind,
                            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
                        )
                    })
                    .collect();
                if non_type.is_empty() {
                    members
                } else {
                    non_type
                }
            } else {
                members
            };
            match members.as_slice() {
                [] => {} // try the next-outer enclosing level, if any
                [one] => {
                    self.refs.set(ptr, Resolution::Resolved(*one));
                    return self.type_of_symbol(*one);
                }
                many => {
                    self.refs.set(ptr, Resolution::Candidates(many.to_vec()));
                    return None;
                }
            }
            enclosing_chain = self.table.get(container).container;
        }

        // Neither a local/param nor a member of the enclosing type -- a
        // bare name can also legally name a project-local top-level type
        // itself, used as the receiver of a static member/method access
        // (`UtilClass.staticMethod(...)`, `MyClass.MY_CONSTANT`) rather
        // than as a value in its own right. Checked last, after member
        // lookup, matching real Apex/Java semantics: an instance member
        // shadows a same-named type in expression position.
        if let Some(type_id) = self.table.top_level(name) {
            self.refs.set(ptr, Resolution::Resolved(type_id));
            return Some(Ty::Project(type_id));
        }
        // Or an SObject used as the receiver of the `Type.Field` token
        // form (`DataImport__c.Status__c`, most often passed straight to
        // `String.valueOf(...)` for the field's API name) -- the same
        // schema fallback `resolve_type_ref` already has for a *type*
        // reference, missing here for the *expression* one. Without
        // this, `DataImport__c` itself never resolved, so the
        // `FieldExpr` chained off it (`bind_field_expr`'s own `Ty::System`
        // fallback) never got a target type to resolve the field
        // against either -- both the object and every field on it
        // failed together, not independently.
        if self.schema.object(name).is_some() {
            self.refs.set(
                ptr,
                Resolution::SchemaObject(Box::new(SchemaObjectRef {
                    object: SmolStr::new(name),
                    field: None,
                })),
            );
            return Some(Ty::system_owned(SmolStr::new(name), Vec::new()));
        }

        self.refs.set(ptr, Resolution::Unresolved);
        None
    }

    fn bind_field_expr(&mut self, scope: ScopeId, f: &FieldExpr) -> Option<Ty> {
        let target_type = f.target().and_then(|t| self.bind_expr(scope, &t));
        let tok = f.member_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, f.syntax());
        let highlight = tok.text_range();

        let container = match target_type {
            Some(Ty::Project(container)) => container,
            // A schema SObject value (`this.dataImport` typed as the
            // custom object `DataImport__c`) -- `.Status__c` is a real
            // field access this binder *can* answer via `self.schema`,
            // even outside SOQL, so this isn't the same "no member
            // model" gap a genuinely unmodeled system type (`String`,
            // `List`, ...) is. Mirrors `crate::soql`'s own object/field
            // hop resolution, `__r` relationship alias included: `name`
            // is converted through `relationship_field_api_name` before
            // the schema lookup (a no-op for a plain field, which is
            // already its own real API name), and a lookup/master-detail
            // field's `reference_to` becomes the resulting `Ty::System`
            // so a further hop (`dataImport.Related__r.Name__c`) keeps
            // resolving instead of dead-ending after the first one.
            Some(Ty::System { name: object, .. }) => {
                let real_field_name = relationship_field_api_name(name);
                let field_schema = self.schema.field(&object, &real_field_name);
                let resolution = match &field_schema {
                    Some(_) => Resolution::SchemaObject(Box::new(SchemaObjectRef {
                        object: object.clone(),
                        field: Some(SmolStr::new(&real_field_name)),
                    })),
                    None if self.schema.object(&object).is_some() => {
                        Resolution::UnknownSchema(Box::new(UnknownSchemaRef {
                            object: Some(object.clone()),
                            field: Some(SmolStr::new(&real_field_name)),
                        }))
                    }
                    None => Resolution::Unresolved,
                };
                self.refs.set_with_highlight(ptr, highlight, resolution);
                return field_schema
                    .and_then(|f| f.reference_to.first())
                    .map(|next| Ty::system_owned(next.clone(), Vec::new()));
            }
            // `target_type` is entirely unknown -- honestly `Unresolved`
            // rather than a guess.
            None => {
                self.refs
                    .set_with_highlight(ptr, highlight, Resolution::Unresolved);
                return None;
            }
        };
        let members: Vec<SymbolId> = self
            .table
            .lookup_member(container, name)
            .into_iter()
            .filter(|&id| {
                !matches!(
                    self.table.get(id).kind,
                    SymbolKind::Method | SymbolKind::Constructor
                ) && self.table.is_visible_from(id, self.enclosing_type)
            })
            .collect();
        match members.as_slice() {
            [] => {
                self.refs
                    .set_with_highlight(ptr, highlight, Resolution::Unresolved);
                None
            }
            [one] => {
                self.refs
                    .set_with_highlight(ptr, highlight, Resolution::Resolved(*one));
                self.type_of_symbol(*one)
            }
            many => {
                self.refs.set_with_highlight(
                    ptr,
                    highlight,
                    Resolution::Candidates(many.to_vec()),
                );
                None
            }
        }
    }

    fn bind_method_call_expr(&mut self, scope: ScopeId, mc: &MethodCallExpr) -> Option<Ty> {
        let target_type = mc.target().and_then(|t| self.bind_expr(scope, &t));
        let mut arg_types = Vec::new();
        if let Some(args) = mc.args() {
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
        }
        let tok = mc.method_name_token()?;
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, mc.syntax());
        let highlight = tok.text_range();

        match target_type {
            Some(Ty::Project(container)) => {
                let methods: Vec<SymbolId> = self
                    .table
                    .lookup_member(container, name)
                    .into_iter()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Method
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                let resolution = widen_for_dynamic_dispatch(
                    self.table,
                    narrow_by_overload(self.table, methods, &arg_types),
                );
                let result_type = match &resolution {
                    Resolution::Resolved(id) => self.type_of_symbol(*id),
                    _ => None,
                };
                self.refs.set_with_highlight(ptr, highlight, resolution);
                result_type
            }
            Some(Ty::System { name: base, args }) => {
                // No `SymbolId` backs a built-in generic method -- there's
                // no real declaration for goto-definition to point at --
                // so the *reference* stays honestly `Unresolved` even
                // when the *type* it returns is known (see
                // `crate::generics`'s module doc comment). The returned
                // `Ty` still flows upward for further chaining, e.g.
                // `myList.get(0).Name` resolving `Name` when the list's
                // element type is project-local.
                self.refs
                    .set_with_highlight(ptr, highlight, Resolution::Unresolved);
                crate::generics::builtin_generic_member_type(&base, &args, name)
            }
            None => {
                self.refs
                    .set_with_highlight(ptr, highlight, Resolution::Unresolved);
                None
            }
        }
    }

    fn bind_call_expr(&mut self, scope: ScopeId, c: &CallExpr) -> Option<Ty> {
        let mut arg_types = Vec::new();
        if let Some(args) = c.args() {
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
        }
        let tok = c.callee_token()?;
        let ptr = SyntaxPtr::new(self.file, c.syntax());
        let highlight = tok.text_range();

        let (target_type, want_ctor) = match tok.kind() {
            SyntaxKind::This => (self.enclosing_type, true),
            SyntaxKind::Super => (
                self.enclosing_type.and_then(|t| self.table.direct_super(t)),
                true,
            ),
            _ => (self.enclosing_type, false),
        };
        let Some(container) = target_type else {
            self.refs
                .set_with_highlight(ptr, highlight, Resolution::Unresolved);
            return None;
        };

        let candidates: Vec<SymbolId> = if want_ctor {
            // `this(...)`/`super(...)` chain to a constructor, whose
            // declared name equals the class name -- never "this"/
            // "super" -- so this takes every constructor directly rather
            // than a name-based `lookup_member`.
            self.table
                .members_of(container)
                .iter()
                .copied()
                .filter(|&id| {
                    self.table.get(id).kind == SymbolKind::Constructor
                        && self.table.is_visible_from(id, self.enclosing_type)
                })
                .collect()
        } else {
            let name = tok.text();
            // Walks outward through the whole lexical nesting chain, not
            // just the call site's immediate enclosing type -- an
            // unqualified call from a nested class to a method declared
            // on its outer class (real Apex: a nested class sees the
            // enclosing type's static members unqualified) otherwise
            // never resolved past the nested class's own (and inherited)
            // members. Mirrors the identical climb `bind_name_expr` and
            // `type_of_symbol` already do for the name and declared-type
            // cases.
            let mut candidates = Vec::new();
            let mut enclosing_chain = Some(container);
            while let Some(level) = enclosing_chain {
                candidates = self
                    .table
                    .lookup_member(level, name)
                    .into_iter()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Method
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                if !candidates.is_empty() {
                    break;
                }
                enclosing_chain = self.table.get(level).container;
            }
            candidates
        };

        let resolution = narrow_by_overload(self.table, candidates, &arg_types);
        // Never widened for `want_ctor`: a constructor call always
        // instantiates the exact named type, so there's no dynamic
        // dispatch to expand across (see `widen_for_dynamic_dispatch`'s
        // own doc comment).
        let resolution = if want_ctor {
            resolution
        } else {
            widen_for_dynamic_dispatch(self.table, resolution)
        };
        // Constructor symbols never carry a `type_name` (see
        // `collect::collect_constructor`), so `type_of_symbol` is `None`
        // for the `this(...)`/`super(...)` case without needing a
        // separate `want_ctor` guard here.
        let result_type = match &resolution {
            Resolution::Resolved(id) => self.type_of_symbol(*id),
            _ => None,
        };
        self.refs.set_with_highlight(ptr, highlight, resolution);
        result_type
    }

    fn bind_new_expr(&mut self, scope: ScopeId, ne: &NewExpr) -> Option<Ty> {
        let resolved_type = ne.type_ref().and_then(|t| self.resolve_type_ref(&t));
        if let Some(args) = ne.args() {
            let mut arg_types = Vec::new();
            for a in args.args() {
                arg_types.push(self.bind_expr(scope, &a));
            }
            if let Some(&Ty::Project(container)) = resolved_type.as_ref() {
                let ctors: Vec<SymbolId> = self
                    .table
                    .members_of(container)
                    .iter()
                    .copied()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Constructor
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                if !ctors.is_empty() {
                    let resolution = narrow_by_overload(self.table, ctors, &arg_types);
                    // `new Outer.Inner(...)` names its constructor after
                    // the *last* segment (`Inner`) -- the type's own
                    // constructor, never `Outer`'s -- so this narrows to
                    // that segment alone rather than the whole dotted
                    // `Type` node, matching `bind_method_call_expr`'s own
                    // method-name-token narrowing.
                    let highlight = ne
                        .type_ref()
                        .and_then(|t| t.base_name_tokens().last().map(|tok| tok.text_range()))
                        .unwrap_or_else(|| ne.syntax().text_range());
                    self.refs.set_with_highlight(
                        SyntaxPtr::new(self.file, ne.syntax()),
                        highlight,
                        resolution,
                    );
                }
            }
        }
        if let Some(size) = ne.array_size() {
            self.bind_expr(scope, &size);
        }
        if let Some(init) = ne.initializer() {
            self.bind_initializer_expr(scope, &init);
        }
        resolved_type
    }

    fn bind_initializer_expr(&mut self, scope: ScopeId, init: &Initializer) {
        match init {
            Initializer::Array(a) => {
                for e in a.elements() {
                    self.bind_expr(scope, &e);
                }
            }
            Initializer::Set(s) => {
                for e in s.elements() {
                    self.bind_expr(scope, &e);
                }
            }
            Initializer::Map(m) => {
                for entry in m.entries() {
                    if let Some(k) = entry.key() {
                        self.bind_expr(scope, &k);
                    }
                    if let Some(v) = entry.value() {
                        self.bind_expr(scope, &v);
                    }
                }
            }
        }
    }
}
