//! The tooling view of the runtime's builtin globals.

use std::collections::HashSet;
use std::sync::OnceLock;

/// The catalog of the runtime's builtins, built once.
pub(crate) fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(Catalog::new)
}

/// The removed callable constructors. Scripts can still name them, and doing
/// so explains the removal, so like the reference the catalog offers them as
/// builtin functions, although [`vibescript::builtins`] leaves them out.
const REMOVED_CONSTRUCTORS: [&str; 3] = ["Proc", "lambda", "proc"];

/// Builtin names classified for completion, hover and signature help.
pub(crate) struct Catalog {
    /// Every global name, sorted.
    pub top_level_names: Vec<String>,
    /// Callable globals and callable namespace members (`Math.sqrt`), sorted.
    pub function_names: Vec<String>,
    /// Names the builtin reference must document, sorted.
    pub documented_names: Vec<String>,
    builtins: HashSet<String>,
    functions: HashSet<String>,
    namespaces: HashSet<String>,
}

impl Catalog {
    pub(crate) fn new() -> Self {
        let mut catalog = Self {
            top_level_names: Vec::new(),
            function_names: Vec::new(),
            documented_names: Vec::new(),
            builtins: HashSet::new(),
            functions: HashSet::new(),
            namespaces: HashSet::new(),
        };
        let removed = REMOVED_CONSTRUCTORS
            .iter()
            .map(|name| ((*name).to_owned(), None));
        let globals = vibescript::builtins()
            .into_iter()
            .map(|(name, value)| (name, Some(value)));
        for (name, value) in globals.chain(removed) {
            catalog.top_level_names.push(name.clone());
            catalog.builtins.insert(name.clone());
            let members = value.as_ref().and_then(vibescript::Value::as_hash);
            let Some(members) = members else {
                catalog.documented_names.push(name.clone());
                if value.is_none_or(|value| value.type_name() == "builtin") {
                    catalog.add_function(name);
                }
                continue;
            };
            catalog.namespaces.insert(name.clone());
            for (member, value) in members {
                let member = String::from_utf8_lossy(member.as_bytes().unwrap_or_default());
                let qualified = format!("{name}.{member}");
                catalog.documented_names.push(qualified.clone());
                if value.type_name() == "builtin" {
                    catalog.add_function(qualified);
                }
            }
        }
        catalog.top_level_names.sort();
        catalog.function_names.sort();
        catalog.documented_names.sort();
        catalog
    }

    fn add_function(&mut self, name: String) {
        self.functions.insert(name.clone());
        self.function_names.push(name);
    }

    pub(crate) fn is_builtin(&self, name: &str) -> bool {
        self.builtins.contains(name)
    }

    pub(crate) fn is_function(&self, name: &str) -> bool {
        self.functions.contains(name)
    }

    pub(crate) fn is_namespace(&self, name: &str) -> bool {
        self.namespaces.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_runtime_registry() {
        let catalog = Catalog::new();
        for names in [
            &catalog.top_level_names,
            &catalog.function_names,
            &catalog.documented_names,
        ] {
            assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        for (name, builtin, function, namespace) in [
            ("format", true, true, false),
            ("Math", true, false, true),
            ("Math.sqrt", false, true, false),
            ("Math.PI", false, false, false),
            ("not_a_builtin", false, false, false),
        ] {
            assert_eq!(catalog.is_builtin(name), builtin, "{name}");
            assert_eq!(catalog.is_function(name), function, "{name}");
            assert_eq!(catalog.is_namespace(name), namespace, "{name}");
        }
        assert!(
            catalog
                .documented_names
                .iter()
                .any(|name| name == "Math.PI")
        );
    }
}
