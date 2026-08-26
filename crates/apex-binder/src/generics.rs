//! A small, hand-written model of `List<T>`/`Map<K, V>`/`Set<T>`'s most
//! common methods, with type-parameter substitution -- see `crate::ty`'s
//! module doc comment for the representation this builds on. Apex has no
//! user-defined generics at all (only these three built-in collection
//! types are generic), so this bounded table genuinely *is* "generics-
//! aware resolution" for Apex, not a partial stand-in for a general
//! generics engine still to come.
//!
//! Not exhaustive -- `List`/`Map`/`Set`'s real APIs have more methods
//! than this hand-written table covers directly (`sort`, `addAll`,
//! `retainAll`, ...). Those either aren't generic-relevant at all (they
//! return `Boolean`/`Integer`/`String`/`void`, which the plain stdlib
//! lookup this falls through to already gets right with no substitution
//! needed) or -- `clone`/`deepClone` -- are covered by
//! [`same_type_as_receiver`]'s data-driven fallback instead of a
//! per-method entry here. An unmodeled method just falls through to
//! today's behavior (`Resolution::Unresolved`, same as any other
//! unmodeled system method) -- no worse than before this table existed,
//! just not yet better either.

use crate::ty::Ty;
use apex_stdlib::StdlibClass;

/// `base`'s (case-insensitively `List`/`Map`/`Set`) `member` method's
/// result type, with `args` (the collection's own type argument(s), as
/// already-resolved `Ty`s) substituted in. `class` is `base`'s own
/// already-resolved bundled stdlib entry, if any (the caller,
/// `crate::resolve::bind_method_call_expr`, already looked it up to
/// decide the call's `Resolution` in the first place -- threaded through
/// here rather than re-looked-up, same "don't repeat a lookup the caller
/// already did" pattern as `narrow_stdlib_overload_type`), used only by
/// [`same_type_as_receiver`]. `None` for a base name this table doesn't
/// cover at all, or a member of `base` this table doesn't (yet) model --
/// callers should treat that exactly like any other unresolved member,
/// not as an error.
pub(crate) fn builtin_generic_member_type(
    class: Option<&'static StdlibClass>,
    base: &str,
    args: &[Ty],
    member: &str,
) -> Option<Ty> {
    let element = args.first().cloned();
    let key = args.first().cloned();
    let value = args.get(1).cloned();
    let member_lower = member.to_ascii_lowercase();
    match base.to_ascii_lowercase().as_str() {
        "list" => match member_lower.as_str() {
            "get" => element,
            // `List.remove(Integer)` returns the removed element (`T`),
            // scraped as bare `Object` -- see this module's own doc
            // comment on why that can't be derived from stdlib data.
            "remove" => element,
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "contains" => Some(Ty::boolean()),
            // `Iterator<T>`'s own members aren't modeled anywhere in
            // this crate -- harmless, the same "type known, no member
            // model" situation as a bare `Ty::System` already is
            // everywhere else, just with a real type argument attached
            // instead of none.
            "iterator" => Some(Ty::system_with_args("Iterator", element.into_iter().collect())),
            _ => same_type_as_receiver(class, "List", args, member),
        },
        "map" => match member_lower.as_str() {
            "get" => value,
            // `Map.put(k, v)` returns the *previous* value (`T2`),
            // scraped as bare `Object` -- same reasoning as `List.remove`.
            "put" => value,
            "remove" => value,
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "containskey" | "containsvalue" => Some(Ty::boolean()),
            "keyset" => Some(Ty::system_with_args("Set", key.into_iter().collect())),
            "values" => Some(Ty::system_with_args("List", value.into_iter().collect())),
            _ => same_type_as_receiver(class, "Map", args, member),
        },
        "set" => match member_lower.as_str() {
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "contains" => Some(Ty::boolean()),
            _ => same_type_as_receiver(class, "Set", args, member),
        },
        _ => None,
    }
}

