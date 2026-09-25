//! Documentation parsing and drift gates.

use super::super::docs::{
    CONTEXTUAL_WORDS, UNIVERSAL, builtin_docs, keyword_doc, keyword_doc_words, member_doc_markdown,
    member_docs, parse_builtin_docs, runtime_members,
};
use super::*;
use std::collections::HashSet;

#[test]
fn builtin_docs_parse_headings_and_qualified_bullets() {
    let markdown = concat!(
        "# Reference\n\n",
        "## Formatting\n\n",
        "### `format(pattern, *values)` / `sprintf(pattern, *values)`\n\n",
        "Formats values with percent strings.\nSecond line of the paragraph.\n\n",
        "```vibe\nformat(\"%d\", 1)\n# `fenced(code)` must not register entries\n```\n\n",
        "Output is capped.\n\n",
        "### Constants\n\n",
        "- `Math::PI` – the circle constant.\n",
        "- `Math.hypot(x, y)` / `Math.atan2(y, x)` – two-argument helpers\n",
        "  spanning a continuation line.\n",
        "- prose bullet with `inline(code)` that must not register.\n\n",
        "### `Hash.new { |hash, key| ... }`\n\n",
        "Builds a hash with a default proc.\n",
    );
    let entries = parse_builtin_docs(markdown);
    let format = &entries["format"];
    assert_eq!(format.signature, "`format(pattern, *values)`");
    assert_eq!(
        format.markdown,
        "`format(pattern, *values)` / `sprintf(pattern, *values)`\n\nFormats values with percent strings.\nSecond line of the paragraph.\n\nOutput is capped."
    );
    assert_eq!(entries["sprintf"].signature, "`sprintf(pattern, *values)`");
    assert_eq!(entries["sprintf"].markdown, format.markdown);
    assert_eq!(
        entries["Math.PI"].markdown,
        "`Math::PI`\n\nthe circle constant."
    );
    assert!(
        entries["Math.hypot"]
            .markdown
            .contains("spanning a continuation line.")
    );
    assert_eq!(
        entries["Math.atan2"].markdown,
        entries["Math.hypot"].markdown
    );
    assert_eq!(
        entries["Hash.new"].signature,
        "`Hash.new { |hash, key| ... }`"
    );
    let mut names: Vec<&str> = entries.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "Hash.new",
            "Math.PI",
            "Math.atan2",
            "Math.hypot",
            "format",
            "sprintf"
        ]
    );
}

/// Every global and namespace member is documented, and the documentation
/// has no entries for builtins the runtime lacks.
#[test]
fn builtin_docs_match_the_registered_builtins() {
    let docs = builtin_docs();
    assert!(!docs.is_empty());
    let catalog = Catalog::new();
    let registered: HashSet<&str> = catalog
        .documented_names
        .iter()
        .map(String::as_str)
        .collect();
    let missing: Vec<&&str> = registered
        .iter()
        .filter(|name| !docs.contains_key(**name))
        .collect();
    assert!(missing.is_empty(), "undocumented builtins: {missing:?}");
    let mut stale: Vec<&String> = docs
        .keys()
        .filter(|name| !registered.contains(name.as_str()))
        .collect();
    stale.sort();
    assert!(
        stale.is_empty(),
        "documented builtins the runtime lacks: {stale:?}"
    );
}

#[test]
fn keyword_docs_cover_the_parser_keywords() {
    for keyword in vibescript::tooling::keywords() {
        let doc = keyword_doc(keyword).unwrap_or_else(|| panic!("{keyword} has no description"));
        assert!(!doc.is_empty() && doc.len() <= 130, "{keyword}: {doc}");
    }
    for word in CONTEXTUAL_WORDS {
        assert!(keyword_doc(word).is_some(), "{word}");
    }
    assert_eq!(
        keyword_doc_words().len(),
        vibescript::tooling::keywords().len() + CONTEXTUAL_WORDS.len()
    );
    assert!(keyword_doc("puts").is_none());
}

/// Every parsed member entry names a member the runtime dispatches for its
/// receiver, one canary per parsed section guards against a dropped source,
/// and most runtime members are documented.
#[test]
fn member_docs_match_the_runtime_members() {
    let docs = member_docs();
    let runtime = runtime_members();
    for (receiver, name) in [
        ("string", "upcase"),
        ("array", "map"),
        ("array", "take"),
        ("hash", "fetch"),
        ("hash", "transform_keys"),
        ("int", "times"),
        ("float", "nan?"),
        ("money", "cents"),
        ("duration", "ago"),
        ("time", "strftime"),
        ("symbol", "id2name"),
        ("range", "cover?"),
        ("regex", "source"),
    ] {
        assert!(
            docs.entries
                .get(name)
                .is_some_and(|entries| entries.iter().any(|entry| entry.receiver == receiver)),
            "lost {receiver}.{name}"
        );
    }
    for name in ["itself", "tap", "eql?", "respond_to?"] {
        assert!(docs.universal.contains_key(name), "lost universal {name}");
    }
    for name in docs.universal.keys() {
        for (receiver, members) in runtime {
            assert!(
                members.contains(&name.as_str()),
                "universal {name} is not on {receiver}"
            );
        }
    }
    for name in ["to_s", "string"] {
        assert!(!docs.universal.contains_key(name), "{name}");
        let markdown = member_doc_markdown(name);
        assert!(!markdown.is_empty(), "{name}");
        let mut bodies = HashSet::new();
        for section in markdown.split("\n\n---\n\n") {
            let body = section.split_once("\n\n").map_or(section, |(_, body)| body);
            assert!(bodies.insert(body), "{name} repeats a section: {markdown}");
        }
    }
    for name in ["strip!", "lstrip!"] {
        assert!(
            member_doc_markdown(name).contains("In-place variant of"),
            "{name}"
        );
    }
    assert!(member_doc_markdown("sub!").contains("never matched"));

    let union: HashSet<&str> = runtime.values().flatten().copied().collect();
    for (name, entries) in &docs.entries {
        for entry in entries {
            let members = runtime
                .get(entry.receiver.as_str())
                .unwrap_or_else(|| panic!("{}.{name} names an unknown receiver", entry.receiver));
            assert!(
                members.contains(&name.as_str()),
                "{}.{name} does not exist",
                entry.receiver
            );
        }
    }
    for name in docs.universal.keys() {
        assert!(union.contains(name.as_str()), "{name}");
    }
    let (mut total, mut documented) = (0, 0);
    for (receiver, members) in runtime {
        for member in members {
            total += 1;
            let universal = docs.universal.contains_key(*member);
            if universal
                || docs
                    .entries
                    .get(*member)
                    .is_some_and(|entries| entries.iter().any(|entry| entry.receiver == *receiver))
            {
                documented += 1;
            }
        }
    }
    assert!(
        documented * 4 >= total * 3,
        "member doc coverage {documented}/{total}"
    );
    assert_eq!(UNIVERSAL, "universal");
}

/// Signatures written as in the builtin table name their entries through
/// their type parameters, as in `map<U>(...)`.
#[test]
fn doc_signatures_may_declare_type_parameters() {
    let entries = parse_builtin_docs(
        "## JSON\n\n### `JSON.parse_as<T>(text: string, schema: type<T>) -> T`\n\nParses and checks a shape.\n",
    );
    assert_eq!(
        entries["JSON.parse_as"].signature,
        "`JSON.parse_as<T>(text: string, schema: type<T>) -> T`"
    );
    let map = &member_docs().entries["map"];
    assert!(
        map.iter()
            .any(|entry| entry.receiver == "array" && entry.signature.starts_with("`map<U>(")),
        "{map:?}"
    );
}
