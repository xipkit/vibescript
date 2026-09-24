//! Hover documentation.

use super::*;

#[test]
fn hover_serves_builtin_docs() {
    let value = hover_value("def run()\n  assert(true)\nend\n", 1, 3);
    assert!(
        value.contains(&format!("`{}`", builtin_signature("assert"))),
        "{value}"
    );
    assert!(
        value.contains("Raises an error if `condition` is falsy"),
        "{value}"
    );
    assert!(!value.contains("Vibescript builtin"), "{value}");
}

#[test]
fn hover_resolves_qualified_builtins() {
    let value = hover_value(
        "def run(raw)\n  JSON.parse_as(raw, { name: string })\nend\n",
        1,
        10,
    );
    assert!(value.contains("JSON.parse_as"), "{value}");
    assert!(value.contains("shape"), "{value}");
    // The member name without its namespace has no entry.
    let bare = hover_value("def run(raw)\n  parse_as(raw)\nend\n", 1, 5);
    assert!(bare.contains("Vibescript symbol"), "{bare}");
}

#[test]
fn hover_serves_keyword_docs() {
    let value = hover_value("if true then\n  1\nend\n", 0, 9);
    assert!(value.contains("`then`"), "{value}");
    assert!(value.contains("Optional separator"), "{value}");
}

#[test]
fn unknown_words_fall_back_to_the_classifier() {
    assert_eq!(
        hover_value("def run()\n  frobnicate\nend\n", 1, 4),
        "`frobnicate`\n\nVibescript symbol"
    );
    // Hovering nothing returns an explicit null.
    let mut server = server();
    open(&mut server, "file:///tmp/test.vibe", "x = 1\n\n");
    let replies = replies(
        &mut server,
        &message(
            "textDocument/hover",
            Some("1"),
            Some(position("file:///tmp/test.vibe", 1, 0)),
        ),
    );
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","id":1,"result":null}"#
    );
}

#[test]
fn hover_serves_member_docs() {
    let value = hover_value("def run(name)\n  name.upcase\nend\n", 1, 9);
    assert!(value.contains("`upcase(mode = nil) -> string`"), "{value}");
    assert!(value.contains("Unicode"), "{value}");
    assert!(!value.contains("---"), "{value}");
}

#[test]
fn hover_merges_ambiguous_member_docs_in_receiver_order() {
    let value = hover_value("def run(items)\n  items.size\nend\n", 1, 9);
    assert!(value.contains("---"), "{value}");
    let mut previous = 0;
    for header in [
        "`array.size`",
        "`hash.size`",
        "`range.size`",
        "`string.size`",
    ] {
        let index = value
            .find(header)
            .unwrap_or_else(|| panic!("{header} in {value}"));
        assert!(index >= previous, "{value}");
        previous = index;
    }
}

#[test]
fn hover_serves_universal_member_docs() {
    let value = hover_value("def run(value)\n  value.itself\nend\n", 1, 10);
    assert!(value.contains("returns the receiver unchanged"), "{value}");
    assert!(!value.contains("---"), "{value}");
}

#[test]
fn unknown_members_fall_back_to_the_classifier() {
    assert_eq!(
        hover_value("def run(x)\n  x.frobnify\nend\n", 1, 6),
        "`frobnify`\n\nVibescript symbol"
    );
    // A bare word named like a member is not a member access.
    assert_eq!(
        hover_value("def run()\n  upcase\nend\n", 1, 4),
        "`upcase`\n\nVibescript symbol"
    );
}

#[test]
fn value_member_access_excludes_namespaces_ranges_and_scopes() {
    let catalog = Catalog::new();
    for (line, character, want) in [
        ("items.map", 7, true),
        ("5.minutes", 4, true),
        ("  .map { |x| x }", 4, true),
        ("map(x)", 1, false),
        ("JSON.parse(raw)", 7, false),
        ("Math::PI", 7, false),
        ("1..last", 4, false),
    ] {
        assert_eq!(
            hover::value_member_access(&catalog, &[line], 0, character),
            want,
            "{line}"
        );
    }
}

