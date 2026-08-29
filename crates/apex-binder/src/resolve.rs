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
//!   of system types: numeric widening, `Object`, `List`/`Set`/`Map`,
//!   `Id`, `Date`/`Datetime`/`Time`, `Blob`, and schema-verified
//!   `SObject` widening), then a most-specific tiebreak (`crate::conversions::is_more_specific`)
//!   among whatever still survives. A call resolves to a single
//!   `Resolved` symbol when exactly one candidate survives elimination,
//!   or when the tiebreak finds a unique most-specific one; otherwise
//!   it's `Candidates` (genuinely ambiguous, or argument/parameter types
//!   outside `crate::conversions`'s curated set) or `Unresolved` (no
//!   same-name member at all).

use crate::conversions;
use crate::file_id::FileId;
use crate::label_index::LabelIndex;
use crate::page_index::PageIndex;
use crate::ptr::{AstPtr, SyntaxPtr};
use crate::reference_table::{
    LabelRef, ReferenceTable, Resolution, SchemaObjectRef, StdlibMemberRef, UnknownSchemaRef,
    VisualforcePageRef,
};
use crate::schema_index::{relationship_field_api_name, SchemaIndex};
use crate::scope::{ScopeId, ScopeKind, ScopeTree};
use crate::stdlib_index::StdlibIndex;
use apex_stdlib::{StdlibClass, StdlibMethod};
use crate::symbol::{ModifierSet, Symbol, SymbolId, SymbolKind};
use crate::symbol_table::SymbolTable;
use crate::ty::Ty;
use apex_syntax::ast::decl::{TriggerBlock, VarDeclarator};
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
/// compile error, an unmodeled overload, or a class extending
/// `Exception` -- Apex implicitly synthesizes four constructors for
/// every such class regardless of what it declares itself, which this
/// binder doesn't model -- see `BACKLOG.md`), the original full
/// candidate set is reported as `Candidates`, **never** `Resolved`, even
/// when it happens to contain exactly one same-named symbol: a lone
/// candidate with the wrong arity still isn't the one actually being
/// called, so confidently resolving to it would be a wrong answer, not
/// just an imprecise one. Confirmed a real bug via
/// `crates/apex-binder/tests/resolution_consistency.rs`'s whole-corpus
/// arity/name self-check before this fix: `pool.len() == 1` used to fire
/// on this exact fallback case too, since it didn't distinguish "the one
/// candidate genuinely matches this call's arity" from "there's only one
/// same-named candidate at all, and it doesn't."
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
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
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
    if by_arity.is_empty() {
        return Resolution::Candidates(candidates);
    }
    let pool = by_arity;
    if pool.len() == 1 {
        return Resolution::Resolved(pool[0]);
    }

    let by_type: Vec<SymbolId> = pool
        .iter()
        .copied()
        .filter(|&id| is_argument_type_compatible(schema, stdlib, table, id, arg_types))
        .collect();
    match by_type.len() {
        1 => Resolution::Resolved(by_type[0]),
        // Over-eliminated (every candidate ruled itself out, which given
        // the conservative rule above should only happen if `pool`
        // itself was already empty -- defensive, not expected) or still
        // ambiguous: report the honest pre-type-filter pool either way.
        0 => Resolution::Candidates(pool),
        _ => match most_specific_candidate(schema, table, &by_type) {
            Some(winner) => Resolution::Resolved(winner),
            None => Resolution::Candidates(by_type),
        },
    }
}

/// A `Resolution::StdlibMember` reference for `class_name.member` (or
/// just `class_name` itself, when `member` is `None` -- a bare class
/// name used as a static-call receiver). `namespace` is the real (post-
/// collision-tiebreak) namespace of `class_name`'s `StdlibClass`, which
/// every call site already has in hand from its own `stdlib.class(...)`
/// lookup (needed there to decide the `Resolution` in the first place),
/// so this takes it directly instead of re-deriving it by name -- shared
/// by every `Ty::System` arm that can produce this resolution.
fn stdlib_member_ref(
    namespace: Option<SmolStr>,
    class_name: &str,
    member: Option<&str>,
    arg_count: Option<usize>,
    narrowed_param_types: Option<Vec<SmolStr>>,
) -> StdlibMemberRef {
    StdlibMemberRef {
        namespace,
        class_name: SmolStr::new(class_name),
        member: member.map(SmolStr::new),
        arg_count,
        narrowed_param_types,
    }
}

/// The real, documented standard-library class `container` (or the
/// nearest ancestor of it, walking `SymbolTable::inherited_chain` the
/// same outward order `SymbolTable::lookup_member` already does) `extends`
/// but could never resolve as a project type at all -- see
/// `SymbolTable::unresolved_direct_super`'s own doc comment for why that
/// name is kept around rather than discarded once Pass 1.5 is done with
/// it. `None` when no ancestor in the chain has an unresolved `extends`
/// name, or when the one it does have doesn't match any real bundled
/// stdlib class (a genuinely unmodeled/nonexistent base, indistinguishable
/// from a typo without more information than this project has).
fn stdlib_class_via_unresolved_supertype(
    table: &SymbolTable,
    stdlib: &StdlibIndex,
    container: SymbolId,
) -> Option<&'static StdlibClass> {
    std::iter::once(container)
        .chain(table.inherited_chain(container).iter().copied())
        .find_map(|id| table.unresolved_direct_super(id))
        .and_then(|name| stdlib.class(name))
}

/// Best-effort narrows `class.member`'s scraped overloads (arity, then
/// `crate::conversions::type_compatible`) down to the one real method a
/// call actually invokes -- deliberately simpler than [`narrow_by_overload`]'s
/// full three-stage algorithm, since a stdlib reference's `Resolution`
/// (`StdlibMember` vs. `Unresolved`) is already decided by mere
/// name-existence before this ever runs; getting the exact overload
/// right only affects how far a chained call keeps resolving type-wise
/// (`narrow_stdlib_overload_type`, below) and which one signature/
/// description a hover shows (`bind_method_call_expr`'s own
/// `StdlibMemberRef.narrowed_param_types`) -- never whether the call
/// itself resolves at all. Ambiguous (0 or 2+ survivors) -> `None`, same
/// as the total absence of this information today -- never a guessed,
/// possibly-wrong winner. Takes an already-looked-up `class` (every
/// caller needs it anyway, to decide the call's own `Resolution`) rather
/// than a `stdlib`/`class_name` pair, and only collects the overloads
/// into a `Vec` once it's confirmed there's more than one -- `member`
/// overwhelmingly has just a single overload, and that common case
/// returns without allocating at all.
fn narrow_stdlib_overload(
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    table: &SymbolTable,
    class: &'static StdlibClass,
    member: &str,
    arg_types: &[Option<Ty>],
) -> Option<&'static StdlibMethod> {
    let mut overloads = StdlibIndex::methods_of(class, member).peekable();
    let first = overloads.next()?;
    if overloads.peek().is_none() {
        return Some(first);
    }
    let overloads: Vec<&StdlibMethod> = std::iter::once(first).chain(overloads).collect();
    let by_arity: Vec<&StdlibMethod> = overloads
        .iter()
        .copied()
        .filter(|m| m.params.len() == arg_types.len())
        .collect();
    let pool: &[&StdlibMethod] = if by_arity.is_empty() { &overloads } else { &by_arity };

    match pool {
        [one] => Some(*one),
        _ => {
            let by_type: Vec<&StdlibMethod> = pool
                .iter()
                .copied()
                .filter(|m| stdlib_args_compatible(schema, stdlib, table, m, arg_types))
                .collect();
            match by_type.as_slice() {
                [one] => Some(*one),
                _ => None,
            }
        }
    }
}

/// Turns a scraped type string (`StdlibMethod::return_type`,
/// `StdlibProperty::type_name`, ...) into a `Ty::System`, stripping a
/// leading `"Namespace."` prefix down to the class's own bare name first
/// when `stdlib` confirms that's a real (namespace, class) pair.
/// `StdlibIndex`'s registry (like every other class lookup in this crate)
/// is keyed by a class's bare name only, never a namespace-qualified
/// compound string -- a return type scraped as `"Schema.DescribeFieldResult"`
/// (real, confirmed: `SObjectField.getDescribe()`'s own scraped return
/// type is exactly this shape) used to produce a `Ty::System` named
/// literally `"Schema.DescribeFieldResult"`, which no later
/// `stdlib.class(&name)` lookup could ever match -- silently dead-ending
/// any further chained call right after it (`token.getDescribe().getName()`,
/// a real bug this fixed). Left as the whole original string when it
/// isn't a recognized two-segment namespace+class pair (a generic
/// argument, a name that merely happens to contain a dot, ...) -- "can't
/// prove it's namespace-qualified" keeps today's behavior, never a guess.
fn ty_from_scraped_type(stdlib: &StdlibIndex, type_str: &str) -> Ty {
    let (base, args) = apex_stdlib::split_generic_type(type_str);
    let base = match base.rsplit_once('.') {
        Some((ns, name)) if stdlib.class_in_namespace(ns, name).is_some() => SmolStr::new(name),
        _ => base,
    };
    Ty::system_owned(base, args.into_iter().map(|a| Ty::system_owned(a, Vec::new())).collect())
}

