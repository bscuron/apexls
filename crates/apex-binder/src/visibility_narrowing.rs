//! Provably-safe visibility-narrowing detection: the analysis backing
//! `apexls-server`'s `visibility_narrowing_diagnostics`. The mirror image
//! of `dead_code.rs` (used-nowhere): this flags a `public`/`protected`
//! member whose real, project-wide usage only ever needs a narrower
//! visibility, and computes the narrowest such level (its **required
//! visibility**) its actual references prove is legally sufficient.
//!
//! Wayfinder `apex-diagnostics` map, tickets 32 (research) -> 33
//! (decision) -> 34 (this implementation). Scope, locked by ticket 33 --
//! do not redesign here, see that ticket for the reasoning:
//!
//! - In scope: `Public` -> narrower, `Protected` -> `Private`. `Global` is
//!   out of scope (no namespace model exists to prove it safe, same
//!   reasoning `dead_code.rs` already uses to exclude `Global` from dead-
//!   code candidacy).
//! - `Method`/`Field`/`Property`/`Constructor` only -- nested/inner-class
//!   visibility narrowing is a deliberate follow-on, not this ticket.
//! - Reuses `dead_code.rs`'s full three-part exemption check verbatim
//!   (`has_platform_invocation_annotation`, `is_visualforce_referenced`,
//!   `is_platform_invoked_test_method`) -- the same channels that make a
//!   member provably-not-dead also make it provably-not-safely-narrowable,
//!   since narrowing could sever a channel this binder can't see a
//!   textual reference for.
//! - Any method that overrides a virtual/abstract member or satisfies an
//!   interface contract is excluded entirely: Apex forbids an overriding/
//!   interface-implementing method from being *less* visible than what it
//!   overrides/implements, so narrowing it isn't just risky, it wouldn't
//!   compile -- a hard correctness constraint, not a judgment call.
//! - A zero-reference candidate (`Public` or `Protected`) is skipped
//!   entirely, deferring to `dead_code_diagnostics` -- this diagnostic's
//!   own identity is "used, but too broadly," not "unused." For
//!   `Protected` this knowingly leaves a gap (`dead_code_diagnostics`
//!   never considers `Protected` candidacy at all today), accepted as
//!   pre-existing and out of scope for this diagnostic to patch over.

use crate::dead_code::{
    has_platform_invocation_annotation, is_platform_invoked_test_method, is_visualforce_referenced,
};
use crate::file_id::FileId;
use crate::ptr::SyntaxPtr;
use crate::symbol::{Symbol, SymbolId, SymbolKind, Visibility};
use crate::BoundProgram;
use rowan::TextRange;
use smol_str::SmolStr;

/// `symbol` is itself a real candidate shape -- the right kind, the right
/// (broader-than-narrowest) visibility, and not declared directly inside
/// an `Interface`. That last check matters because Apex forbids an
/// explicit access modifier on an interface method entirely (it's always
/// implicitly `public`/abstract) -- so an interface's own method
/// declaration is never itself narrowable, regardless of how narrowly
/// it's referenced; only a *concrete implementation* of it can be (and is
/// already separately excluded, when it is one, by `overrides_or_implements`).
fn is_narrowing_candidate_kind(program: &BoundProgram, symbol: &Symbol) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::Method | SymbolKind::Field | SymbolKind::Property | SymbolKind::Constructor
    ) && matches!(
        symbol.modifiers.visibility,
        Visibility::Public | Visibility::Protected
    ) && !symbol
        .container
        .is_some_and(|c| program.symbols.get(c).kind == SymbolKind::Interface)
}

/// Walks `id` up to its outermost enclosing type. A local reimplementation
/// of `SymbolTable`'s own private `top_level_of`, matching `dead_code.rs`'s
/// own precedent (`top_level_container`, `dead_code.rs:208-217`) of not
/// exposing new crate-wide `SymbolTable` surface for a three-line walk
/// only one module needs.
fn top_level_container(program: &BoundProgram, id: SymbolId) -> SymbolId {
    let mut current = id;
    while let Some(parent) = program.symbols.get(current).container {
        current = parent;
    }
    current
}

/// True for a `Method` that overrides a virtual/abstract member, satisfies
/// (same name, same arity) an interface/base-class method somewhere in
/// its container's `inherited_chain`, or satisfies a real Salesforce
/// standard-library interface's method the same way (`Database.Batchable`,
/// `Schedulable`, ... -- see `SymbolTable::implements_stdlib_interface_method`;
/// these never appear in `inherited_chain` itself, since they have no
/// `SymbolId`) -- either way, Apex forbids the method from being declared
/// *less* visible than what it overrides/implements, so narrowing it is
/// never safe regardless of its own real reference footprint. Mirrors `apexls-server`'s
/// `method_override_chain_reason`'s first two checks (its third,
/// "overridden by a subclass," doesn't apply here: narrowing a virtual
/// base method doesn't force narrowing whatever overrides it, and this
/// module's own reference-bucketing already keeps any base method whose
/// override is called from an unrelated file at its current visibility).
/// Only `Method` can carry this shape in Apex -- a `Field`/`Constructor`
/// has no override/interface-satisfaction concept at all, and while Apex
/// properties do support `virtual`/`override`, they can't satisfy an
/// interface member (Apex interfaces declare only methods), so a
/// `Property`'s own `is_override` flag alone is the whole check for it.
fn overrides_or_implements(program: &BoundProgram, id: SymbolId, symbol: &Symbol) -> bool {
    if symbol.modifiers.is_override {
        return true;
    }
    if symbol.kind != SymbolKind::Method {
        return false;
    }
    let Some(container) = symbol.container else {
        return false;
    };
    let arity = program.symbols.params(id).len();
    program.symbols.inherited_chain(container).iter().any(|&ancestor| {
        program
            .symbols
            .lookup_member(ancestor, &symbol.name)
            .into_iter()
            .any(|candidate| candidate != id && program.symbols.params(candidate).len() == arity)
    }) || program
        .symbols
        .implements_stdlib_interface_method(container, &symbol.name, arity)
}

