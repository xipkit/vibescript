//! Completion items and the script-local completion index.

use super::*;

#[test]
fn completion_items_are_sorted_and_categorized() {
    let items = static_items();
    let labels: Vec<&str> = items
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert!(
        labels.windows(2).all(|pair| pair[0] <= pair[1]),
        "{labels:?}"
    );
    let keyword = find(&items, "if");
    assert_eq!(
        (keyword["detail"].as_str(), keyword["kind"].as_i64()),
        (Some("keyword"), Some(14))
    );
    let builtin = find(&items, "assert");
    assert_eq!(builtin["detail"], builtin_signature("assert"));
    assert_eq!(builtin["kind"], 3);
    let namespace = find(&items, "JSON");
    assert_eq!(
        (namespace["detail"].as_str(), namespace["kind"].as_i64()),
        (Some("namespace"), Some(9))
    );
    assert!(
        namespace["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("Members: `parse`")
    );
}

#[test]
fn keyword_completions_are_the_parser_keywords() {
    let items = static_items();
    let keywords: Vec<&str> = items
        .iter()
        .filter(|item| item["detail"] == "keyword")
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(keywords, vibescript::tooling::keywords());
    let require = find(&items, "require");
    assert_eq!(require["detail"], builtin_signature("require"));
    assert_eq!(require["kind"], 3);
}

#[test]
fn completion_items_carry_documentation() {
    let items = static_items();
    let puts = find(&items, "puts");
    assert_eq!(puts["documentation"]["kind"], "markdown");
    assert!(
        puts["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("Writes each value")
    );
    assert_eq!(puts["detail"], builtin_signature("puts"));
    let keyword = find(&items, "if");
    assert!(
        keyword["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains(docs::keyword_doc("if").unwrap())
    );
}

#[test]
fn every_builtin_has_a_completion_and_every_function_a_signature() {
    let catalog = Catalog::new();
    let items = static_items();
    for name in &catalog.top_level_names {
        find(&items, name);
    }
    let missing: Vec<&String> = catalog
        .function_names
        .iter()
        .filter(|name| {
            docs::builtin_docs()
                .get(*name)
                .is_none_or(|doc| doc.signature.is_empty())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "functions without documented signatures: {missing:?}"
    );
}

#[test]
fn the_completion_index_is_built_lazily() {
    let mut server = server();
    let uri = "file:///tmp/lazy.vibe";
    open(
        &mut server,
        uri,
        "def helper(value)\n  value\nend\n\ndef run()\n  helper(1)\nend\n",
    );
    assert!(document(&server, uri).completion.get().is_none());
    let labels = completion_labels(&mut server, uri, 5, 2);
    assert!(labels.contains_key("helper"));
    assert!(document(&server, uri).completion.get().is_some());
}

#[test]
fn completion_after_a_dot_offers_member_methods() {
    let mut server = server();
    let uri = "file:///tmp/members.vibe";
    open(&mut server, uri, "def run()\n  \"abc\".\nend\n");
    let labels = completion_labels(&mut server, uri, 1, 8);
    let upcase = &labels["upcase"];
    assert_eq!(upcase["kind"], 2);
    assert!(upcase["detail"].as_str().unwrap().contains("string"));
    for absent in ["def", "flatten", "cents"] {
        assert!(!labels.contains_key(absent), "{absent}");
    }
    open(&mut server, uri, "def run()\n  \"abc\".upc\nend\n");
    assert!(completion_labels(&mut server, uri, 1, 11).contains_key("upcase"));
}

#[test]
fn completion_offers_functions_params_and_locals() {
    let mut server = server();
    let uri = "file:///tmp/scope.vibe";
    open(
        &mut server,
        uri,
        "def helper(amount)\n  doubled = amount * 2\n  doubled\nend\n\ndef run()\n  total = helper(2)\n  total\nend\n",
    );
    let labels = completion_labels(&mut server, uri, 1, 2);
    for (label, detail) in [
        ("helper", "function".to_owned()),
        ("run", "function".to_owned()),
        ("amount", "parameter".to_owned()),
        ("doubled", "local".to_owned()),
        ("if", "keyword".to_owned()),
        ("assert", builtin_signature("assert")),
    ] {
        assert_eq!(labels[label]["detail"], detail, "{label}");
    }
    assert!(!labels.contains_key("total"));
    assert!(completion_labels(&mut server, uri, 6, 2).contains_key("total"));
}

#[test]
fn completion_offers_locals_from_every_statement_shape() {
    let mut server = server();
    let uri = "file:///tmp/locals.vibe";
    open(
        &mut server,
        uri,
        "def run()\n  first, *rest, last = [1, 2, 3]\n  nested, (left, right) = [4, [5, 6]]\n  first\nend\n",
    );
    let labels = completion_labels(&mut server, uri, 3, 2);
    for want in ["first", "rest", "last", "nested", "left", "right"] {
        assert!(labels.contains_key(want), "{want}");
    }
    open(
        &mut server,
        uri,
        "def run()\n  begin\n    value = 1\n  rescue\n    fallback = 2\n  else\n    from_else = value\n  end\n  from_else\nend\n",
    );
    let labels = completion_labels(&mut server, uri, 8, 2);
    for want in ["value", "fallback", "from_else"] {
        assert!(labels.contains_key(want), "{want}");
    }
    open(
        &mut server,
        uri,
        "def run(flag)\n  if flag; x = 1; end.to_s\n  x\nend\n",
    );
    assert!(completion_labels(&mut server, uri, 2, 2).contains_key("x"));
}

#[test]
fn rescue_bindings_are_offered_only_inside_their_handlers() {
    let mut server = server();
    let uri = "file:///tmp/rescue.vibe";
    open(
        &mut server,
        uri,
        "def run()\n  begin\n    raise(\"boom\")\n  rescue RuntimeError => err\n    err.message\n  end\n  nil\nend\n",
    );
    assert!(completion_labels(&mut server, uri, 4, 4).contains_key("err"));
    assert!(!completion_labels(&mut server, uri, 6, 2).contains_key("err"));
    open(
        &mut server,
        uri,
        "def run()\n  begin\n    raise(\"outer\")\n  rescue RuntimeError => outer_err\n    begin\n      raise(\"inner\")\n    rescue RuntimeError => inner_err\n      inner_err\n    end\n    outer_err\n  end\nend\n",
    );
    let inside = completion_labels(&mut server, uri, 7, 6);
    assert!(inside.contains_key("outer_err") && inside.contains_key("inner_err"));
    let after = completion_labels(&mut server, uri, 9, 4);
    assert!(after.contains_key("outer_err") && !after.contains_key("inner_err"));
}

#[test]
fn completion_survives_unparsable_edits() {
    let mut server = server();
    let uri = "file:///tmp/midedit.vibe";
    open(&mut server, uri, "def helper()\n  1\nend\n");
    change(&mut server, uri, "def helper()\n  1\nend\n\ndef broken(");
    assert!(completion_labels(&mut server, uri, 4, 0).contains_key("helper"));
}

#[test]
fn member_context_excludes_float_literals() {
    for (source, line, character, want) in [
        ("x.", 0, 2, true),
        ("x.up", 0, 4, true),
        ("xup", 0, 3, false),
        ("x. y", 0, 4, false),
        ("x.", 5, 1, false),
        ("1.5", 0, 3, false),
        ("1.5", 0, 2, false),
        ("1.", 0, 2, true),
        ("1.days", 0, 6, true),
        ("1.5e2", 0, 5, false),
        ("1.5E6", 0, 5, false),
        ("1.5e1_0", 0, 7, false),
        ("1.5e", 0, 4, false),
        ("1.5e2.foo", 0, 9, true),
        ("1.5x", 0, 4, true),
    ] {
        assert_eq!(
            completion::member_context(&text::split_lines(source), line, character),
            want,
            "{source}"
        );
    }
}

#[test]
fn locals_do_not_leak_between_functions() {
    let mut server = server();
    let uri = "file:///tmp/gaps.vibe";
    open(
        &mut server,
        uri,
        "def first(alpha)\n  beta = alpha\n  beta\nend\n\ndef second()\n  1\nend\n",
    );
    let between = completion_labels(&mut server, uri, 4, 0);
    assert!(!between.contains_key("alpha") && !between.contains_key("beta"));
    assert!(between.contains_key("first"));
    let inside = completion_labels(&mut server, uri, 1, 2);
    assert!(inside.contains_key("alpha") && inside.contains_key("beta"));
}

#[test]
fn scopes_survive_a_flush_left_inner_end() {
    let mut server = server();
    let uri = "file:///tmp/flushleft.vibe";
    open(
        &mut server,
        uri,
        "def first(alpha)\n  if alpha > 1\n    beta = alpha\nend\n  gamma = alpha\n  gamma\nend\n",
    );
    let labels = completion_labels(&mut server, uri, 4, 2);
    assert!(labels.contains_key("alpha") && labels.contains_key("gamma"));
}

#[test]
fn scopes_reanchor_when_lines_shift_while_unparsable() {
    let mut server = server();
    let uri = "file:///tmp/shifted.vibe";
    open(
        &mut server,
        uri,
        "def first(alpha)\n  beta = alpha\n  beta\nend\n",
    );
    change(
        &mut server,
        uri,
        "# one\n# two\n# three\ndef first(alpha)\n  beta = alpha\n  beta\nend\n\ndef broken(",
    );
    let inside = completion_labels(&mut server, uri, 4, 2);
    assert!(inside.contains_key("alpha") && inside.contains_key("beta"));
    assert!(!completion_labels(&mut server, uri, 0, 0).contains_key("beta"));
}

#[test]
fn anchors_ignore_same_named_class_methods() {
    let mut server = server();
    let uri = "file:///tmp/shadowed-def.vibe";
    open(
        &mut server,
        uri,
        "class Wallet\n  def total(cents)\n    cents\n  end\nend\n\ndef total(amount)\n  rounded = amount\n  rounded\nend\n",
    );
    let inside = completion_labels(&mut server, uri, 7, 2);
    assert!(inside.contains_key("amount") && inside.contains_key("rounded"));
    assert!(!inside.contains_key("cents"));
}

#[test]
fn anchors_match_decorated_top_level_defs() {
    let mut server = server();
    let uri = "file:///tmp/decorated.vibe";
    open(
        &mut server,
        uri,
        "private def secret(token)\n  hashed = token\n  hashed\nend\n",
    );
    change(
        &mut server,
        uri,
        "# one\n# two\nprivate def secret(token)\n  hashed = token\n  hashed\nend\n\ndef broken(",
    );
    let inside = completion_labels(&mut server, uri, 3, 2);
    assert!(inside.contains_key("token") && inside.contains_key("hashed"));
    assert!(!completion_labels(&mut server, uri, 0, 0).contains_key("hashed"));
}

#[test]
fn aliases_complete_as_functions() {
    let mut server = server();
    let uri = "file:///tmp/alias.vibe";
    open(
        &mut server,
        uri,
        "def helper(n)\n  m = n\n  m\nend\n\nalias assist helper\n",
    );
    let labels = completion_labels(&mut server, uri, 1, 2);
    assert_eq!(labels["assist"]["detail"], "function");
    assert!(labels.contains_key("m"));
}

#[test]
fn completion_for_an_unknown_document_offers_keywords_and_builtins() {
    let mut server = server();
    let labels = completion_labels(&mut server, "file:///tmp/unknown.vibe", 0, 0);
    assert!(labels.contains_key("def") && labels.contains_key("puts"));
}

#[test]
fn member_items_carry_unambiguous_docs_and_table_signatures() {
    let items: Vec<Value> = completion::members()
        .iter()
        .map(|entry| serde_json::from_str(&server::completion_json(entry).encode()).unwrap())
        .collect();
    let upcase = find(&items, "upcase");
    assert_eq!(upcase["documentation"]["kind"], "markdown");
    assert!(
        upcase["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("Unicode")
    );
    // Removed spellings are not offered.
    for removed in ["itself", "size", "nil?"] {
        assert!(
            !items.iter().any(|item| item["label"] == removed),
            "{removed}"
        );
    }
    for (label, signatures) in [
        (
            "fetch",
            &[
                "`array<T>.fetch(index: int, default?: T, &block?: int -> T) -> T`",
                "`hash<string, V>.fetch(key: string, default?: V, &block?: string -> V) -> V`",
            ][..],
        ),
        (
            "map",
            &[
                "`array<T>.map<U>(&block: T -> U) -> array<U>`",
                "`hash<string, V>.map<U>(&block: (string, V) -> U) -> array<U>`",
                "`hash<string, V>.map<U>(&block: [string, V] -> U) -> array<U>`",
            ],
        ),
        (
            "sort",
            &[
                "`array<T>.sort(&block: (T, T) -> int) -> array<T>`",
                "`array<T: comparable>.sort -> array<T>`",
            ],
        ),
        ("to_i", &["`string.to_i -> int`", "`duration.to_i -> int`"]),
        ("dup", &["`dup -> T`"]),
        ("to_s", &["`nil.to_s -> string`", "`int.to_s -> string`"]),
    ] {
        let value = find(&items, label)["documentation"]["value"]
            .as_str()
            .unwrap()
            .to_owned();
        for signature in signatures {
            assert!(value.contains(signature), "{label}: {value}");
        }
    }
}

#[test]
fn table_signatures_name_members_the_runtime_dispatches() {
    let members = docs::receiver_members();
    for item in &vibescript::signatures::table().items {
        let vibescript::signatures::Item::Class(class) = item else {
            continue;
        };
        let kind = class.base();
        let Some(available) = members.get(kind) else {
            continue;
        };
        for member in &class.members {
            let name = member.name();
            assert!(available.contains(&name), "{kind}.{name}");
        }
    }
}