#[test]
fn qualified_words_need_a_standalone_receiver() {
    for (line, character, want) in [
        ("JSON.parse_as(raw)", 8, "JSON.parse_as"),
        ("parse_as(raw)", 3, ""),
        ("JSON. parse_as", 8, ""),
        (".parse_as", 3, ""),
        ("payload.keys.sort", 9, "payload.keys"),
        ("Math::PI", 7, "Math.PI"),
        ("payload.JSON.parse", 14, ""),
        ("A::JSON.parse", 9, ""),
        ("label: value", 8, ""),
        ("::PI", 3, ""),
    ] {
        assert_eq!(hover::qualified_word(&[line], 0, character), want, "{line}");
    }
}

const USER_HOVER: &str = "# vibe: strict
# Adds a and b.
# Returns their sum.
def add(a: int, b: int = 2) -> int
  a + b
end

def plain(value)
  value
end

def run()
  add(1)
  plain(2)
end
";

#[test]
fn hover_serves_user_function_docs() {
    let value = hover_value(USER_HOVER, 12, 3);
    assert!(
        value.contains("```vibe\ndef add(a: int, b: int = …) -> int\n```"),
        "{value}"
    );
    assert!(
        value.contains("Adds a and b.\nReturns their sum."),
        "{value}"
    );
    assert!(!value.contains("vibe: strict"), "{value}");
    assert_eq!(
        hover_value(USER_HOVER, 13, 3),
        "```vibe\ndef plain(value)\n```"
    );
}

#[test]
fn builtin_docs_shadow_same_named_user_functions() {
    let value = hover_value(
        "def puts(value)\n  value\nend\n\ndef run()\n  puts(1)\nend\n",
        5,
        3,
    );
    assert!(value.contains("Writes each value"), "{value}");
    assert!(!value.contains("```vibe"), "{value}");
}

#[test]
fn hover_serves_user_classes_methods_and_enums() {
    let mut server = server();
    let uri = "file:///tmp/user-hover.vibe";
    open(&mut server, uri, NAVIGATION);
    for (line, character, want) in [
        (4, 8, "```vibe\nclass Wallet\n```"),
        (5, 7, "```vibe\ndef balance\n```"),
        (9, 12, "```vibe\ndef self.empty\n```"),
        (14, 6, "```vibe\nenum Status\n```"),
        (16, 4, "```vibe\nStatus::Published\n```"),
    ] {
        assert_eq!(hover_at(&mut server, uri, line, character), want);
    }
}

#[test]
fn hover_resolves_setters_at_write_sites_and_declarations() {
    let source = "class Counter\n  # Stores the count.\n  def value=(n)\n    @value = n\n  end\nend\n\ndef run()\n  c = Counter.new\n  c.value = 3\nend\n";
    let value = hover_value(source, 9, 5);
    assert!(value.contains("```vibe\ndef value=(n)\n```"), "{value}");
    assert!(value.contains("Stores the count."), "{value}");

    let source = "class Counter\n  # Reads the current value.\n  def value\n    @value\n  end\n\n  # Writes the current value.\n  def value=(next_value)\n    @value = next_value\n  end\nend\n\nc = Counter.new\nc.value = 3\nx = c.value\nok = c.value == 3\n";
    for (line, character, want) in [
        (13, 3, "Writes the current value."),
        (14, 7, "Reads the current value."),
        (15, 8, "Reads the current value."),
    ] {
        let value = hover_value(source, line, character);
        assert!(value.contains(want), "{line}: {value}");
    }

    let source = "class Counter\n  # Reads the current value.\n  def value\n  end\n\n  # Writes the current value.\n  def value=(v)\n  end\nend\n\nvalue = 3\nc = Counter.new\nself.value = 4\n";
    assert!(!hover_value(source, 10, 2).contains("Writes the current value."));
    assert!(hover_value(source, 12, 7).contains("Writes the current value."));

    let source = "class Counter\n  # Reads the current value.\n  def value\n  end\n\n  # Writes the current value.\n  def value=(v)\n  end\nend\n\nvalue=3\n";
    assert!(hover_value(source, 6, 8).contains("Writes the current value."));
    assert!(!hover_value(source, 10, 2).contains("Writes the current value."));
}

