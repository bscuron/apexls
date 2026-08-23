//! A small, hand-written model of `List<T>`/`Map<K, V>`/`Set<T>`'s most
//! common methods, with type-parameter substitution -- see `crate::ty`'s
//! module doc comment for the representation this builds on. Apex has no
//! user-defined generics at all (only these three built-in collection
//! types are generic), so this bounded table genuinely *is* "generics-
//! aware resolution" for Apex, not a partial stand-in for a general
//! generics engine still to come.
//!
//! Not exhaustive -- `List`/`Map`/`Set`'s real APIs have more methods
//! than this covers (`sort`, `addAll`, `retainAll`, `clone`, `iterator`,
//! ...). An unmodeled method just falls through to today's behavior
//! (`Resolution::Unresolved`, same as any other unmodeled system method)
//! -- no worse than before this table existed, just not yet better
//! either.

use crate::ty::Ty;

/// `base`'s (case-insensitively `List`/`Map`/`Set`) `member` method's
/// result type, with `args` (the collection's own type argument(s), as
/// already-resolved `Ty`s) substituted in. `None` for a base name this
/// table doesn't cover at all, or a member of `base` this table doesn't
/// (yet) model -- callers should treat that exactly like any other
/// unresolved member, not as an error.
pub(crate) fn builtin_generic_member_type(base: &str, args: &[Ty], member: &str) -> Option<Ty> {
    let element = args.first().cloned();
    let key = args.first().cloned();
    let value = args.get(1).cloned();
    let member = member.to_ascii_lowercase();
    match base.to_ascii_lowercase().as_str() {
        "list" => match member.as_str() {
            "get" => element,
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "contains" => Some(Ty::boolean()),
            _ => None,
        },
        "map" => match member.as_str() {
            "get" => value,
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "containskey" | "containsvalue" => Some(Ty::boolean()),
            "keyset" => Some(Ty::system_with_args("Set", key.into_iter().collect())),
            "values" => Some(Ty::system_with_args("List", value.into_iter().collect())),
            _ => None,
        },
        "set" => match member.as_str() {
            "size" => Some(Ty::system("Integer")),
            "isempty" => Some(Ty::boolean()),
            "contains" => Some(Ty::boolean()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_get_substitutes_the_element_type() {
        let account = Ty::Project(crate::symbol::SymbolId::new(crate::file_id::FileId(0), 0));
        assert_eq!(
            builtin_generic_member_type("List", std::slice::from_ref(&account), "get"),
            Some(account)
        );
    }

    #[test]
    fn map_keyset_and_values_wrap_the_respective_type_argument() {
        let key = Ty::system("String");
        let value = Ty::system("Integer");
        assert_eq!(
            builtin_generic_member_type("Map", &[key.clone(), value.clone()], "keySet"),
            Some(Ty::system_with_args("Set", vec![key]))
        );
        assert_eq!(
            builtin_generic_member_type("Map", &[Ty::system("String"), value.clone()], "values"),
            Some(Ty::system_with_args("List", vec![value]))
        );
    }

    #[test]
    fn size_and_isempty_are_modeled_case_insensitively_across_all_three_types() {
        for base in ["List", "list", "Map", "Set", "SET"] {
            assert_eq!(
                builtin_generic_member_type(base, &[], "size"),
                Some(Ty::system("Integer")),
                "base = {base}"
            );
            assert_eq!(
                builtin_generic_member_type(base, &[], "isEmpty"),
                Some(Ty::boolean()),
                "base = {base}"
            );
        }
    }

    #[test]
    fn an_unmodeled_base_or_member_returns_none() {
        assert_eq!(builtin_generic_member_type("String", &[], "length"), None);
        assert_eq!(builtin_generic_member_type("List", &[], "sort"), None);
    }
}