/// Data-driven fallback for a `List`/`Map`/`Set` method this table has
/// no per-method entry for: if `member` is a real method on `class` (the
/// receiver's own bundled stdlib entry) whose scraped return type's base
/// name is `canonical_base` itself (`List.clone`/`List.deepClone` ->
/// `List`, `Map.clone`/`Map.deepClone` -> `Map`, `Set.clone` -> `Set`,
/// ...), the result is the receiver's own type *unchanged*, `args` and
/// all. Unlike `get`/`put`/`keySet`/`values`, this needs no hand-written
/// per-method entry and needs no knowledge of *which* type-parameter
/// position is generic -- "the whole receiver type passes through
/// unchanged" is unambiguous regardless of how many type parameters
/// `canonical_base` has, so it generalizes to any future same-shaped
/// method Salesforce adds, sourced straight from `apex_stdlib`'s scraped
/// signature instead of hand-listed here. This is *not* a case where
/// stdlib data can replace this module's hand-written entries generally
/// (see the module's own doc comment): Salesforce's reference docs are
/// fully type-erased (`List.get` scrapes as returning bare `Object`, not
/// `T`), so this only works because "same as receiver" needs no position
/// information at all -- every other substitution shape still does.
fn same_type_as_receiver(
    class: Option<&'static StdlibClass>,
    canonical_base: &'static str,
    args: &[Ty],
    member: &str,
) -> Option<Ty> {
    let stdlib_method = class?.methods.iter().find(|m| m.name.eq_ignore_ascii_case(member))?;
    let return_type = stdlib_method.return_type.as_deref()?;
    let (return_base, _) = apex_stdlib::split_generic_type(return_type);
    if return_base.eq_ignore_ascii_case(canonical_base) {
        Some(Ty::system_with_args(canonical_base, args.to_vec()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real bundled stdlib entry for `System.List`/`System.Map`/
    /// `System.Set` -- needed for [`same_type_as_receiver`]'s tests,
    /// which check against real scraped return-type strings, not a
    /// synthetic fixture.
    fn stdlib_class(name: &str) -> &'static StdlibClass {
        apex_stdlib::standard_classes()
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name) && c.namespace.as_deref() == Some("System"))
            .unwrap_or_else(|| panic!("{name} should be in the bundled stdlib snapshot"))
    }

    #[test]
    fn list_get_substitutes_the_element_type() {
        let account = Ty::Project(crate::symbol::SymbolId::new(crate::file_id::FileId(0), 0));
        assert_eq!(
            builtin_generic_member_type(None, "List", std::slice::from_ref(&account), "get"),
            Some(account)
        );
    }

    #[test]
    fn map_keyset_and_values_wrap_the_respective_type_argument() {
        let key = Ty::system("String");
        let value = Ty::system("Integer");
        assert_eq!(
            builtin_generic_member_type(None, "Map", &[key.clone(), value.clone()], "keySet"),
            Some(Ty::system_with_args("Set", vec![key]))
        );
        assert_eq!(
            builtin_generic_member_type(None, "Map", &[Ty::system("String"), value.clone()], "values"),
            Some(Ty::system_with_args("List", vec![value]))
        );
    }

    #[test]
    fn size_and_isempty_are_modeled_case_insensitively_across_all_three_types() {
        for base in ["List", "list", "Map", "Set", "SET"] {
            assert_eq!(
                builtin_generic_member_type(None, base, &[], "size"),
                Some(Ty::system("Integer")),
                "base = {base}"
            );
            assert_eq!(
                builtin_generic_member_type(None, base, &[], "isEmpty"),
                Some(Ty::boolean()),
                "base = {base}"
            );
        }
    }

    #[test]
    fn an_unmodeled_base_or_member_returns_none() {
        assert_eq!(builtin_generic_member_type(None, "String", &[], "length"), None);
        // No `class` provided (as if the receiver's stdlib class couldn't
        // be resolved at all) -- `same_type_as_receiver` must not panic,
        // just honestly return `None`, same as any other unmodeled case.
        assert_eq!(builtin_generic_member_type(None, "List", &[], "sort"), None);
    }

    #[test]
    fn list_remove_substitutes_the_element_type() {
        let account = Ty::system("Account");
        assert_eq!(
            builtin_generic_member_type(
                Some(stdlib_class("List")),
                "List",
                std::slice::from_ref(&account),
                "remove"
            ),
            Some(account)
        );
    }

    #[test]
    fn list_iterator_wraps_the_element_type() {
        let account = Ty::system("Account");
        assert_eq!(
            builtin_generic_member_type(
                Some(stdlib_class("List")),
                "List",
                std::slice::from_ref(&account),
                "iterator"
            ),
            Some(Ty::system_with_args("Iterator", vec![account]))
        );
    }

    #[test]
    fn map_put_and_remove_substitute_the_value_type() {
        let key = Ty::system("String");
        let value = Ty::system("Account");
        for member in ["put", "remove"] {
            assert_eq!(
                builtin_generic_member_type(
                    Some(stdlib_class("Map")),
                    "Map",
                    &[key.clone(), value.clone()],
                    member
                ),
                Some(value.clone()),
                "member = {member}"
            );
        }
    }

    /// `List.clone`/`List.deepClone` both scrape as returning bare
    /// `List` (Salesforce's own reference docs are type-erased) -- the
    /// data-driven fallback should recognize both without either being
    /// hand-listed by name, and preserve the receiver's own type
    /// argument.
    #[test]
    fn list_clone_and_deepclone_return_the_receivers_own_type_via_the_stdlib_fallback() {
        let account = Ty::system("Account");
        for member in ["clone", "deepClone"] {
            assert_eq!(
                builtin_generic_member_type(
                    Some(stdlib_class("List")),
                    "List",
                    std::slice::from_ref(&account),
                    member
                ),
                Some(Ty::system_with_args("List", vec![account.clone()])),
                "member = {member}"
            );
        }
    }

    #[test]
    fn map_and_set_clone_return_the_receivers_own_type_via_the_stdlib_fallback() {
        let key = Ty::system("String");
        let value = Ty::system("Account");
        assert_eq!(
            builtin_generic_member_type(Some(stdlib_class("Map")), "Map", &[key.clone(), value.clone()], "clone"),
            Some(Ty::system_with_args("Map", vec![key.clone(), value.clone()]))
        );
        assert_eq!(
            builtin_generic_member_type(Some(stdlib_class("Set")), "Set", &[value.clone()], "clone"),
            Some(Ty::system_with_args("Set", vec![value]))
        );
    }

    /// A method that exists on the real stdlib class but doesn't return
    /// the receiver's own type (`sort` returns `void`) must not trip the
    /// data-driven fallback into a wrong guess.
    #[test]
    fn a_non_self_returning_stdlib_method_does_not_trigger_the_fallback() {
        assert_eq!(
            builtin_generic_member_type(Some(stdlib_class("List")), "List", &[], "sort"),
            None
        );
    }
}