#[test]
fn hover_prefers_the_declaration_in_scope() {
    let source = "class Alpha\n  # Runs the alpha path.\n  def run(a: int) -> int\n    a\n  end\nend\n\nclass Beta\n  # Runs the beta path.\n  def run(b: string) -> string\n    helper\n  end\n\n  def helper\n    run(\"x\")\n  end\nend\n\nenum First\n  Draft\nend\n\nenum Second\n  Draft\nend\n\n# Runs the top-level path.\ndef run\nend\nrun\n";
    for (line, character, want) in [
        (2, 7, "Runs the alpha path."),
        (9, 7, "Runs the beta path."),
        (14, 5, "Runs the beta path."),
        (2, 7, "def run(a: int) -> int"),
        (9, 7, "def run(b: string) -> string"),
        (19, 3, "First::Draft"),
        (23, 3, "Second::Draft"),
        (29, 1, "Runs the top-level path."),
    ] {
        let value = hover_value(source, line, character);
        assert!(value.contains(want), "{line}:{character}: {value}");
    }
}

#[test]
fn dotted_members_beat_global_builtins() {
    let value = hover_value("price = money(\"$3.50\")\nlabel = price.format\n", 1, 15);
    assert!(!value.contains("format(pattern, *values)"), "{value}");
    assert!(
        value.contains("format") && !value.contains("Vibescript symbol"),
        "{value}"
    );
}

#[test]
fn hover_resolves_qualified_and_nested_symbols() {
    let source = "enum First\n  Draft\nend\n\nenum Second\n  Draft\nend\n\nmodule Outer\n  module Inner\n    # Inner helper.\n    def self.helper\n      1\n    end\n  end\n\n  # Outer helper.\n  def self.helper\n    2\n  end\n\n  def self.run\n    helper\n  end\nend\n\na = First::Draft\nb = Second::Draft\n";
    for (line, character, want) in [
        (26, 12, "First::Draft"),
        (27, 13, "Second::Draft"),
        (22, 5, "Outer helper."),
        (11, 14, "Inner helper."),
    ] {
        let value = hover_value(source, line, character);
        assert!(value.contains(want), "{line}:{character}: {value}");
    }
    let source =
        "module Outer\n  # Inner workings.\n  module Inner\n  end\nend\n\nx = Outer::Inner\n";
    let value = hover_value(source, 6, 12);
    assert!(
        value.contains("module Inner") && value.contains("Inner workings."),
        "{value}"
    );
}

#[test]
fn hover_renders_typed_required_keywords_in_declaration_form() {
    let value = hover_value(
        "# Greets loudly.\ndef f(name: string:)\n  name\nend\n",
        1,
        5,
    );
    assert!(value.contains("def f(name: string:)"), "{value}");
    assert!(!value.contains("name:: string"), "{value}");
}

#[test]
fn qualified_access_excludes_top_level_declarations() {
    let source = "# Saves everything at once.\ndef save\nend\n\nclass Client\n  # Saves this client.\n  def save\n  end\nend\n\nclient.save\nsave\n";
    assert!(hover_value(source, 10, 8).contains("Saves this client."));
    assert!(hover_value(source, 11, 1).contains("Saves everything at once."));
}

#[test]
fn receivers_pick_instance_or_class_methods() {
    let source = "class Client\n  # Saves this client instance.\n  def save\n  end\n\n  # Saves every client at once.\n  def self.save\n  end\nend\n\nclient.save\nClient.save\n";
    assert!(hover_value(source, 10, 8).contains("Saves this client instance."));
    assert!(hover_value(source, 11, 8).contains("Saves every client at once."));
    let source = "class Client\n  # Saves this client instance.\n  def save\n  end\nend\n\nClient.save\nclient.save\n";
    assert!(!hover_value(source, 6, 8).contains("Saves this client instance."));
    assert!(hover_value(source, 7, 8).contains("Saves this client instance."));
}

#[test]
fn scoped_names_stay_out_of_the_top_level() {
    let source = "module Outer\n  module Inner\n    # Inner helper.\n    def self.helper\n    end\n  end\n  helper\nend\n\nclass A\n  # Runs A.\n  def run\n  end\nend\n\nrun\n";
    assert!(!hover_value(source, 15, 1).contains("Runs A."));
    assert!(!hover_value(source, 6, 3).contains("Inner helper."));
}

#[test]
fn hover_uses_utf16_positions() {
    let value = hover_value("x = \"\u{1F600}\" + puts\n", 0, 12);
    assert!(value.contains("Writes each value"), "{value}");
}
