//! Names the REPL completes and lists: builtins from the library's catalog,
//! parser keywords and the REPL's own commands.

use vibescript::Value;

/// The REPL commands Tab completes, long and short forms.
pub const COMMANDS: [&str; 18] = [
    ":help",
    ":h",
    ":vars",
    ":v",
    ":globals",
    ":g",
    ":functions",
    ":f",
    ":types",
    ":t",
    ":clear",
    ":c",
    ":reset",
    ":r",
    ":last_error",
    ":le",
    ":quit",
    ":q",
];

/// The parser's reserved keywords, in the Go REPL's completion order.
pub const KEYWORDS: [&str; 34] = [
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "enum",
    "export", "false", "for", "getter", "if", "in", "next", "nil", "private", "property", "raise",
    "rescue", "retry", "return", "self", "setter", "then", "true", "unless", "until", "when",
    "while", "yield",
];

/// The tooling view of the library's builtins, derived from
/// [`vibescript::builtins`] rather than a parallel table.
#[derive(Debug)]
pub struct Catalog {
    /// Every top-level builtin name, such as `puts` or `JSON`.
    pub top_level: Vec<String>,
    /// Callable builtins, with namespace members qualified as `JSON.parse`.
    pub functions: Vec<String>,
    /// Top-level callables and values plus every qualified namespace member.
    pub documented: Vec<String>,
}

impl Catalog {
    pub fn new() -> Self {
        let mut catalog = Self {
            top_level: Vec::new(),
            functions: Vec::new(),
            documented: Vec::new(),
        };
        for (name, value) in vibescript::builtins() {
            catalog.top_level.push(name.clone());
            if callable(&value) {
                catalog.functions.push(name.clone());
                catalog.documented.push(name);
                continue;
            }
            let Some(members) = value.as_hash().filter(|_| value.type_name() == "object") else {
                catalog.documented.push(name);
                continue;
            };
            for (member, member_value) in members {
                let member = String::from_utf8_lossy(member.as_bytes().unwrap_or_default());
                let qualified = format!("{name}.{member}");
                if callable(member_value) {
                    catalog.functions.push(qualified.clone());
                }
                catalog.documented.push(qualified);
            }
        }
        catalog.top_level.sort();
        catalog.functions.sort();
        catalog.documented.sort();
        catalog
    }
}

/// Reports whether a value can be called: builtins, host methods, functions
/// and classes, as the Go REPL classifies them.
pub fn callable(value: &Value) -> bool {
    matches!(value.type_name(), "builtin" | "function" | "class")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_derives_names_from_the_library() {
        let catalog = Catalog::new();
        for name in ["JSON", "Math", "puts", "require"] {
            assert!(catalog.top_level.iter().any(|n| n == name), "{name}");
        }
        for name in ["JSON.parse_as", "Math.sqrt", "puts", "Time.now"] {
            assert!(catalog.functions.iter().any(|n| n == name), "{name}");
        }
        assert!(!catalog.functions.iter().any(|n| n == "Math.PI"));
        assert!(!catalog.functions.iter().any(|n| n == "JSON"));
        assert!(catalog.documented.iter().any(|n| n == "Math.PI"));
        assert!(catalog.documented.is_sorted());
        assert!(KEYWORDS.is_sorted());
    }
}
