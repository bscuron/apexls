//! [`apex_stdlib::StdlibClass`] wrapped into a lowercase-keyed lookup
//! index, mirroring `crate::schema_index::SchemaIndex`'s own shape --
//! Apex class/member names, like Salesforce API names, are case-
//! insensitive for lookup purposes.

use crate::ci_key::{CiKey, CiMap, CiQuery};
use apex_stdlib::{StdlibClass, StdlibMethod, StdlibProperty};

pub struct StdlibIndex {
    /// More than one entry only for a real namespace collision (7
    /// confirmed in the whole real corpus, e.g. `Test` existing in both
    /// `Canvas` and `System`) -- see [`StdlibIndex::class`] for how a
    /// collision is broken.
    classes: CiMap<Vec<&'static StdlibClass>>,
}

impl StdlibIndex {
    pub fn new() -> Self {
        let mut classes: CiMap<Vec<&'static StdlibClass>> = CiMap::default();
        for class in apex_stdlib::standard_classes() {
            classes
                .entry(CiKey::from(class.name.as_str()))
                .or_default()
                .push(class);
        }
        StdlibIndex { classes }
    }

    /// A bare `Ty::System` name (e.g. `"String"`) never carries its own
    /// namespace -- real Apex code writes `Test`, not `System.Test` --
    /// so a collision is broken by preferring the `System`-namespaced
    /// entry (the default, implicitly-available namespace, and the one
    /// a bare unqualified name overwhelmingly means in practice), else
    /// falling back to whichever entry was scraped first. A genuinely
    /// ambiguous non-`System` collision (none exist in the real corpus
    /// today) would silently pick one -- an accepted, rare, documented
    /// limitation, the same tradeoff `relationship_field_api_name`'s own
    /// doc comment already accepts for a similar non-default-namespace
    /// edge case.
    pub fn class(&self, name: &str) -> Option<&'static StdlibClass> {
        let candidates = self.classes.get(&CiQuery(name))?;
        candidates
            .iter()
            .find(|c| c.namespace.as_deref() == Some("System"))
            .or_else(|| candidates.first())
            .copied()
    }

    /// The first method named `member` on `class_name`, if any -- an
    /// existence check, not overload resolution (see
    /// [`Self::methods`] for every overload).
    pub fn method(&self, class_name: &str, member: &str) -> Option<&'static StdlibMethod> {
        self.methods(class_name, member).next()
    }

    /// Every overload of `member` on `class_name`, in scraped order.
    pub fn methods<'a>(
        &'a self,
        class_name: &str,
        member: &'a str,
    ) -> impl Iterator<Item = &'static StdlibMethod> + 'a {
        self.class(class_name)
            .into_iter()
            .flat_map(|c| c.methods.iter())
            .filter(move |m| m.name.eq_ignore_ascii_case(member))
    }

    pub fn property(&self, class_name: &str, member: &str) -> Option<&'static StdlibProperty> {
        self.class(class_name)?
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(member))
    }
}

impl Default for StdlibIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_static_method_is_found_case_insensitively() {
        let index = StdlibIndex::new();
        let method = index.method("string", "ISBLANK").expect("String.isBlank should be found");
        assert!(method.is_static);
        assert_eq!(method.return_type.as_deref(), Some("Boolean"));
    }

    #[test]
    fn an_unmodeled_member_returns_none() {
        let index = StdlibIndex::new();
        assert!(index.method("String", "definitelyNotARealMethod").is_none());
        assert!(index.class("DefinitelyNotARealClass").is_none());
    }

    #[test]
    fn a_namespace_collision_prefers_the_system_entry() {
        let index = StdlibIndex::new();
        let test_class = index.class("Test").expect("Test should resolve to one entry");
        assert_eq!(test_class.namespace.as_deref(), Some("System"));
        // System.Test has many more methods than Canvas.Test (31 vs 2 in
        // the real corpus) -- a second, independent confirmation this
        // picked the right one, not just the right namespace label.
        assert!(test_class.methods.len() > 10);
    }

    #[test]
    fn every_overload_of_an_overloaded_method_is_returned() {
        let index = StdlibIndex::new();
        let overloads: Vec<_> = index.methods("Database", "query").collect();
        assert_eq!(overloads.len(), 2, "expected both Database.query overloads");
    }
}