/// The nearest enclosing `Class`/`Interface`/`Enum` symbol whose
/// declaration contains `reference`'s own node -- generalizes
/// `BoundProgram::enclosing_callable`'s ancestor-walk-then-range-match
/// pattern (`lib.rs:1157-1194`, built for `Method`/`Constructor`) to type
/// declarations instead. Deliberately matches the *innermost* enclosing
/// type via `.ancestors()`'s own innermost-to-outermost order -- a
/// reference written inside a nested class's own method finds that nested
/// class, not its outer one, which matters for same-file nested-class
/// bucketing (see this module's doc comment and `top_level_container`).
/// `None` for a reference this binder can't place at all: `reference` not
/// resolving to a real node against the current tree (a stale pointer),
/// or -- the common real case -- a token-shaped `SyntaxPtr`
/// (`SyntaxPtr::for_token`/`with_range`, e.g. one segment of a dotted
/// `Type` or a dynamic-SOQL bind-variable range) that `SyntaxPtr::to_node`
/// never resolves by construction. Callers treat `None` conservatively --
/// see `required_visibility`.
fn declaring_type_of_reference(program: &BoundProgram, reference: SyntaxPtr) -> Option<SymbolId> {
    let file = reference.file();
    let root = program.syntax(file);
    let node = reference.to_node(&root)?;
    let type_node = node.ancestors().find(|n| {
        matches!(
            n.kind(),
            apex_syntax::SyntaxKind::ClassDecl
                | apex_syntax::SyntaxKind::InterfaceDecl
                | apex_syntax::SyntaxKind::EnumDecl
        )
    })?;
    let range = type_node.text_range();
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .find(|(_, s)| {
            matches!(
                s.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
            ) && s.ptr.range() == range
        })
        .map(|(local, _)| SymbolId::new(file, local as u32))
}