/// The real Apex type a schema field's *value* has, from its own
/// metadata `field_type` (Salesforce's `<type>` element text --
/// `"Picklist"`, `"Currency"`, `"DateTime"`, ... -- see
/// `apex_metadata::FieldSchema::field_type`'s own doc comment), for a
/// non-relationship field accessed outside SOQL (`opp.Name`, `opp.Type`
/// -- a `reference_to`-bearing field already gets its own `Ty` a
/// different way, via the field's real target object, not this).
/// Without this, *every* scalar field access had no inferred type at
/// all, so a chained call on it (`opp.Name.equals(...)`, `opp.Type.startsWith(...)`,
/// both real fflib_SObjectDomain.cls shapes) stayed `Unresolved`
/// unconditionally -- an extremely common real Apex pattern, not a rare
/// edge case. Deliberately conservative: the bundled standard-object
/// snapshot's own `field_type` strings are scraped prose, not a clean
/// enum (confirmed by direct inspection -- alongside clean values like
/// `"picklist"`/`"currency"` there's real noise like `"ManageableState
/// enumerated list"`/`"reference to a WorkGoal object"`), so an
/// unrecognized value stays honestly unknown (`None`) rather than
/// guessed. The recognized mappings themselves are verified against a
/// real org (`sf apex run`), not assumed from memory.
fn apex_type_for_schema_field_type(field_type: &str) -> Option<&'static str> {
    match field_type.trim().to_ascii_lowercase().as_str() {
        "string" | "text" | "textarea" | "picklist" | "multipicklist" | "combobox" | "email" | "phone" | "url"
        | "encryptedstring" | "base64" => Some("String"),
        "boolean" | "check" => Some("Boolean"),
        "date" => Some("Date"),
        "datetime" => Some("Datetime"),
        "time" | "timeonly" => Some("Time"),
        "currency" | "percent" | "double" | "number" => Some("Decimal"),
        "int" | "integer" => Some("Integer"),
        "long" => Some("Long"),
        "id" => Some("Id"),
        _ => None,
    }
}

/// A [`narrow_stdlib_overload`] winner's own declared return type,
/// converted to the *propagated type* for continued chaining
/// (`Database.query(soql).size()`).
fn stdlib_method_return_ty(stdlib: &StdlibIndex, method: &StdlibMethod) -> Option<Ty> {
    let return_type = method.return_type.as_deref()?;
    Some(ty_from_scraped_type(stdlib, return_type))
}