/// The narrowest [`Visibility`] `id`'s real, project-wide references prove
/// is legally sufficient, given it currently has at least one reference
/// (callers must skip the zero-reference case themselves -- see this
/// module's own doc comment on why that's `dead_code_diagnostics`'s
/// territory, not this function's). `None` means "stays at `current`,
/// not a narrowing candidate at all" -- either because every reference
/// already needs `current`'s own breadth, or because a reference couldn't
/// be placed (`declaring_type_of_reference` returned `None`), which is
/// treated conservatively: never assume safety from a reference this
/// binder can't account for.
///
/// `Protected -> Private`: narrows iff every reference's declaring type
/// shares `id`'s own top-level family (`top_level_container` equality).
///
/// `Public -> narrower` (three-way): a same-family reference contributes
/// nothing; a reference from anywhere in `subtype_set` (already
/// transitive, cycle-guarded, self-exclusive -- see `SymbolTable::subtypes`;
/// the declaring type itself is never a member of its own `subtypes`, but
/// it's already caught by the same-family check above since it trivially
/// shares its own top-level container) means only inheritance reaches it,
/// so it contributes "needs at least `Protected`"; any other reference
/// means the member is genuinely used from an unrelated type and can't be
/// narrowed at all.
fn required_visibility(
    program: &BoundProgram,
    id: SymbolId,
    declaring_top_level: SymbolId,
    subtype_set: &[SymbolId],
    current: Visibility,
) -> Option<Visibility> {
    let mut needs_protected = false;
    for reference in program.references_to(id) {
        let from = declaring_type_of_reference(program, reference)?;
        if top_level_container(program, from) == declaring_top_level {
            continue;
        }
        match current {
            Visibility::Protected => return None,
            Visibility::Public => {
                if subtype_set.contains(&from) {
                    needs_protected = true;
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    match current {
        Visibility::Protected => Some(Visibility::Private),
        Visibility::Public => Some(if needs_protected {
            Visibility::Protected
        } else {
            Visibility::Private
        }),
        _ => None,
    }
}

/// `symbol` is a nested `Class`/`Interface`/`Enum` declared `Public`.
/// `Protected` is deliberately excluded -- flatly illegal on a nested type
/// (confirmed via `sf` CLI, ticket 35 §1) -- so unlike
/// [`is_narrowing_candidate_kind`] there is no `Public | Protected`
/// disjunction. No interface-body exclusion either: a nested type
/// declaration inside an interface body isn't a legal Apex shape at all,
/// so there's nothing to exclude.
fn is_type_narrowing_candidate_kind(symbol: &Symbol) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
    ) && symbol.container.is_some()
        && symbol.modifiers.visibility == Visibility::Public
}

/// The `Method`/`Constructor`/`Field`/`Property` symbol whose own
/// declared-type slot (a method's return type, or a field/property/
/// parameter's own type) `reference` sits inside -- `None` when
/// `reference` is in a body position instead (a `new` expression, a
/// local variable's type, `instanceof`, a supertype clause, ...),
/// including one nested inside a property accessor's own block. Walks
/// ancestors to whichever comes first: a `Block` (body position, stop
/// there) or one of the four decl kinds (a genuine signature slot) --
/// a `Block` is always found first for a body reference, since every one
/// of these decl kinds' own body, if it has one, is a `Block` strictly
/// nested inside it, never a sibling.
///
/// Real-world motivation (found via code review, confirmed via a real
/// `sf` CLI deploy against a connected org): a same-family public
/// accessor whose *declared return type is the concrete nested type
/// itself* (not wrapped in an interface, unlike ticket 36's own tested
/// bypass case) still requires the type's visibility once an external
/// caller resolves a call chained off that accessor, even though the
/// caller's own source never writes the nested type's name --
/// `Foo.make().greet()` fails to deploy once `Secret` (the real return
/// type of `Foo.make()`) is narrowed to `Private`, confirmed via a real
/// deploy: "Method return type Foo.Secret is not visible for: Foo.Secret
/// Foo.make()". Ticket 36's own two-way check missed this because the
/// leaking reference (the return-type slot inside `make()`) is itself a
/// same-family reference, so it never tripped the plain "any reference
/// from outside the family" test. Extending a private nested type from a
/// public one, by contrast, was confirmed *not* to leak this way (a real
/// deploy of a public `Sub extends Secret` with only `Sub` referenced
/// externally succeeds), so this check deliberately covers only the four
/// declared-type-slot decl kinds below, not a class/interface/enum's own
/// supertype clause.
/// True if `id` has at least one reference this binder can place outside
/// `declaring_top_level`'s own family, or at least one it can't place at
/// all -- treated the same conservative way [`required_visibility_for_type`]
/// itself treats an unplaceable reference: never assume `id` is only
/// reached from inside the family when a reference exists this binder
/// can't account for.
fn has_external_reference(program: &BoundProgram, id: SymbolId, declaring_top_level: SymbolId) -> bool {
    program.references_to(id).any(|reference| {
        declaring_type_of_reference(program, reference)
            .is_none_or(|from| top_level_container(program, from) != declaring_top_level)
    })
}

/// True if `reference` sits in a signature slot whose owning member is
/// still reachable from outside `declaring_top_level`'s own family --
/// see this section's own doc comment above for the real leak this
/// guards against. Finds the nearest ancestor among a `Block` (a body
/// position, stop: `false`) or one of `MethodDecl`/`ConstructorDecl`/
/// `FieldDecl`/`PropertyDecl` (a genuine signature slot).
///
/// `FieldDecl` needs its own case, not a plain range-match the other
/// three share: a field's declared type (`f.type_ref()`) is `FieldDecl`'s
/// own direct child, a *sibling* of each `VarDeclarator` (`public
/// Integer x, y;` has one shared `Integer` and two declarators) -- never
/// contained inside any one of them. Each `Field` symbol's own `ptr` is
/// its `VarDeclarator` node (`collect::collect_field`), strictly
/// *narrower* than the whole `FieldDecl`, so it can never equal
/// `FieldDecl`'s own range the way a `Method`/`Constructor`/`Property`
/// symbol's `ptr` (the whole decl node) equals its own. Every field a
/// `FieldDecl` declares shares one exposure surface either way, so this
/// checks every one of them and leaks if any is externally reachable.
fn signature_slot_leaks(
    program: &BoundProgram,
    reference: SyntaxPtr,
    declaring_top_level: SymbolId,
) -> bool {
    let file = reference.file();
    let root = program.syntax(file);
    let Some(node) = reference.to_node(&root) else {
        return false;
    };
    let Some(decl) = node.ancestors().find(|n| {
        matches!(
            n.kind(),
            apex_syntax::SyntaxKind::Block
                | apex_syntax::SyntaxKind::MethodDecl
                | apex_syntax::SyntaxKind::ConstructorDecl
                | apex_syntax::SyntaxKind::FieldDecl
                | apex_syntax::SyntaxKind::PropertyDecl
        )
    }) else {
        return false;
    };
    let range = decl.text_range();
    if decl.kind() == apex_syntax::SyntaxKind::FieldDecl {
        return program
            .symbols
            .symbols_of_file(file)
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SymbolKind::Field)
            .filter(|(_, s)| {
                s.ptr
                    .to_node(&root)
                    .and_then(|n| n.parent())
                    .is_some_and(|p| p.text_range() == range)
            })
            .any(|(local, _)| {
                has_external_reference(program, SymbolId::new(file, local as u32), declaring_top_level)
            });
    }
    let kind = match decl.kind() {
        apex_syntax::SyntaxKind::MethodDecl => SymbolKind::Method,
        apex_syntax::SyntaxKind::ConstructorDecl => SymbolKind::Constructor,
        apex_syntax::SyntaxKind::PropertyDecl => SymbolKind::Property,
        _ => return false, // Block: a body position, not a signature slot
    };
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .find(|(_, s)| s.kind == kind && s.ptr.range() == range)
        .is_some_and(|(local, _)| {
            has_external_reference(program, SymbolId::new(file, local as u32), declaring_top_level)
        })
}

/// The narrowest [`Visibility`] `id`'s real, project-wide references prove
/// is legally sufficient for a nested-type candidate. A genuine two-way
/// check, unlike [`required_visibility`]'s three-way `subtype_set`
/// disjunction: `Protected` is never a legal target for a nested type, so
/// there's no "needs at least the inheritance-only tier" outcome to
/// compute. Dispatch through a publicly-visible interface/base type never
/// names the concrete nested type externally, so it never produces a
/// reference to `id` at all (confirmed via real fixtures) -- but a
/// same-family reference sitting in a still-externally-reachable member's
/// own declared-type slot is a real leak the plain same-family check
/// alone misses, so every same-family reference also gets checked via
/// [`signature_slot_leaks`] before being accepted as safe (see that
/// function's own doc comment for the real, `sf`-CLI-confirmed break this
/// guards against).
fn required_visibility_for_type(
    program: &BoundProgram,
    id: SymbolId,
    declaring_top_level: SymbolId,
) -> Option<Visibility> {
    for reference in program.references_to(id) {
        let from = declaring_type_of_reference(program, reference)?;
        if top_level_container(program, from) != declaring_top_level {
            return None;
        }
        if signature_slot_leaks(program, reference, declaring_top_level) {
            return None;
        }
    }
    Some(Visibility::Private)
}

/// Every nested `Class`/`Interface`/`Enum` in `file` this binder can prove
/// could be declared `Private` instead of its current `Public`. A sibling
/// of [`narrowing_candidates_in_file`], not a folded-in pass: the
/// candidate-kind filter, exemption chain (none applies to a type itself
/// per ticket 35 §4), and `required_visibility` computation all differ
/// enough from the member-level check to warrant their own function
/// (ticket 36's locked placement decision).
pub fn type_narrowing_candidates_in_file(
    program: &BoundProgram,
    file: FileId,
) -> Vec<NarrowingCandidate> {
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .map(|(local, s)| (SymbolId::new(file, local as u32), s))
        .filter(|(_, s)| is_type_narrowing_candidate_kind(s))
        .filter(|(id, _)| program.references_to(*id).next().is_some())
        .filter_map(|(id, s)| {
            let declaring = s.container?;
            let declaring_top_level = top_level_container(program, declaring);
            let required = required_visibility_for_type(program, id, declaring_top_level)?;
            Some(NarrowingCandidate {
                ptr: s.ptr,
                name_range: s.name_range,
                kind: s.kind,
                name: s.name.clone(),
                current: s.modifiers.visibility,
                required,
            })
        })
        .collect()
}

/// One member this binder can *prove* is declared more broadly than its
/// real usage needs. `required` is always strictly narrower than
/// `current` -- nothing is ever constructed otherwise.
pub struct NarrowingCandidate {
    pub ptr: SyntaxPtr,
    pub name_range: TextRange,
    pub kind: SymbolKind,
    pub name: SmolStr,
    pub current: Visibility,
    pub required: Visibility,
}

/// Every member in `file` this binder can prove could be declared with a
/// narrower visibility than it currently has. See this module's own doc
/// comment for the full scope and why each exclusion exists.
pub fn narrowing_candidates_in_file(program: &BoundProgram, file: FileId) -> Vec<NarrowingCandidate> {
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .enumerate()
        .map(|(local, s)| (SymbolId::new(file, local as u32), s))
        .filter(|(_, s)| is_narrowing_candidate_kind(program, s))
        .filter(|(_, s)| !is_platform_invoked_test_method(program, s))
        .filter(|(_, s)| {
            !has_platform_invocation_annotation(program, s) && !is_visualforce_referenced(program, s)
        })
        .filter(|(id, s)| !overrides_or_implements(program, *id, s))
        .filter(|(id, _)| program.references_to(*id).next().is_some())
        .filter_map(|(id, s)| {
            let declaring = s.container?;
            let declaring_top_level = top_level_container(program, declaring);
            let subtype_set = program.symbols.subtypes(declaring);
            let required = required_visibility(
                program,
                id,
                declaring_top_level,
                subtype_set,
                s.modifiers.visibility,
            )?;
            Some(NarrowingCandidate {
                ptr: s.ptr,
                name_range: s.name_range,
                kind: s.kind,
                name: s.name.clone(),
                current: s.modifiers.visibility,
                required,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn write_fixture(
        test_name: &str,
        src: &str,
        extra_files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apex-binder-visibility-narrowing-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Foo.cls"), src).unwrap();
        for (name, content) in extra_files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    fn candidates(test_name: &str, src: &str) -> Vec<NarrowingCandidate> {
        candidates_with_extra_files(test_name, src, &[])
    }

    fn candidates_with_extra_files(
        test_name: &str,
        src: &str,
        extra_files: &[(&str, &str)],
    ) -> Vec<NarrowingCandidate> {
        let dir = write_fixture(test_name, src, extra_files);
        let program = BoundProgram::from_files(&dir);
        let file = program.file_id(&dir.join("Foo.cls")).unwrap();
        let found = narrowing_candidates_in_file(&program, file);
        std::fs::remove_dir_all(&dir).ok();
        found
    }

    fn names_and_required(candidates: &[NarrowingCandidate]) -> Vec<(String, Visibility)> {
        candidates
            .iter()
            .map(|c| (c.name.to_string(), c.required))
            .collect()
    }

    #[test]
    fn zero_reference_public_method_is_not_flagged_defers_to_dead_code() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        let found = candidates("zero-reference-public", src);
        assert!(
            found.is_empty(),
            "a zero-reference candidate is dead_code_diagnostics's territory, not this diagnostic's: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn public_method_used_only_within_declaring_class_narrows_to_private() {
        let src = "public class Foo {\n    public void helper() { }\n    public void run() { helper(); }\n}\n";
        let found = candidates("public-same-class", src);
        assert_eq!(
            names_and_required(&found),
            vec![("helper".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn public_method_used_by_subclass_narrows_to_protected() {
        let src = "public virtual class Foo {\n    public void helper() { }\n}\n";
        let subclass = "public class Sub extends Foo {\n    public void go() { helper(); }\n}\n";
        let found = candidates_with_extra_files("public-subclass", src, &[("Sub.cls", subclass)]);
        assert_eq!(
            names_and_required(&found),
            vec![("helper".to_string(), Visibility::Protected)]
        );
    }

    #[test]
    fn public_method_used_by_unrelated_class_stays_public_not_flagged() {
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        let caller = "public class Caller {\n    public void go() { new Foo().helper(); }\n}\n";
        let found = candidates_with_extra_files("public-unrelated", src, &[("Caller.cls", caller)]);
        assert!(
            found.is_empty(),
            "referenced from an unrelated type -- must stay Public, not flagged: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn protected_method_used_only_within_declaring_class_narrows_to_private() {
        let src = "public class Foo {\n    protected void helper() { }\n    public void run() { helper(); }\n}\n";
        let found = candidates("protected-same-class", src);
        assert_eq!(
            names_and_required(&found),
            vec![("helper".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn protected_method_used_by_subclass_stays_protected_not_flagged() {
        let src = "public virtual class Foo {\n    protected void helper() { }\n}\n";
        let subclass = "public class Sub extends Foo {\n    public void go() { helper(); }\n}\n";
        let found = candidates_with_extra_files("protected-subclass", src, &[("Sub.cls", subclass)]);
        assert!(
            found.is_empty(),
            "already at the narrowest level real usage proves (Protected), must not be flagged: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn override_method_is_never_flagged_even_if_narrowly_used() {
        let src = "public virtual class Foo {\n    \
             public virtual void greet() { } \
             public class Derived extends Foo { \
                 public override void greet() { helper(); } \
                 void helper() { } \
             } \
             public void run() { new Derived().greet(); } \
         }\n";
        let found = candidates("override-not-flagged", src);
        assert!(
            !found.iter().any(|c| c.name == "greet"),
            "an override must never be flagged, regardless of its own narrow reference footprint: {:?}",
            names_and_required(&found)
        );
    }

    /// A real gap found while implementing this diagnostic (ticket 34):
    /// Apex forbids an explicit access modifier on an interface method
    /// entirely -- it's always implicitly `public`. This binder's own
    /// collector defaults an unmodified interface method to `Public`
    /// (correctly, matching real Apex), which without this check would
    /// make the interface's own abstract declaration itself look like a
    /// narrowing candidate whenever a same-class unqualified call resolves
    /// ambiguously against both the concrete override and the abstract
    /// declaration (recording a reference against both) -- narrowing an
    /// interface method is never legal, regardless of how narrowly it
    /// appears used.
    #[test]
    fn interface_own_method_declaration_is_never_a_candidate() {
        let src = "public class Foo implements Foo.Greeter {\n    \
             public String greet() { return 'hi'; }\n    \
             public interface Greeter { String greet(); }\n    \
             public void run() { greet(); }\n\
         }\n";
        let found = candidates("interface-own-decl-not-candidate", src);
        assert!(
            found.is_empty(),
            "neither the concrete override nor the interface's own abstract declaration may be \
             flagged: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn interface_implementing_method_is_never_flagged_even_if_narrowly_used() {
        let src = "public class Foo implements Foo.Greeter {\n    \
             public String greet() { return 'hi'; }\n    \
             public interface Greeter { String greet(); }\n    \
             public void run() { greet(); }\n\
         }\n";
        let found = candidates("interface-not-flagged", src);
        assert!(
            !found.iter().any(|c| c.name == "greet"),
            "a method satisfying an interface contract must never be flagged: {:?}",
            names_and_required(&found)
        );
    }

    /// `Database.Batchable`'s methods have no `SymbolId` (they're never
    /// project-local), so before `implements_stdlib_interface_method` this
    /// would incorrectly narrow `finish` to `Private` on the strength of
    /// its one same-class call -- but Apex forbids a `Database.Batchable`
    /// implementation from declaring `finish` anything but `public`.
    #[test]
    fn stdlib_interface_implementing_method_is_never_flagged_even_if_narrowly_used() {
        let src = "public class Foo implements Database.Batchable<SObject> {\n    \
             public Database.QueryLocator start(Database.BatchableContext bc) { return null; }\n    \
             public void execute(Database.BatchableContext bc, List<SObject> records) { }\n    \
             public void finish(Database.BatchableContext bc) { }\n    \
             public void run() { finish(null); }\n\
         }\n";
        let found = candidates("stdlib-interface-not-flagged", src);
        assert!(
            !found.iter().any(|c| c.name == "finish"),
            "a method satisfying Database.Batchable's contract must never be flagged, even though \
             its only real reference is a narrow same-class call: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn aura_enabled_annotated_public_method_is_never_flagged() {
        let src = "public class Foo {\n    @AuraEnabled\n    public void helper() { }\n    public void run() { helper(); }\n}\n";
        let found = candidates("aura-enabled-not-flagged", src);
        assert!(
            found.is_empty(),
            "a platform-invocation-annotated member must never be flagged, its real reach isn't provable: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn visualforce_referenced_class_member_is_never_flagged() {
        let src = "public class Foo {\n    public void helper() { }\n    public void run() { helper(); }\n}\n";
        let page = "<apex:page controller=\"Foo\">Hello</apex:page>";
        let found = candidates_with_extra_files("vf-not-flagged", src, &[("Foo.page", page)]);
        assert!(
            found.is_empty(),
            "a Visualforce-referenced class's members must never be flagged: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn platform_invoked_test_method_is_never_flagged() {
        let src = "public class Foo {\n    \
             @isTest\n    public static void helperAssertion() { System.assert(true); }\n    \
             @isTest\n    public static void testSomething() { helperAssertion(); }\n\
         }\n";
        let found = candidates("test-method-not-flagged", src);
        assert!(
            !found.iter().any(|c| c.name == "helperAssertion"),
            "a platform-invoked test method must never be flagged even with a real narrow reference footprint: {:?}",
            names_and_required(&found)
        );
    }

    /// The key correctness fix ticket 32's research found: a reference
    /// from a same-file nested class must be treated as same-family (top-
    /// level identity), not as an unrelated type (literal container
    /// identity) -- confirmed empirically via a real connected org that
    /// unqualified static cross-nested-type access compiles.
    #[test]
    fn same_file_nested_class_reference_narrows_to_private() {
        let src = "public class Foo {\n    \
             public class Nested {\n        public void go() { helperStatic(); }\n    }\n    \
             public static void helperStatic() { }\n\
         }\n";
        let found = candidates("nested-same-family", src);
        assert_eq!(
            names_and_required(&found),
            vec![("helperStatic".to_string(), Visibility::Private)],
            "a same-file nested-class reference is same-family, must narrow to Private, not stay Public"
        );
    }

    #[test]
    fn public_field_used_only_within_declaring_class_narrows_to_private() {
        let src = "public class Foo {\n    public Integer x;\n    public void run() { x = 1; }\n}\n";
        let found = candidates("public-field-same-class", src);
        assert_eq!(
            names_and_required(&found),
            vec![("x".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn public_property_used_only_within_declaring_class_narrows_to_private() {
        let src = "public class Foo {\n    public Integer x { get; set; }\n    public void run() { x = 1; }\n}\n";
        let found = candidates("public-property-same-class", src);
        assert_eq!(
            names_and_required(&found),
            vec![("x".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn public_constructor_used_only_within_declaring_class_narrows_to_private() {
        let src = "public class Foo {\n    public Foo() { }\n    public static Foo make() { return new Foo(); }\n}\n";
        let found = candidates("public-ctor-same-class", src);
        assert_eq!(
            names_and_required(&found),
            vec![("Foo".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn private_and_global_members_are_never_candidates() {
        let src = "public class Foo {\n    private void a() { }\n    global void b() { }\n    public void run() { a(); b(); }\n}\n";
        let found = candidates("private-global-not-candidates", src);
        assert!(
            found.is_empty(),
            "Private is already narrowest, Global is out of scope: {:?}",
            names_and_required(&found)
        );
    }

    fn type_candidates(test_name: &str, src: &str) -> Vec<NarrowingCandidate> {
        type_candidates_with_extra_files(test_name, src, &[])
    }

    fn type_candidates_with_extra_files(
        test_name: &str,
        src: &str,
        extra_files: &[(&str, &str)],
    ) -> Vec<NarrowingCandidate> {
        let dir = write_fixture(test_name, src, extra_files);
        let program = BoundProgram::from_files(&dir);
        let file = program.file_id(&dir.join("Foo.cls")).unwrap();
        let found = type_narrowing_candidates_in_file(&program, file);
        std::fs::remove_dir_all(&dir).ok();
        found
    }

    #[test]
    fn zero_reference_public_nested_type_is_not_flagged_defers_to_dead_code() {
        let src = "public class Foo {\n    public class Nested { }\n}\n";
        let found = type_candidates("zero-reference-nested", src);
        assert!(
            found.is_empty(),
            "a zero-reference nested type is dead_code_diagnostics's territory: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn public_nested_class_used_only_within_declaring_top_level_narrows_to_private() {
        let src = "public class Foo {\n    \
             public class Nested { }\n    \
             public void run() { Nested n = new Nested(); }\n\
         }\n";
        let found = type_candidates("nested-type-same-family", src);
        assert_eq!(
            names_and_required(&found),
            vec![("Nested".to_string(), Visibility::Private)]
        );
    }

    #[test]
    fn public_nested_class_referenced_by_unrelated_class_stays_public_not_flagged() {
        let src = "public class Foo {\n    public class Nested { }\n}\n";
        let caller = "public class Caller {\n    public void go() { Foo.Nested n = new Foo.Nested(); }\n}\n";
        let found = type_candidates_with_extra_files("nested-unrelated", src, &[("Caller.cls", caller)]);
        assert!(
            found.is_empty(),
            "referenced from an unrelated type -- must stay Public, not flagged: {:?}",
            names_and_required(&found)
        );
    }

    /// A real false-positive found via code review and confirmed via a
    /// real `sf` CLI deploy (see `signature_slot_owner`'s own doc
    /// comment): a same-family public accessor whose *declared return
    /// type is the concrete nested type itself* still leaks it to an
    /// external caller that chains a call off the accessor without ever
    /// naming the nested type -- narrowing must not fire here.
    #[test]
    fn public_nested_class_returned_by_a_public_accessor_called_externally_stays_public_not_flagged() {
        let src = "public class Foo {\n    \
             public class Secret {\n        public String greet() { return 'hi'; }\n    }\n    \
             public static Secret make() { return new Secret(); }\n\
         }\n";
        let caller =
            "public class Caller {\n    public void go() { System.debug(Foo.make().greet()); }\n}\n";
        let found = type_candidates_with_extra_files("nested-return-type-leak", src, &[("Caller.cls", caller)]);
        assert!(
            found.is_empty(),
            "Secret leaks through make()'s own externally-called return-type slot, must stay Public: {:?}",
            names_and_required(&found)
        );
    }

    /// The same real leak as the return-type case above, via a public
    /// field instead of a public accessor method -- found via code review
    /// on this ticket's own first fix attempt (`signature_slot_owner`'s
    /// range-match against the whole `FieldDecl` node never matched a
    /// `Field` symbol, whose own `ptr` is its narrower `VarDeclarator`
    /// node instead, so this exact shape silently slipped through).
    #[test]
    fn public_nested_class_exposed_by_a_public_field_used_externally_stays_public_not_flagged() {
        let src = "public class Foo {\n    \
             public class Secret {\n        public String greet() { return 'hi'; }\n    }\n    \
             public static Secret instance = new Secret();\n\
         }\n";
        let caller =
            "public class Caller {\n    public void go() { System.debug(Foo.instance.greet()); }\n}\n";
        let found = type_candidates_with_extra_files("nested-field-leak", src, &[("Caller.cls", caller)]);
        assert!(
            found.is_empty(),
            "Secret leaks through instance's own externally-referenced field type, must stay Public: {:?}",
            names_and_required(&found)
        );
    }

    /// Two fields sharing one `FieldDecl` (`public Secret a, b;`) share
    /// one declared-type exposure surface -- narrowing must see the leak
    /// even though only the *second* declarator (`b`) is ever referenced
    /// externally.
    #[test]
    fn public_nested_class_exposed_by_either_of_two_co_declared_fields_stays_public_not_flagged() {
        let src = "public class Foo {\n    \
             public class Secret {\n        public String greet() { return 'hi'; }\n    }\n    \
             public static Secret a = new Secret(), b = new Secret();\n\
         }\n";
        let caller = "public class Caller {\n    public void go() { System.debug(Foo.b.greet()); }\n}\n";
        let found = type_candidates_with_extra_files("nested-field-leak-co-declared", src, &[("Caller.cls", caller)]);
        assert!(
            found.is_empty(),
            "Secret leaks through b's own externally-referenced field type, shared with a: {:?}",
            names_and_required(&found)
        );
    }

    /// The same shape as the leak above, except `make()` itself has no
    /// external caller -- narrowing `Secret` is genuinely safe today,
    /// since no currently-compiling code depends on `make()`'s return
    /// type being externally visible.
    #[test]
    fn public_nested_class_returned_by_a_public_accessor_used_only_internally_narrows_to_private() {
        let src = "public class Foo {\n    \
             public class Secret {\n        public String greet() { return 'hi'; }\n    }\n    \
             public static Secret make() { return new Secret(); }\n    \
             public void run() { make().greet(); }\n\
         }\n";
        let found = type_candidates("nested-return-type-internal-only", src);
        assert_eq!(
            names_and_required(&found),
            vec![("Secret".to_string(), Visibility::Private)],
            "make() is never called externally, so Secret's own narrow usage is still provably safe: {:?}",
            names_and_required(&found)
        );
    }

    /// A public subclass of a to-be-narrowed private base class was
    /// confirmed, via a real `sf` CLI deploy, *not* to leak the base
    /// type's visibility requirement the way a return-type slot does --
    /// an external caller can `new Foo.Sub()` and call an inherited
    /// method without ever needing `Secret`'s own visibility. Must stay
    /// out of `signature_slot_owner`'s scope (see its own doc comment).
    #[test]
    fn public_subclass_of_a_narrowable_base_class_used_externally_does_not_block_narrowing() {
        let src = "public class Foo {\n    \
             public virtual class Secret {\n        public String greet() { return 'hi'; }\n    }\n    \
             public class Sub extends Secret { }\n\
         }\n";
        let caller =
            "public class Caller {\n    public void go() { System.debug(new Foo.Sub().greet()); }\n}\n";
        let found = type_candidates_with_extra_files("nested-extends-no-leak", src, &[("Caller.cls", caller)]);
        assert_eq!(
            names_and_required(&found),
            vec![("Secret".to_string(), Visibility::Private)],
            "extends does not leak Secret's visibility requirement to Sub's external callers: {:?}",
            names_and_required(&found)
        );
    }

    /// Ticket 36's locked reasoning: dispatch through a publicly-visible
    /// interface/base type never names the concrete nested type externally,
    /// so the two-way check already handles the bypass case for free, with
    /// no interface-aware logic required.
    #[test]
    fn public_nested_class_reachable_only_via_public_interface_narrows_to_private() {
        let src = "public class Foo {\n    \
             public interface Greeter { String greet(); }\n    \
             public class Secret implements Foo.Greeter {\n        public String greet() { return 'hi'; }\n    }\n    \
             public static Foo.Greeter make() { return new Secret(); }\n\
         }\n";
        let caller = "public class Caller {\n    \
             public void go() { Foo.Greeter g = Foo.make(); g.greet(); }\n\
         }\n";
        let found = type_candidates_with_extra_files("nested-interface-bypass", src, &[("Caller.cls", caller)]);
        assert_eq!(
            names_and_required(&found),
            vec![("Secret".to_string(), Visibility::Private)],
            "dispatch through the public interface never names Secret externally, so it must narrow: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn private_and_global_nested_types_are_never_candidates() {
        let src = "public class Foo {\n    \
             private class A { }\n    global class B { }\n    \
             public void run() { A a = new A(); B b = new B(); }\n\
         }\n";
        let found = type_candidates("private-global-nested-not-candidates", src);
        assert!(
            found.is_empty(),
            "Private is already narrowest, Global is out of scope: {:?}",
            names_and_required(&found)
        );
    }

    #[test]
    fn top_level_public_class_is_never_a_type_candidate() {
        let src = "public class Foo {\n    public void run() { }\n}\n";
        let found = type_candidates("top-level-not-candidate", src);
        assert!(
            found.is_empty(),
            "a top-level type has no container, must never be a candidate: {:?}",
            names_and_required(&found)
        );
    }

    /// Real-corpus smoke test, matching `dead_code.rs`'s own
    /// `npsp_corpus_dead_symbol_sweep_stays_conservative` precedent: must
    /// run to completion without panicking across the real NPSP checkout,
    /// and stay conservative in aggregate.
    #[test]
    fn npsp_corpus_narrowing_sweep_stays_conservative() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus")
            .join("npsp");
        let Ok(root) = root.canonicalize() else {
            eprintln!("skipping: real NPSP corpus checkout not present at {root:?}");
            return;
        };
        let program = BoundProgram::from_files(&root);
        let candidate_count = program
            .symbols
            .iter()
            .filter(|(_, s)| is_narrowing_candidate_kind(&program, s))
            .count();
        let files: HashSet<FileId> = program.symbols.iter().map(|(_, s)| s.file).collect();
        let narrowing_count: usize = files
            .iter()
            .map(|&file| narrowing_candidates_in_file(&program, file).len())
            .sum();
        assert!(
            candidate_count > 0,
            "expected at least some eligible public/protected members in a real corpus this size"
        );
        let ratio = narrowing_count as f64 / candidate_count as f64;
        assert!(
            ratio < 0.5,
            "flagged {narrowing_count}/{candidate_count} ({:.0}%) of eligible members as \
             narrowable -- suspiciously high, likely an overzealous detector rather than a real finding",
            ratio * 100.0
        );
    }

    /// Real-corpus smoke test for the type-level check, mirroring
    /// `npsp_corpus_narrowing_sweep_stays_conservative` above. Ticket 35's
    /// own scan found 684 nested types with an explicit visibility keyword,
    /// 499 already `Public` -- the pre-reference-filter candidate pool this
    /// test's `candidate_count` should land near.
    #[test]
    fn npsp_corpus_type_narrowing_sweep_stays_conservative() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus")
            .join("npsp");
        let Ok(root) = root.canonicalize() else {
            eprintln!("skipping: real NPSP corpus checkout not present at {root:?}");
            return;
        };
        let program = BoundProgram::from_files(&root);
        let candidate_count = program
            .symbols
            .iter()
            .filter(|(_, s)| is_type_narrowing_candidate_kind(s))
            .count();
        let files: HashSet<FileId> = program.symbols.iter().map(|(_, s)| s.file).collect();
        let narrowing_count: usize = files
            .iter()
            .map(|&file| type_narrowing_candidates_in_file(&program, file).len())
            .sum();
        assert!(
            candidate_count > 0,
            "expected at least some eligible public nested types in a real corpus this size"
        );
        let ratio = narrowing_count as f64 / candidate_count as f64;
        assert!(
            ratio < 0.5,
            "flagged {narrowing_count}/{candidate_count} ({:.0}%) of eligible nested types as \
             narrowable -- suspiciously high, likely an overzealous detector rather than a real finding",
            ratio * 100.0
        );
    }
}