/// Mirrors [`is_argument_type_compatible`], but against a scraped
/// [`StdlibMethod`]'s string-typed params instead of a `SymbolId`'s
/// declared ones -- an argument or parameter type this crate can't even
/// name never eliminates (same "can't prove wrong beats assume wrong"
/// rule `crate::conversions::type_compatible` itself already applies).
fn stdlib_args_compatible(
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    table: &SymbolTable,
    method: &StdlibMethod,
    arg_types: &[Option<Ty>],
) -> bool {
    for (param, arg_type) in method.params.iter().zip(arg_types.iter()) {
        let (Some(param_type), Some(arg_type)) = (&param.type_name, arg_type) else {
            continue;
        };
        let (param_name, param_args) = apex_stdlib::split_generic_type(param_type);
        if conversions::type_compatible(schema, stdlib, table, &param_name, &param_args, arg_type)
            == Some(false)
        {
            return false;
        }
    }
    true
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
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
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
        if conversions::type_compatible(
            schema,
            stdlib,
            table,
            param_type_name,
            &param_symbol.type_args,
            arg_type,
        ) == Some(false)
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
fn most_specific_candidate(
    schema: &SchemaIndex,
    table: &SymbolTable,
    candidates: &[SymbolId],
) -> Option<SymbolId> {
    candidates
        .iter()
        .copied()
        .find(|&c| {
            candidates
                .iter()
                .all(|&d| d == c || dominates(schema, table, c, d))
        })
}

/// Whether `a`'s declared parameter list is at least as specific as `b`'s
/// in every position, with a strict improvement in at least one --
/// compares only the two candidates' own declared signatures, independent
/// of the actual call's arguments (both are already known-applicable).
fn dominates(schema: &SchemaIndex, table: &SymbolTable, a: SymbolId, b: SymbolId) -> bool {
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
        if conversions::is_more_specific(
            schema,
            table,
            a_name,
            &a_sym.type_args,
            b_name,
            &b_sym.type_args,
        ) {
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

/// Recognizes `arg` as an SObject constructor field-init pair (`field =
/// value`, e.g. `Primary_Affiliation__c = acc.id` inside `new
/// Contact(...)`) -- `Some((field_name, value))` only for a plain `=`
/// (never a compound assignment like `+=`, which Apex's own constructor
/// sugar doesn't accept here anyway) with a bare identifier LHS, `None`
/// for anything else (an ordinary positional argument, or any other
/// shape `arg_list`'s generic grammar happens to also accept there --
/// see `crate::resolve::BodyBinder::bind_new_expr`'s own doc comment on
/// why the grammar can't tell these apart itself). A bare identifier is
/// the only LHS shape Apex's real syntax allows here -- a dotted
/// relationship path (`Account.Name = ...`) is not valid inside this
/// constructor sugar -- so `Expr::Name` is the only case worth matching.
fn sobject_field_init(arg: &Expr) -> Option<(NameExpr, Expr)> {
    let Expr::Bin(b) = arg else { return None };
    let op_text: String = b.operator_tokens().iter().map(|t| t.text()).collect();
    if op_text != "=" {
        return None;
    }
    let Expr::Name(field_name) = b.lhs()? else {
        return None;
    };
    Some((field_name, b.rhs()?))
}

/// Cheap, non-allocating guard against treating an unrelated string's
/// incidental `:word` pattern (a URL like `'http://host:8080'`, or the
/// name of a config key) as a real dynamic-SOQL bind: only a string that
/// itself looks like a SOQL query is ever scanned for binds at all. Real
/// SOQL always contains `SELECT` (case-insensitive; Apex is
/// case-insensitive for keywords) -- `Database.query`/`countQuery`/
/// `getQueryLocator` are exclusively SOQL, never SOSL, so `FIND` doesn't
/// need checking here.
fn looks_like_soql(text: &str) -> bool {
    text.as_bytes()
        .windows(6)
        .any(|w| w.eq_ignore_ascii_case(b"select"))
}

/// Every `:identifier` bind-variable occurrence inside `token`'s own raw
/// text (quotes and all), as `(name, absolute_text_range)` pairs. Scans
/// the raw token text directly rather than unescaping it first -- Apex
/// string escapes can never produce a literal `:` or identifier
/// character, so there's nothing an escape sequence could hide here.
/// Deliberately matches only a bare identifier (ASCII letters/digits/
/// underscore, not starting with a digit) immediately after `:`: a
/// dotted bind expression (`:obj.field`) is legal grammar for *inline*
/// SOQL's own `SoqlBoundExpr` (`crate::soql::bind_bound_expr`, which
/// binds a real parsed `Expr`), but dynamic SOQL's string-embedded form
/// has no such node to parse -- only the leading identifier segment
/// could ever match a real local/parameter name in scope, so that's all
/// this looks for.
fn find_bind_vars(token: &apex_syntax::SyntaxToken) -> Vec<(&str, rowan::TextRange)> {
    let text = token.text();
    let bytes = text.as_bytes();
    let base = token.text_range().start();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end > start && !bytes[start].is_ascii_digit() {
            let range = rowan::TextRange::new(
                base + rowan::TextSize::from(start as u32),
                base + rowan::TextSize::from(end as u32),
            );
            result.push((&text[start..end], range));
        }
        i = end.max(start);
    }
    result
}

/// Finds the nearest earlier statement, in the same straight-line
/// sequence of enclosing blocks as `anchor` (never a sibling `if`/`else`
/// branch, a loop body, or another method entirely -- deliberately a
/// bounded, branch-free approximation, not a real control-flow
/// analysis), that declares-with-initializer or plainly reassigns
/// (`name = expr;`, never a compound assignment like `+=`, which would
/// need the *prior* value to evaluate and so can't be treated as a fresh
/// source) a local named `name`. Returns that statement's initializer/
/// RHS expression -- the most recent one found, since scanning proceeds
/// forward through each block (oldest to newest) and keeps overwriting
/// the candidate, so whatever's left standing after a block is legally
/// its last write before `anchor`.
fn last_assignment_to_local(anchor: &apex_syntax::SyntaxNode, name: &str) -> Option<Expr> {
    let mut boundary = anchor.clone();
    loop {
        let block = boundary.parent()?.ancestors().find_map(Block::cast)?;
        let boundary_start = boundary.text_range().start();
        let mut candidate = None;
        for stmt in block.statements() {
            if stmt.syntax().text_range().start() >= boundary_start {
                break;
            }
            match &stmt {
                Stmt::LocalVarDecl(decl) => {
                    for d in decl.declarators() {
                        if d.name().and_then(|n| n.text()).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
                            candidate = d.init();
                        }
                    }
                }
                Stmt::Expr(e) => {
                    if let Some(Expr::Bin(b)) = e.expr() {
                        let op: String = b.operator_tokens().iter().map(|t| t.text()).collect();
                        if op == "=" {
                            if let Some(Expr::Name(n)) = b.lhs() {
                                if n.name_token().is_some_and(|t| t.text().eq_ignore_ascii_case(name)) {
                                    candidate = b.rhs();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(c) = candidate {
            return Some(c);
        }
        boundary = block.syntax().clone();
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
    pub(crate) stdlib: &'a StdlibIndex,
    pub(crate) labels: &'a LabelIndex,
    pub(crate) pages: &'a PageIndex,
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
    stdlib: &StdlibIndex,
    labels: &LabelIndex,
    pages: &PageIndex,
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
        stdlib,
        labels,
        pages,
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
#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
pub(crate) fn bind_trigger_body(
    table: &SymbolTable,
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    labels: &LabelIndex,
    pages: &PageIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    block: &TriggerBlock,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, block.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        stdlib,
        labels,
        pages,
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
#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
pub(crate) fn bind_initializer(
    table: &SymbolTable,
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    labels: &LabelIndex,
    pages: &PageIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    expr: &Expr,
) -> BoundBody {
    let (scopes, root_scope) = ScopeTree::new_root(ScopeKind::Body, expr.syntax().text_range());
    let mut binder = BodyBinder {
        table,
        schema,
        stdlib,
        labels,
        pages,
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

/// Builds a throwaway `BodyBinder` for `apex_binder::completion`'s
/// receiver-type inference -- given a real, already-bound body's `scope`
/// tree, lets `crate::completion` call the existing `bind_expr` on just a
/// receiver subexpression (e.g. `foo` in `foo.|`) to get its `Ty`, reusing
/// every existing chain-typing rule (arbitrary depth, every `Expr`
/// variant, `this`/`super`, stdlib/schema fallthrough) instead of
/// duplicating any of it.
///
/// Safe to discard afterward without merging anything back: `bind_expr`
/// never mutates `scopes` (only statement-level `bind_stmt`, never reached
/// from a bare `bind_expr` call on an already-existing expression, does
/// that), and the fresh `ReferenceTable` this constructs is never anything
/// but a scratch sink for `bind_expr`'s side-effecting `refs.set(...)`
/// calls -- it's simply dropped, the same way every other `BodyBinder`
/// above is already built fresh-and-discarded (or fresh-and-merged) per
/// call, never shared or mutated across calls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn body_binder_for_completion<'a>(
    table: &'a SymbolTable,
    schema: &'a SchemaIndex,
    stdlib: &'a StdlibIndex,
    labels: &'a LabelIndex,
    pages: &'a PageIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    enclosing_member: Option<SymbolId>,
    scopes: ScopeTree,
) -> BodyBinder<'a> {
    BodyBinder {
        table,
        schema,
        stdlib,
        labels,
        pages,
        refs: ReferenceTable::default(),
        scopes,
        pending_locals: Vec::new(),
        file,
        enclosing_type,
        enclosing_member,
    }
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
    stdlib: &StdlibIndex,
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
        record_qualified_segments(table, stdlib, refs, file, &segments);
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
                .filter_map(|arg| resolve_type_ref(table, schema, stdlib, refs, file, enclosing_type, &arg))
                .collect()
        })
        .unwrap_or_default();
    // A stdlib class referenced with its own namespace spelled out
    // (`Schema.SObjectField token;`, `System.String s;`) -- real, legal
    // Apex, and not covered by the plain `stdlib.class(&name)` lookup
    // below, which only ever matches a *bare* class name: `name` here is
    // the whole dotted path (`"Schema.SObjectField"`), which is never a
    // key in `StdlibIndex`'s by-bare-name map, so a namespace-qualified
    // declared type fell all the way through to `Unresolved` -- and with
    // it, every member access on a variable declared with one (e.g.
    // `token.getDescribe()` below), since the fallback `Ty::system_owned`
    // then carried the whole dotted string as its "class name" instead
    // of the real bare one. Only tried for exactly two segments: a
    // deeper qualified path (`Foo.Bar.Baz`) isn't a real namespace-
    // qualified stdlib shape.
    if segments.len() == 2 {
        if let Some(class) = stdlib.class_in_namespace(segments[0].text(), segments[1].text()) {
            refs.set(
                ptr,
                Resolution::StdlibMember(Box::new(stdlib_member_ref(
                    class.namespace.clone(),
                    &class.name,
                    None,
                    None,
                    None,
                ))),
            );
            return Some(Ty::system_owned(class.name.clone(), args));
        }
    }
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
    // A real, documented stdlib class (`String`, `List`, `Database`, ...)
    // used as a declared type (`String s;`, `List<Contact>`, a parameter/
    // return type) -- mirrors `bind_name_expr`'s identical bare-class-as-
    // value fallback (the `member: None` shape `stdlib_member_ref`'s own
    // doc comment already covers) for the *expression*-position case,
    // which this one was missing entirely: before this, `String` in
    // `String.isBlank(...)` resolved as `StdlibMember`, but the exact
    // same name in `String s;` fell all the way through to `Unresolved`
    // -- confirmed a real, large-volume gap via a whole-corpus
    // `Unresolved`-clustering sweep (`examples/unresolved_clusters.rs`),
    // not a rare edge case.
    if let Some(class) = stdlib.class(&name) {
        refs.set(
            ptr,
            Resolution::StdlibMember(Box::new(stdlib_member_ref(
                class.namespace.clone(),
                &name,
                None,
                None,
                None,
            ))),
        );
        return Some(Ty::system_owned(name, args));
    }
    // Could be a standard object this repo never locally extended
    // (indistinguishable, using `apex-metadata` alone, from a genuinely
    // nonexistent name), or an unmodeled system/library type (an
    // `Exception` subtype -- the scraped stdlib snapshot has no entry for
    // `Exception`/`DmlException`/etc. at all, since the real Apex
    // Reference Guide only documents them on grouped, empty-methods
    // "Built-In Exceptions"-style pages `apex_stdlib::standard_classes`
    // already filters out -- or any other genuinely unmodeled name)  --
    // v1 can't tell these apart, so both land here as `Unresolved` rather
    // than one of them being misreported. The name is still real, though,
    // so the returned `Ty` keeps it.
    refs.set(ptr, Resolution::Unresolved);
    Some(Ty::system_owned(name, args))
}

/// A field/property named identically to a *nested type* also declared
/// in the same enclosing class is a real, idiomatic Apex pattern (NPSP
/// shape: `fflib_SObjectDomain`'s `Configuration Configuration { get;
/// private set; }`) -- `SymbolTable::lookup_member` finds both (nested
/// types are indexed as members alongside fields/properties, with no
/// kind partitioning), which would otherwise land in the `Candidates`
/// arm and give up before ever inferring a type to chain further access
/// off of. In expression position the *value* always wins over the
/// same-named type -- the same "instance member shadows a type name"
/// precedence `static_member_access.rs` already documents for the
/// separate top-level-type fallback, extended here to when both
/// candidates come from member lookup itself. Only actually drops
/// anything when a non-type alternative exists; a lone nested-type match
/// (a bare reference to a sibling nested type) is untouched. Shared by
/// `bind_name_expr` (a *bare* reference, `Configuration` from within
/// `fflib_SObjectDomain` itself) and `bind_field_expr` (a *qualified*
/// one, `domainObject.Configuration`) -- the first is where this was
/// originally fixed; the second had the identical gap, found only later
/// via a real `domainObject.Configuration.TriggerStateEnabled` chain
/// that needed the *value*'s own type to keep resolving further.
fn prefer_value_over_same_named_type(table: &SymbolTable, members: Vec<SymbolId>) -> Vec<SymbolId> {
    if members.len() <= 1 {
        return members;
    }
    let non_type: Vec<SymbolId> = members
        .iter()
        .copied()
        .filter(|&id| {
            !matches!(
                table.get(id).kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
            )
        })
        .collect();
    if non_type.is_empty() {
        members
    } else {
        non_type
    }
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
    stdlib: &StdlibIndex,
    refs: &mut ReferenceTable,
    file: FileId,
    segments: &[apex_syntax::SyntaxToken],
) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    let current = table.top_level(first.text());
    // `first` alone might be a real stdlib class in its own right, not
    // just a namespace prefix -- `Schema`/`System`/... are simultaneously
    // both (`Schema.getGlobalDescribe()` is a real static call on the
    // `Schema` class itself), so `Schema.SObjectType`'s own first segment
    // deserves the same StdlibMember treatment `resolve_type_ref`'s
    // whole-node single-segment lookup (`stdlib.class(&name)`) already
    // gives a bare `Schema` reference elsewhere.
    let first_resolution = match current {
        Some(id) => Resolution::Resolved(id),
        None => match stdlib.class(first.text()) {
            Some(class) => Resolution::StdlibMember(Box::new(stdlib_member_ref(
                class.namespace.clone(),
                &class.name,
                None,
                None,
                None,
            ))),
            None => Resolution::Unresolved,
        },
    };
    refs.set(SyntaxPtr::for_token(file, first), first_resolution);
    let Some((second, deeper)) = rest.split_first() else {
        return;
    };
    let Some(current) = current else {
        // `first` isn't a project type -- it might still be a real
        // namespace (`Schema`, `System`, ...) qualifying a real stdlib
        // class one segment later (`Schema.SObjectType`). Only tried
        // here, for the segment immediately after `first`: real Apex
        // namespace-qualified references are always exactly
        // `Namespace.Class`, never deeper through a bare namespace
        // prefix, so a further segment (`Schema.SObjectType.Whatever`,
        // not a real shape in practice) has no model to fall back to and
        // stays honestly `Unresolved`, same as before this fallback
        // existed. Mirrors `resolve::bind_field_expr`'s identical
        // `class_in_namespace` fallback for this same shape used in
        // *expression* position (`Schema.SoapType.ID`) -- this is the
        // *type*-position (`Schema.SObjectType token;`) counterpart, a
        // real, common gap in a real fflib_QueryFactory.cls: the *whole*
        // `Schema.SObjectType` reference already resolved fine via
        // `resolve_type_ref`'s own two-segment fallback further down --
        // only *this* per-segment hover/goto-definition/diagnostic
        // record never learned the same trick.
        let resolution = match stdlib.class_in_namespace(first.text(), second.text()) {
            Some(class) => Resolution::StdlibMember(Box::new(stdlib_member_ref(
                class.namespace.clone(),
                &class.name,
                None,
                None,
                None,
            ))),
            None => Resolution::Unresolved,
        };
        refs.set(SyntaxPtr::for_token(file, second), resolution);
        for seg in deeper {
            refs.set(SyntaxPtr::for_token(file, seg), Resolution::Unresolved);
        }
        return;
    };
    let mut current = Some(current);
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
    stdlib: &StdlibIndex,
    file: FileId,
    enclosing_type: Option<SymbolId>,
    ty: &Type,
) -> BoundBody {
    let mut refs = ReferenceTable::default();
    resolve_type_ref(table, schema, stdlib, &mut refs, file, enclosing_type, ty);
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
        self.declare_local_raw(kind, name, type_ptr, type_name, type_args)
    }

    /// Like [`Self::declare_local`], but for a caller with a plain type-
    /// name *string* on hand instead of a real `Type` AST node --
    /// specifically, a catch clause's own exception name, parsed as a
    /// `QualifiedName` (per real Apex grammar: `catch (Type e)`'s `Type`
    /// production is a bare `qualifiedName`, not a full `typeRef`, so
    /// there's no `Type` node to reuse [`Self::declare_local`] with at
    /// all -- see `resolve_type_ref`'s own doc comment on
    /// `QualifiedName`'s other two uses, `whenValue`/`upsert`, for the
    /// same grammar distinction). Before this, a catch variable's own
    /// declared type was silently discarded entirely (`declare_local`
    /// called with `None`), so `e.getMessage()` inside *any* catch block
    /// stayed `Unresolved` regardless of whether the exception type
    /// itself was ever modeled -- real, common Apex, not an edge case.
    /// No `type_ref`/`type_args` to carry (a caught exception type is
    /// never generic), so those stay empty; only `type_name` feeds
    /// `type_of_symbol`'s existing `resolve_dotted_name`/`stdlib.class`
    /// lookup, the same one the catch clause's own exception-type
    /// reference already resolves through.
    fn declare_local_with_type_name(
        &mut self,
        kind: SymbolKind,
        name: &Name,
        type_name: Option<SmolStr>,
    ) -> SymbolId {
        self.declare_local_raw(kind, name, None, type_name, Vec::new())
    }

    fn declare_local_raw(
        &mut self,
        kind: SymbolKind,
        name: &Name,
        type_ptr: Option<AstPtr<Type>>,
        type_name: Option<SmolStr>,
        type_args: Vec<SmolStr>,
    ) -> SymbolId {
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
        let args: Vec<Ty> = symbol
            .type_args
            .iter()
            .map(|name| match self.table.resolve_dotted_name(name) {
                Some(id) => Ty::Project(id),
                // A namespace-qualified stdlib type as a *generic type
                // argument* (`Map<System.Type, System.Type> bindings;`) --
                // the exact same gap the field's own outer `type_name`
                // gets the `class_in_namespace` fallback for just below,
                // but never mirrored here for each *argument* name
                // independently. Without this, `bindings`'s own `Ty`
                // carried `"System.Type"` (the whole dotted string,
                // unsplit) as an argument's name -- never a key in
                // `StdlibIndex`'s by-bare-name map -- so every further
                // hop off a `.get(...)`-substituted argument type (real
                // NPSP shape: `this.bindings.get(interfaceType).newInstance()`)
                // stayed `Unresolved`, even though the field's own
                // top-level `Map` type resolved fine.
                None => match name.split_once('.') {
                    Some((namespace, class_name)) if !class_name.contains('.') => {
                        match self.stdlib.class_in_namespace(namespace, class_name) {
                            Some(class) => Ty::system_owned(class.name.clone(), Vec::new()),
                            None => Ty::system_owned(name.clone(), Vec::new()),
                        }
                    }
                    _ => Ty::system_owned(name.clone(), Vec::new()),
                },
            })
            .collect();
        // A stdlib class referenced with its own namespace spelled out as
        // the declared type (`Schema.SObjectField token;`) -- the same
        // gap `resolve_type_ref` has for the `Type` AST node itself (see
        // its own matching comment), but hit independently here: this
        // function resolves a *reference* to an already-declared symbol
        // from its stored `type_name` string, a wholly separate path
        // that never consulted `StdlibIndex` by namespace at all. Without
        // this, `token`'s own declared-type *annotation* could resolve
        // fine while every later use of `token` (`token.getDescribe()`)
        // still built a `Ty::System` carrying the literal dotted string
        // `"Schema.SObjectField"` as its name -- never a key in
        // `StdlibIndex`'s by-bare-name map -- so the method call itself
        // stayed `Unresolved`. Only tried for exactly two segments, same
        // restriction as `resolve_type_ref`.
        if let Some((namespace, class_name)) = type_name.split_once('.') {
            if !class_name.contains('.') {
                if let Some(class) = self.stdlib.class_in_namespace(namespace, class_name) {
                    return Some(Ty::system_owned(class.name.clone(), args));
                }
                // Not a *documented* class in this namespace either (a
                // built-in exception subtype like `DmlException`, most
                // commonly -- see `bind_method_call_expr`'s own matching
                // fallback) -- but the shape is still unambiguous
                // (`Namespace.Class`), and every `Ty::System` name
                // elsewhere in this crate is bare, never namespace-
                // prefixed, so falling through to the *whole* dotted
                // string below would only ever produce a name no later
                // `stdlib.class`/`self.schema.object` lookup could ever
                // match -- strictly worse than the bare class name, never
                // better. Real NPSP shape: `System.DmlException caughtEx
                // = null;` -- stripping the namespace here is what lets
                // `caughtEx.getMessage()` reach `bind_method_call_expr`'s
                // own `base.ends_with("Exception")` fallback at all.
                return Some(Ty::system_owned(SmolStr::new(class_name), args));
            }
        }
        Some(Ty::system_owned(type_name.to_string(), args))
    }

    /// The propagated `Ty` for a method/constructor-call `Resolution`:
    /// exact for `Resolved` (`type_of_symbol` of the one candidate), and
    /// also recoverable for `Candidates` when every surviving candidate
    /// happens to declare the *identical* return type -- which chaining
    /// off an overloaded or dynamically-dispatched call very often does
    /// (a fluent builder's `withX(...)` overloads all returning the same
    /// class, or a virtual method and its override sharing an identical
    /// return type by construction). Without this, resolving to
    /// `Candidates` -- itself often unavoidable, e.g.
    /// `crate::conversions`'s curated type model has no rule for an
    /// unmodeled system parameter type like `Schema.FieldSetMember`, so a
    /// real, unambiguous call can still end up `Candidates` rather than
    /// `Resolved` -- silently dropped the call's type entirely, breaking
    /// resolution for *every* subsequent `.member`/`.method()` chained
    /// onto it, even though the type itself was never actually in doubt.
    /// Real NPSP shape this fixed: `UTIL_Finder`'s fluent
    /// `.withSelectFields(...)` (three overloads, all returning
    /// `UTIL_Finder`) chained into `.withSearchQuery(...)` -- the
    /// ambiguous `withSelectFields` overload pick (`List<FieldSetMember>`
    /// can't be positively ruled out for a `List<String>` argument,
    /// `FieldSetMember` being outside `crate::conversions`'s curated set)
    /// used to blank out the whole rest of the chain, permanently hiding
    /// every real call to `withSearchQuery` and flagging it dead. `None`
    /// whenever candidates disagree (or any one of them can't be typed at
    /// all) -- "can't prove a shared type" must never guess one.
    fn result_type_of(&self, resolution: &Resolution) -> Option<Ty> {
        match resolution {
            Resolution::Resolved(id) => self.type_of_symbol(*id),
            Resolution::Candidates(ids) => {
                let mut ids = ids.iter();
                let first = self.type_of_symbol(*ids.next()?)?;
                for &id in ids {
                    if self.type_of_symbol(id).as_ref() != Some(&first) {
                        return None;
                    }
                }
                Some(first)
            }
            _ => None,
        }
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
            self.stdlib,
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
                    // The exception type name, trimmed -- also fed to the
                    // catch variable's own declared type below, kept
                    // outside the reference-resolution block so both uses
                    // share the one trim instead of recomputing it.
                    let exception_type_name = catch.exception_type().map(|ty| {
                        let ptr = SyntaxPtr::new(self.file, ty.syntax());
                        // `QualifiedName` (not `Type`) -- resolve by its
                        // whole-text base name against project types
                        // only; unlike `resolve_type_ref`, exception
                        // types are never SObject-shaped, so there's no
                        // schema fallback to attempt here. (The catch
                        // *variable*'s own type, set below via
                        // `declare_local_with_type_name`, still gets the
                        // fuller `type_of_symbol` resolution -- including
                        // its `stdlib.class` fallback -- when something
                        // later in the block actually uses `e`; this
                        // reference alone deliberately stays project-only,
                        // the "structural" classification
                        // `capabilities::classify_unresolved` documents
                        // for exactly this shape.)
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
                        let name = SmolStr::new(ty.syntax().text().to_string().trim());
                        match self.table.resolve_dotted_name(&name) {
                            Some(id) => self.refs.set(ptr, Resolution::Resolved(id)),
                            None => self.refs.set(ptr, Resolution::Unresolved),
                        }
                        name
                    });
                    let child = self.scopes.push(
                        Some(scope),
                        ScopeKind::Catch,
                        catch.syntax().text_range(),
                    );
                    if let Some(name) = catch.name() {
                        // Real Apex has no generic exception type, so
                        // `declare_local_with_type_name` (no `type_args`)
                        // is exact here, not a simplification -- see its
                        // own doc comment for why this can't just reuse
                        // `declare_local` the way every other local
                        // declaration does.
                        let sym = self.declare_local_with_type_name(
                            SymbolKind::CatchVar,
                            &name,
                            exception_type_name,
                        );
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
                // When both branches are known but differ, `conversions::widen`
                // computes their real common type (verified against a real
                // org -- see its own doc comment); only one side known
                // still uses that side's type unchanged, never a guess.
                match (then_ty, else_ty) {
                    (Some(a), Some(b)) => conversions::widen(self.schema, self.stdlib, self.table, &a, &b),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
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
                let target_ty = idx.target().and_then(|t| self.bind_expr(scope, &t));
                let has_real_index = idx.index().is_some();
                if let Some(i) = idx.index() {
                    self.bind_expr(scope, &i);
                }
                // The empty-`[]` array-type-suffix form of the
                // `Foo[].class` reflection idiom (`IndexExpr::index`'s own
                // doc comment) is a type marker, not a real index -- never
                // worth an element type. A real index (`list[0]`) on a
                // `List<T>`/`T[]` target does propagate `T`, the same
                // substitution `crate::generics::builtin_generic_member_type`'s
                // `"list"` -> `"get"` arm already gives `list.get(0)` --
                // `[...]` is just `.get(...)`'s own syntax sugar, so it
                // deserves the identical result type. Real Apex has no
                // `map[key]` bracket syntax at all (`Map` only ever
                // indexes via `.get(...)`), so this never needs a `Map`
                // arm the way the method-call table does.
                match target_ty {
                    Some(Ty::System { name, args }) if has_real_index && name.eq_ignore_ascii_case("List") => {
                        args.into_iter().next()
                    }
                    _ => None,
                }
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
            let members = prefer_value_over_same_named_type(self.table, members);
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
        // Or a real stdlib class used as a static-call receiver
        // (`String.isBlank(...)`, `Database.query(...)`, `Math.max(...)`)
        // -- without this, `String` itself bound to `None` here (an
        // unrecognized bare name, no SObject match either), so the call
        // hanging off it never even reached the `Ty::System` arms below
        // that actually know how to look a stdlib member up; every
        // static stdlib call stayed `Unresolved` regardless of how real
        // the method name was. A stdlib class used only as a *type*
        // (`String s;`) already worked before this -- `resolve_type_ref`
        // returns `Ty::system_owned` unconditionally for any unresolved
        // name, this is specifically the *expression*-position gap.
        if let Some(class) = self.stdlib.class(name) {
            self.refs.set(
                ptr,
                Resolution::StdlibMember(Box::new(stdlib_member_ref(
                    class.namespace.clone(),
                    name,
                    None,
                    None,
                    None,
                ))),
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

        // The `X.class` reflection idiom (`fflib_IAppBinding.class`,
        // `String.class`, ...) -- valid on any project or stdlib type
        // name and not a real member of anything (`class` is a reserved
        // word, never a real field/method name, so this can't collide
        // with a genuine one). `List<Foo>.class`/`Map<K, V>.class` never
        // reach here: `bind_name_expr`'s own `n.type_ref()` check
        // intercepts the whole expression first for that generic-
        // collection form (see its doc comment) -- this only needs to
        // cover the plain-name case that doesn't go through. Resolves to
        // the real `System.Type` class, the same `Ty` a `System.Type`-
        // declared variable already carries, so a further chained call
        // (`fflib_IAppBinding.class.getName()`) keeps resolving too.
        if target_type.is_some() && name.eq_ignore_ascii_case("class") {
            let class = self.stdlib.class("Type");
            self.refs.set_with_highlight(
                ptr,
                highlight,
                class.map_or(Resolution::Unresolved, |c| {
                    Resolution::StdlibMember(Box::new(stdlib_member_ref(c.namespace.clone(), &c.name, None, None, None)))
                }),
            );
            return class.map(|c| Ty::system_owned(c.name.clone(), Vec::new()));
        }

        // `SObjectTypeName.SObjectType` (`Opportunity.SObjectType`,
        // `Schema.Opportunity.SObjectType` once `Schema.Opportunity`
        // itself resolves as a real schema object via the fallback
        // further down) -- another compiler-magic universal property,
        // confirmed against a real org, this time specifically on any
        // real SObject type name (standard or custom). Not a real
        // documented member of anything (`self.schema.field`/the stdlib-
        // property lookup below never find it), so this needs the same
        // early, targeted intercept `.class` gets just above -- scoped
        // specifically to a confirmed real schema object receiver,
        // unlike `.class`, which applies to any type at all.
        if let Some(Ty::System { name: object, .. }) = &target_type {
            if name.eq_ignore_ascii_case("SObjectType") && self.schema.object(object).is_some() {
                let class = self.stdlib.class("SObjectType");
                self.refs.set_with_highlight(
                    ptr,
                    highlight,
                    class.map_or(Resolution::Unresolved, |c| {
                        Resolution::StdlibMember(Box::new(stdlib_member_ref(c.namespace.clone(), &c.name, None, None, None)))
                    }),
                );
                return class.map(|c| Ty::system_owned(c.name.clone(), Vec::new()));
            }
        }

        // `Label.<fullName>`/`System.Label.<fullName>` (a custom-label
        // read, e.g. `System.Label.fflib_security_error_object_not_insertable`)
        // -- `Label` itself is a real stdlib class (`System.Label`), so
        // `target_type` is already `Ty::System { name: "Label", .. }` by
        // the time either spelling reaches here: a bare `Label` receiver
        // resolves via `bind_name_expr`'s stdlib-class fallback, and
        // `System.Label` resolves via this same function's own
        // `class_in_namespace` fallback one level up the chain. Checked
        // before the stdlib-property lookup below: `Label`'s own scraped
        // stdlib entry has zero documented properties (real custom label
        // names are, by definition, never part of any fixed/bundled set
        // -- see `apex_metadata::LabelSchema`'s doc comment), so there's
        // no ambiguity to resolve between the two sources. Always types
        // as `String`: a label reference is never anything else in real
        // Apex, `{0}`-style placeholders included -- those are substituted
        // separately via `String.format`, not part of the label's own
        // type. A name this project has no local `.labels-meta.xml`
        // declaration for -- most often a namespace segment in the
        // `Label.<namespace>.<name>` cross-package form (`System.Label.npo02.Foo`,
        // real Apex syntax for disambiguating a label declared in a
        // specific installed package; SFDX metadata never records a
        // package's own namespace in its `.labels-meta.xml` files, so
        // there's no local data to resolve that segment against) -- stays
        // honestly `Unresolved` with no propagated type, the same as
        // every other lookup miss in this function, rather than guessing
        // `String` anyway and risking a further chained call resolving
        // against the wrong type.
        if let Some(Ty::System { name: object, .. }) = &target_type {
            if object.eq_ignore_ascii_case("Label") {
                return match self.labels.get(name) {
                    Some(label) => {
                        let resolution = Resolution::Label(Box::new(LabelRef {
                            full_name: label.full_name.clone(),
                        }));
                        self.refs.set_with_highlight(ptr, highlight, resolution);
                        Some(Ty::system("String"))
                    }
                    None => {
                        self.refs.set_with_highlight(ptr, highlight, Resolution::Unresolved);
                        None
                    }
                };
            }
        }

        // `Page.<name>` (`PageReference pr = Page.MyPage;`) -- pure
        // compiler-magic syntax, unlike `Label`/`Schema`: there is no real
        // "Page" class anywhere in Salesforce's own docs (confirmed: no
        // such entry in `apex_reference.json`, unlike `Label`'s real
        // `System.Label` one), so the bare `Page` identifier itself has
        // nothing to resolve to and honestly stays `Resolution::Unresolved`
        // via the ordinary `bind_expr` call for `f.target()` at the top of
        // this function -- `target_type` is `None` here as a direct
        // result. Detected instead from the receiver's own raw token text
        // (there's no resolved `Ty` to key off, unlike every other special
        // case in this function), the same "recognize the two-token shape
        // itself" approach the `.class` idiom above already takes for a
        // different reason. Only applies when nothing else already
        // resolved as `Page` (`target_type.is_none()`) -- Apex doesn't
        // reserve `Page` as a keyword, so a project genuinely declaring
        // its own real `Page` type correctly shadows this instead. Always
        // types as `PageReference` (a real, documented stdlib class) on a
        // successful lookup, matching real Apex.
        if target_type.is_none() {
            if let Some(Expr::Name(target_name)) = f.target() {
                if target_name
                    .name_token()
                    .is_some_and(|t| t.text().eq_ignore_ascii_case("Page"))
                {
                    return match self.pages.get(name) {
                        Some(page) => {
                            let resolution = Resolution::VisualforcePage(Box::new(VisualforcePageRef {
                                name: page.name.clone(),
                            }));
                            self.refs.set_with_highlight(ptr, highlight, resolution);
                            Some(Ty::system("PageReference"))
                        }
                        None => {
                            self.refs.set_with_highlight(ptr, highlight, Resolution::Unresolved);
                            None
                        }
                    };
                }
            }
        }

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
                // The generic `SObject` type itself is never a registered
                // `self.schema.object(...)` entry -- it isn't a real,
                // queryable object, just Apex's common base type -- so
                // `field_schema`/`self.schema.object` both miss every
                // field on it, `Id` included. Real Apex still allows
                // `.Id` directly on any `SObject`-typed value with no
                // cast, unlike every other field (`.Name`, say, isn't
                // guaranteed to exist on every SObject the way `Id` is):
                // confirmed against a real org, and also confirmed
                // against this project's own bundled stdlib snapshot --
                // `System.SObject`'s scraped `properties` list is
                // genuinely empty (Salesforce's own docs describe `Id`
                // as a schema field, not a class member), so there's no
                // stdlib-property fallback to reach for either. This is
                // the one narrow, language-level exception, not a general
                // "SObject has every field" relaxation.
                let is_universal_id = object.eq_ignore_ascii_case("SObject") && real_field_name.eq_ignore_ascii_case("Id");
                if field_schema.is_some() || is_universal_id || self.schema.object(&object).is_some() {
                    let resolution = match &field_schema {
                        Some(_) => Resolution::SchemaObject(Box::new(SchemaObjectRef {
                            object: object.clone(),
                            field: Some(SmolStr::new(&real_field_name)),
                        })),
                        None if is_universal_id => Resolution::SchemaObject(Box::new(SchemaObjectRef {
                            object: object.clone(),
                            field: Some(SmolStr::new(&real_field_name)),
                        })),
                        None => Resolution::UnknownSchema(Box::new(UnknownSchemaRef {
                            object: Some(object.clone()),
                            field: Some(SmolStr::new(&real_field_name)),
                        })),
                    };
                    self.refs.set_with_highlight(ptr, highlight, resolution);
                    return field_schema.and_then(|f| {
                        f.reference_to
                            .first()
                            .map(|next| Ty::system_owned(next.clone(), Vec::new()))
                            .or_else(|| {
                                f.field_type
                                    .as_deref()
                                    .and_then(apex_type_for_schema_field_type)
                                    .map(|apex_type| Ty::system(apex_type))
                            })
                    });
                }
                // Not a known SObject/field at all (real or standard) --
                // try a stdlib class property before finally giving up
                // (e.g. accessing a documented static/instance property
                // on `String`/`Database`/... -- properties have no
                // overloads, so this is a plain existence-plus-type
                // lookup, unlike the method-call arm's narrowing).
                let class = self.stdlib.class(&object);
                let property = class.and_then(|c| StdlibIndex::property_of(c, name));
                if let Some(prop) = property {
                    self.refs.set_with_highlight(
                        ptr,
                        highlight,
                        Resolution::StdlibMember(Box::new(stdlib_member_ref(
                            class.and_then(|c| c.namespace.clone()),
                            &object,
                            Some(name),
                            None,
                            None,
                        ))),
                    );
                    return prop.type_name.as_deref().map(|type_name| ty_from_scraped_type(self.stdlib, type_name));
                }
                // `name` isn't a *property* of `object` -- but `object`
                // (e.g. `Schema`, itself a real class as well as a real
                // namespace -- `Schema.getGlobalDescribe()` is a real
                // static call) might still be acting as a namespace
                // prefix here, not a value: `Schema.SoapType` used in
                // expression position (`... == Schema.SoapType.ID`), the
                // same namespace-qualified shape `resolve_type_ref`'s own
                // two-segment `class_in_namespace` fallback already
                // handles for a *type* reference, just never mirrored
                // here for an *expression* one. Without this,
                // `Schema.SoapType` had no member model at all (`SoapType`
                // is a whole separate class, not a property of `Schema`),
                // so it -- and every further member off it, like the real
                // bug this fixes, `Schema.SoapType.ID` -- stayed
                // `Unresolved` unconditionally.
                if let Some(namespaced) = self.stdlib.class_in_namespace(&object, name) {
                    self.refs.set_with_highlight(
                        ptr,
                        highlight,
                        Resolution::StdlibMember(Box::new(stdlib_member_ref(
                            namespaced.namespace.clone(),
                            &namespaced.name,
                            None,
                            None,
                            None,
                        ))),
                    );
                    return Some(Ty::system_owned(namespaced.name.clone(), Vec::new()));
                }
                // `Schema.Opportunity` (a real schema object referenced
                // through the `Schema` namespace prefix, real Apex --
                // confirmed against a real org, most often immediately
                // followed by `.SObjectType`, the early check above) --
                // `name` isn't a stdlib *class* in this namespace, but it
                // might still be a real SObject, standard or custom,
                // reachable the same way a bare `Opportunity` `NameExpr`
                // already resolves (`bind_name_expr`'s own `self.schema.object`
                // fallback).
                if self.schema.object(name).is_some() {
                    self.refs.set_with_highlight(
                        ptr,
                        highlight,
                        Resolution::SchemaObject(Box::new(SchemaObjectRef {
                            object: SmolStr::new(name),
                            field: None,
                        })),
                    );
                    return Some(Ty::system_owned(SmolStr::new(name), Vec::new()));
                }
                self.refs.set_with_highlight(ptr, highlight, Resolution::Unresolved);
                return None;
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
        let members = prefer_value_over_same_named_type(self.table, members);
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

    /// A conservative, ambiguity-averse name lookup used only for
    /// dynamic-SOQL data-flow tracing (`dynamic_soql_source_tokens`):
    /// local/parameter scope, then a plain field/property lookup on the
    /// *immediately* enclosing type only (not `bind_name_expr`'s full
    /// outward nested-class climb, and no same-name type-vs-value
    /// shadowing resolution). Getting this wrong only means "the query
    /// text isn't found, so no binds get marked used" -- a missed
    /// opportunity, never a wrong goto-definition target -- so this
    /// deliberately doesn't replicate `bind_name_expr`'s full precision;
    /// see that function's own doc comment for what real name resolution
    /// actually requires.
    fn resolve_query_variable(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        if let Some(local) = self.scopes.resolve_local(scope, name) {
            return Some(local);
        }
        let container = self.enclosing_type?;
        let mut fields = self
            .table
            .lookup_member(container, name)
            .into_iter()
            .filter(|&id| self.table.get(id).kind == SymbolKind::Field);
        let first = fields.next()?;
        if fields.next().is_some() {
            return None; // ambiguous -- never guess which one
        }
        Some(first)
    }

    /// Every string-literal token reachable while tracing `expr`'s
    /// possible value(s), for the purpose of scanning it for dynamic-SOQL
    /// bind variables (`bind_dynamic_soql_binds`). `+`-concatenation
    /// collects from both sides; a parenthesized expression recurses
    /// through; a bare name (a local or a field of the immediately
    /// enclosing type, via `resolve_query_variable`) resolves one hop
    /// back to whatever it was last assigned from -- a local uses
    /// `last_assignment_to_local` (straight-line/branch-free: the
    /// nearest textually-preceding declaration-with-initializer or plain
    /// reassignment in the same enclosing-block chain as `anchor`, the
    /// call site itself), a field uses its own declared initializer only
    /// (`crate::symbol::Symbol::ptr` points at the specific `VarDeclarator`
    /// a field's `SymbolId` names -- see `collect::collect_field` --
    /// re-resolved against this file's current tree the same way any
    /// other `SyntaxPtr` is). Anything else (a method call's result, a
    /// ternary, a ` += `-style compound reassignment, ...) contributes
    /// nothing. Deliberately *not* a general data-flow analysis -- only
    /// the bounded, common "declare/assign a query string in this method,
    /// then pass it to `Database.query`" pattern; `depth` guards against
    /// a pathological concatenation/self-referential chain rather than
    /// assuming real Apex code never gets deep enough to matter.
    fn dynamic_soql_source_tokens(
        &self,
        scope: ScopeId,
        anchor: &apex_syntax::SyntaxNode,
        expr: &Expr,
        depth: u32,
    ) -> Vec<apex_syntax::SyntaxToken> {
        const MAX_DEPTH: u32 = 8;
        if depth > MAX_DEPTH {
            return Vec::new();
        }
        match expr {
            Expr::Literal(lit) => match lit.token() {
                Some(tok)
                    if matches!(
                        tok.kind(),
                        SyntaxKind::StringLiteral | SyntaxKind::MultilineStringLiteral
                    ) =>
                {
                    vec![tok]
                }
                _ => Vec::new(),
            },
            Expr::Paren(p) => p
                .inner()
                .map(|inner| self.dynamic_soql_source_tokens(scope, anchor, &inner, depth + 1))
                .unwrap_or_default(),
            Expr::Bin(b) => {
                let op: String = b.operator_tokens().iter().map(|t| t.text()).collect();
                if op != "+" {
                    return Vec::new();
                }
                let mut tokens = b
                    .lhs()
                    .map(|l| self.dynamic_soql_source_tokens(scope, anchor, &l, depth + 1))
                    .unwrap_or_default();
                if let Some(r) = b.rhs() {
                    tokens.extend(self.dynamic_soql_source_tokens(scope, anchor, &r, depth + 1));
                }
                tokens
            }
            Expr::Name(n) => {
                let Some(name_tok) = n.name_token() else {
                    return Vec::new();
                };
                let name = name_tok.text();
                let Some(id) = self.resolve_query_variable(scope, name) else {
                    return Vec::new();
                };
                let source = match self.get_symbol(id).kind {
                    SymbolKind::LocalVar => last_assignment_to_local(anchor, name),
                    SymbolKind::Field => {
                        let root = anchor.ancestors().last().unwrap_or_else(|| anchor.clone());
                        self.get_symbol(id)
                            .ptr
                            .to_node(&root)
                            .and_then(VarDeclarator::cast)
                            .and_then(|d| d.init())
                    }
                    _ => None,
                };
                match source {
                    Some(e) => self.dynamic_soql_source_tokens(scope, anchor, &e, depth + 1),
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// Handles `Database.query`/`countQuery`/`getQueryLocator`'s dynamic-
    /// SOQL string argument: finds every string literal that could reach
    /// it (`dynamic_soql_source_tokens`), and for each one that looks
    /// like a real SOQL query (`looks_like_soql` -- a cheap guard against
    /// treating an unrelated string's incidental `:word` pattern as a
    /// real bind), scans it for `:identifier` bind variables
    /// (`find_bind_vars`) and records each one that resolves against the
    /// call site's own local/parameter scope. This is the fix for two
    /// real, related problems: goto-definition on a `:nameVar` bind
    /// previously did nothing (there was no reference recorded for it at
    /// all -- string content isn't tokenized into anything the binder
    /// ever walks), and `apex_binder::dead_code` incorrectly flagged the
    /// bound variable as unused (its only real "use" was invisible to
    /// `ReferenceTable`, which only ever sees real AST-node references).
    ///
    /// Deliberately conservative in scope, matching this project's "never
    /// guess" posture: only a bind name that resolves unambiguously
    /// against local/parameter scope gets recorded (a field bind is not
    /// resolved here -- see `resolve_query_variable`'s doc comment on why
    /// that's a wider, riskier lookup than tracing *which* string to scan
    /// in the first place); an unresolved bind name is silently skipped,
    /// never recorded as `Unresolved` (a scan over arbitrary string text
    /// finding a plausible-looking identifier that happens not to exist
    /// is meaningfully weaker evidence than a real unresolved reference
    /// elsewhere, and this project's diagnostics don't want that noise).
    ///
    /// **Known, deliberate gap: no interprocedural tracing.**
    /// `dynamic_soql_source_tokens` only ever looks within the current
    /// method body (a literal, a `+`-concatenation, or one hop back
    /// through a local's own last straight-line assignment / a field's
    /// own declared initializer). A query string assembled in a
    /// *different* method -- most commonly the fflib-apex-common
    /// `QueryFactory` fluent-builder idiom,
    /// `newQueryFactory().setCondition('id in :idSet').toSOQL()`, where
    /// `setCondition`'s argument is stored into a field by one method and
    /// read back by a different one, possibly in another file entirely --
    /// is not traced, so a bind reachable only that way neither escapes
    /// `dead_code`'s false-positive nor gets a goto-definition target.
    /// This was scoped out deliberately (see `BACKLOG.md` §4's own entry
    /// for the full reasoning) rather than built speculatively: real
    /// cross-method tracing needs fetching another method's body
    /// (architecturally cheap -- `crate::BoundProgram::from_files_cached`
    /// already builds a project-wide `FxHashMap<FileId, Parse>` before
    /// Pass 2 runs, `parse_by_file`, just not yet threaded into
    /// `BodyBinder`) plus recognizing a fluent setter's `return this;`
    /// shape to track builder state across a call chain -- meaningfully
    /// more surface and risk than this same-method version, for a
    /// narrower payoff. The current behavior is a false negative only (a
    /// missed reference/goto-target), never a wrong one.
    fn bind_dynamic_soql_binds(&mut self, scope: ScopeId, call_node: &apex_syntax::SyntaxNode, arg: &Expr) {
        let tokens = self.dynamic_soql_source_tokens(scope, call_node, arg, 0);
        // The SOQL-shape guard applies to the *whole* reconstructed query,
        // not each collected token individually -- a concatenated query
        // (`'SELECT Id FROM Account ' + 'WHERE Name = :nameVar'`) commonly
        // splits "SELECT ... FROM ..." into one piece and its `WHERE
        // ... :bind` clause into another, so requiring every single piece
        // to independently look SOQL-shaped would silently drop binds
        // living in a piece that happens not to contain "select" itself.
        if !tokens.iter().any(|t| looks_like_soql(t.text())) {
            return;
        }
        for token in &tokens {
            let container = SyntaxPtr::for_token(self.file, token);
            for (name, range) in find_bind_vars(token) {
                let Some(id) = self.scopes.resolve_local(scope, name) else {
                    continue;
                };
                let sub_ptr = container.with_range(range);
                self.refs
                    .set_dynamic_soql_bind(container, sub_ptr, Resolution::Resolved(id));
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

        // `sobjectExpr.FieldName.addError(errorMsg)` -- a real,
        // documented Apex compiler idiom (the canonical way to attach a
        // field-level validation error, e.g. `Trigger.new[0].SomeField.addError('msg')`),
        // confirmed against a real org: `.addError` is valid on *any*
        // field-value access chained off an SObject record, regardless
        // of that field's own scalar type (`Opportunity.Type` is a
        // `Picklist`/`String`-typed field; `opp.Type.addError(...)` still
        // compiles) -- not really calling `addError` on the field's VALUE
        // type at all, compiler magic recognizing this specific syntactic
        // shape. `SObject.addError` itself already resolves fine when
        // called on the *object* directly (`opp.addError(...)`,
        // `System.SObject`'s own scraped method, matched by the ordinary
        // `Ty::System` arm below when the target is a *reference* field
        // like `opp.AccountId`) -- this covers only the narrower gap: a
        // *scalar* field's own value has no real `addError` method to
        // find on its own type (`String.addError` isn't a real stdlib
        // method), so it stayed `Unresolved` unconditionally otherwise.
        // Guarded to only apply when the target isn't already a real
        // project type (`Ty::Project`) -- a user-defined class with its
        // own, unrelated `addError` method reached via a property chain
        // must still resolve through the ordinary member lookup below,
        // never overridden by this.
        if name.eq_ignore_ascii_case("addError")
            && !matches!(target_type, Some(Ty::Project(_)))
            && matches!(mc.target(), Some(Expr::Field(_)))
        {
            let class = self.stdlib.class("SObject");
            let resolution = class.map_or(Resolution::Unresolved, |c| {
                Resolution::StdlibMember(Box::new(stdlib_member_ref(
                    c.namespace.clone(),
                    &c.name,
                    Some(name),
                    Some(arg_types.len()),
                    None,
                )))
            });
            self.refs.set_with_highlight(ptr, highlight, resolution);
            return None;
        }

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
                    narrow_by_overload(self.schema, self.stdlib, self.table, methods, &arg_types),
                );
                // This project's own inheritance chain has nothing by
                // this name -- but `container` (or an ancestor of it)
                // might still `extends` a *real* standard-library type
                // this project's `SymbolTable` could never resolve in the
                // first place (`extends Exception`, most commonly: see
                // `SymbolTable::unresolved_direct_super`'s own doc
                // comment). Without this, every custom exception
                // subclass's own inherited `getMessage`/`setMessage`/
                // `getCause`/... call -- extremely common real Apex,
                // `Exception` being the one base *every* custom exception
                // has -- looked exactly like a typo. Only tried when the
                // project-local lookup came up empty: a real project-
                // declared override always wins outright, same priority
                // order `lookup_member`'s own inherited-chain walk
                // already gives a project ancestor over a further one.
                let stdlib_fallback = matches!(resolution, Resolution::Unresolved)
                    .then(|| stdlib_class_via_unresolved_supertype(self.table, self.stdlib, container))
                    .flatten()
                    .and_then(|c| narrow_stdlib_overload(self.schema, self.stdlib, self.table, c, name, &arg_types).map(|m| (c, m)));
                let (resolution, result_type) = match stdlib_fallback {
                    Some((c, winner)) => (
                        Resolution::StdlibMember(Box::new(stdlib_member_ref(
                            c.namespace.clone(),
                            &c.name,
                            Some(name),
                            Some(arg_types.len()),
                            None,
                        ))),
                        // `result_type_of` only ever handles `Resolved`/
                        // `Candidates` (a project `SymbolId`'s own type),
                        // never `StdlibMember` -- computed directly here
                        // instead, the same way the `Ty::System` arm below
                        // already does for every other stdlib call. Without
                        // this, the *resolution* was already correct
                        // (`getMessage()` itself resolved) but its *result
                        // type* silently stayed `None`, so a further
                        // chained call (`caughtEx.getMessage().contains(...)`,
                        // a real fflib_SObjectUnitOfWorkTest.cls shape)
                        // stayed `Unresolved` right after -- a real bug in
                        // this fallback's own first version, not a
                        // pre-existing gap.
                        stdlib_method_return_ty(self.stdlib, winner),
                    ),
                    None => {
                        let result_type = self.result_type_of(&resolution);
                        (resolution, result_type)
                    }
                };
                self.refs.set_with_highlight(ptr, highlight, resolution);
                result_type
            }
            Some(Ty::System { name: base, args }) => {
                // No `SymbolId` backs either a built-in generic method or
                // a scraped stdlib one -- there's no real declaration for
                // goto-definition to point at -- but a *real, documented*
                // stdlib member (checked via `self.stdlib`, independent
                // of whichever `Ty` ends up propagating) still escapes
                // `Unresolved` into `Resolution::StdlibMember`, so a
                // genuine typo stays distinguishable from a real call --
                // see `crate::reference_table::Resolution`'s own doc
                // comment on why that distinction exists at all.
                let class = self.stdlib.class(&base);
                // A real object (standard or custom) is never itself a
                // stdlib class by that exact name (`self.stdlib.class`
                // only ever indexes `apex_stdlib::standard_classes`), so
                // `class` alone never accounts for the generic instance
                // methods every real object actually has -- `get`/`put`/
                // `getSObjectType`/`clone`/`addError`/`getErrors`/... are
                // all declared once on the scraped `SObject` class itself
                // (`apex_stdlib::standard_classes()` really does have a
                // `"SObject"`/`"System"` entry with these, confirmed by
                // direct inspection of `data/apex_reference.json`), not
                // repeated per concrete object type. Falls back to it only
                // when `base` is confirmed to actually *be* a real object
                // (`self.schema.object`, not a name guess) -- otherwise an
                // unrelated `Ty::System` name that merely happens to share
                // a method name with `SObject` (there are none today, but
                // nothing guarantees that forever) would wrongly resolve.
                let method_class = class
                    .filter(|c| StdlibIndex::methods_of(c, name).next().is_some())
                    .or_else(|| {
                        self.schema
                            .object(&base)
                            .is_some()
                            .then(|| self.stdlib.class("SObject"))
                            .flatten()
                            .filter(|c| StdlibIndex::methods_of(c, name).next().is_some())
                    })
                    .or_else(|| {
                        // A built-in Apex exception *subtype*
                        // (`DmlException`, `QueryException`,
                        // `NullPointerException`, ...) -- Salesforce's own
                        // docs only cover these in prose alongside
                        // `Exception` itself, never as their own scraped
                        // class/method reference page, so `class` above
                        // is always `None` for one of these (confirmed:
                        // no `apex_stdlib::standard_classes()` entry at
                        // all, same gap `Exception` itself had before
                        // this crate's own entry was hand-corrected).
                        // Every real Apex exception class name ends in
                        // literally `Exception` -- not just convention,
                        // a hard compiler rule (confirmed against a real
                        // org: "Classes extending Exception must have a
                        // name ending in Exception") -- so this is a
                        // safe, fully general signal, not fflib-specific.
                        // Real NPSP shape: `System.DmlException caughtEx
                        // = null; ... caughtEx.getMessage()`.
                        base.ends_with("Exception")
                            .then(|| self.stdlib.class("Exception"))
                            .flatten()
                            .filter(|c| StdlibIndex::methods_of(c, name).next().is_some())
                    });
                let winner = method_class
                    .and_then(|c| narrow_stdlib_overload(self.schema, self.stdlib, self.table, c, name, &arg_types));
                // Only worth the `Vec` allocation when arity alone
                // couldn't already have told a hover/signature-help
                // consumer which overload this is -- a genuine
                // same-arity overload pair (`List.addAll(List)` vs.
                // `List.addAll(Set)`, both one parameter, but List/Set
                // aren't implicitly convertible) is real but rare (per
                // `StdlibMemberRef::narrowed_param_types`'s own doc
                // comment, different-arity is far more common in the
                // scraped data), so the overwhelming majority of real
                // stdlib calls -- a single overload, or an arity-
                // disambiguated one -- skip this entirely and cost
                // nothing beyond the cheap, allocation-free arity recount
                // below. `None` whenever it doesn't apply, or `winner`
                // itself is ambiguous or missing a scraped param type.
                let narrowed_param_types = method_class.zip(winner).and_then(|(c, m)| {
                    let mut same_arity =
                        StdlibIndex::methods_of(c, name).filter(|o| o.params.len() == arg_types.len());
                    same_arity.next()?;
                    same_arity.next()?; // fewer than two same-arity candidates -- arity alone already disambiguates
                    m.params.iter().map(|p| p.type_name.clone()).collect::<Option<Vec<_>>>()
                });
                let resolution = match method_class {
                    Some(c) => Resolution::StdlibMember(Box::new(stdlib_member_ref(
                        c.namespace.clone(),
                        &c.name,
                        Some(name),
                        Some(arg_types.len()),
                        narrowed_param_types,
                    ))),
                    None => Resolution::Unresolved,
                };
                self.refs.set_with_highlight(ptr, highlight, resolution);
                // Dynamic SOQL: `Database.query`/`countQuery`/`getQueryLocator`'s
                // first argument is a plain `String`, not a real parsed SOQL
                // expression -- unlike `queryWithBinds`/`countQueryWithBinds`/
                // `getQueryLocatorWithBinds`, deliberately excluded here since
                // their bind names are looked up as *keys in an explicit
                // `Map<String, Object>` argument*, not against lexical scope
                // at all, so resolving them the same way here would be
                // outright wrong, not just unhelpful. See `bind_dynamic_soql_binds`'s
                // own doc comment for what this actually does.
                if base.eq_ignore_ascii_case("Database")
                    && matches!(name.to_ascii_lowercase().as_str(), "query" | "countquery" | "getquerylocator")
                {
                    if let Some(first_arg) = mc.args().and_then(|a| a.args().next()) {
                        self.bind_dynamic_soql_binds(scope, mc.syntax(), &first_arg);
                    }
                }
                // `generics.rs` handles type-*argument substitution*
                // (`List<Account>.get(0)` returning `Account`, not
                // whatever a raw scraped signature says) -- tried first,
                // and always wins when it applies. Only when it doesn't
                // (a non-generic class entirely, or a `List`/`Map`/`Set`
                // member `generics.rs` doesn't model, like `sort`/
                // `addAll`, which need no substitution anyway) does the
                // scraped, best-effort-narrowed return type get used.
                crate::generics::builtin_generic_member_type(class, &base, &args, name)
                    .or_else(|| winner.and_then(|m| stdlib_method_return_ty(self.stdlib, m)))
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
            // cases, but with one real difference a plain field/type
            // lookup doesn't need: a method name can be *overloaded*, so
            // finding *any* same-named method at a level isn't enough
            // reason to stop there -- only a same-named method that also
            // matches this call's own arity shadows the outer scope.
            // Confirmed against a real org: a nested class's own single
            // 1-arg `meetsCriteria` did **not** shadow its outer class's
            // unrelated 2-arg static `meetsCriteria` for a 2-arg
            // unqualified call from inside the nested class -- that
            // deploys and runs successfully in real Apex, which the
            // original "stop at the first non-empty level" version of
            // this climb got wrong (found only the inner 1-arg method,
            // never reaching the real 2-arg target at all). Confirmed via
            // `crates/apex-binder/tests/resolution_consistency.rs`'s
            // whole-corpus arity/name self-check, which caught this
            // exact shape live in NPSP's `UTIL_Where.cls`.
            let mut candidates = Vec::new();
            let mut enclosing_chain = Some(container);
            while let Some(level) = enclosing_chain {
                let level_candidates: Vec<SymbolId> = self
                    .table
                    .lookup_member(level, name)
                    .into_iter()
                    .filter(|&id| {
                        self.table.get(id).kind == SymbolKind::Method
                            && self.table.is_visible_from(id, self.enclosing_type)
                    })
                    .collect();
                if !level_candidates.is_empty() {
                    let arity_matches_here = level_candidates
                        .iter()
                        .any(|&id| self.table.params(id).len() == arg_types.len());
                    candidates = level_candidates;
                    if arity_matches_here {
                        break;
                    }
                }
                enclosing_chain = self.table.get(level).container;
            }
            candidates
        };

        let resolution = narrow_by_overload(self.schema, self.stdlib, self.table, candidates, &arg_types);
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
        let result_type = self.result_type_of(&resolution);
        self.refs.set_with_highlight(ptr, highlight, resolution);
        result_type
    }

    fn bind_new_expr(&mut self, scope: ScopeId, ne: &NewExpr) -> Option<Ty> {
        let resolved_type = ne.type_ref().and_then(|t| self.resolve_type_ref(&t));
        // `new Contact(LastName = 'foo', Primary_Affiliation__c = acc.id)`
        // -- Apex's SObject constructor sugar for setting fields by name
        // -- parses as a perfectly ordinary `Expr::Bin` (`=` is just the
        // normal assignment operator, `arg_list`'s grammar has no special
        // case for it: `crates/apex-parser/src/grammar/expressions.rs`'s
        // `arg_list` is shared verbatim by `CallExpr`/`MethodCallExpr`/
        // `NewExpr`). Without this, each `LastName`/`Primary_Affiliation__c`
        // LHS fell through `bind_name_expr`'s ordinary local/member/type
        // lookups (none of which a bare field name ever matches) straight
        // to `Resolution::Unresolved`, so goto-definition on it did
        // nothing -- only meaningful for a real schema object target
        // (`self.schema.object` confirms it, not just any `Ty::System`:
        // `new List<Integer>()`'s target is `Ty::System` too, but never
        // takes `field = value` args).
        let sobject_name = match &resolved_type {
            Some(Ty::System { name, .. }) if self.schema.object(name).is_some() => Some(name.clone()),
            _ => None,
        };
        if let Some(args) = ne.args() {
            let mut arg_types = Vec::new();
            for a in args.args() {
                let bound = match (&sobject_name, sobject_field_init(&a)) {
                    (Some(object), Some((field_name, rhs))) => {
                        self.bind_sobject_field_init(object, &field_name);
                        self.bind_expr(scope, &rhs)
                    }
                    _ => self.bind_expr(scope, &a),
                };
                arg_types.push(bound);
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
                    let resolution = narrow_by_overload(self.schema, self.stdlib, self.table, ctors, &arg_types);
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

    /// Resolves `field_name` (an SObject constructor field-init's LHS,
    /// e.g. `Primary_Affiliation__c` in `new Contact(Primary_Affiliation__c
    /// = acc.id)`) against `object`'s schema, mirroring `bind_field_expr`'s
    /// `Ty::System` arm -- `Resolution::SchemaObject` for a real field,
    /// `Resolution::UnknownSchema` otherwise, so a genuine typo stays
    /// distinguishable from a real (if locally-unmodeled) field the same
    /// way every other schema reference already does. No `set_with_highlight`
    /// call needed: unlike `FieldExpr`, `field_name`'s own `NameExpr` node
    /// range already *is* just the identifier (see `bind_name_expr`'s own
    /// doc comment on why `NameExpr` deliberately never narrows its range
    /// this way either).
    fn bind_sobject_field_init(&mut self, object: &str, field_name: &NameExpr) {
        let Some(tok) = field_name.name_token() else {
            return;
        };
        let name = tok.text();
        let ptr = SyntaxPtr::new(self.file, field_name.syntax());
        let resolution = match self.schema.field(object, name) {
            Some(_) => Resolution::SchemaObject(Box::new(SchemaObjectRef {
                object: SmolStr::new(object),
                field: Some(SmolStr::new(name)),
            })),
            None => Resolution::UnknownSchema(Box::new(UnknownSchemaRef {
                object: Some(SmolStr::new(object)),
                field: Some(SmolStr::new(name)),
            })),
        };
        self.refs.set(ptr, resolution);
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
