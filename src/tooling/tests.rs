use super::*;
use crate::Engine;
use std::collections::HashSet;

const NAVIGATION: &str = "def helper(n)
  n * 2
end

class Wallet
  property cents: int
  def balance()
    1
  end
  alias total balance

  def self.empty()
    Wallet.new
  end
end

enum Status
  Draft
  Published
end

alias assist helper
run_now = 1

def run()
  helper(1)
end
";

const MODULES: &str = "module Billing
  LIMIT = 100
  count = 1

  module Codes
    PREFIX = \"B\"

    def self.tag
      PREFIX
    end
  end

  def self.code
    \"B-1\"
  end
end
";

fn at(line: usize, column: usize) -> Position {
    Position { line, column }
}

fn shape(items: &[Item]) -> Vec<(ItemKind, &str, usize)> {
    items
        .iter()
        .map(|item| (item.kind, item.name.as_str(), item.position.line))
        .collect()
}

fn outline_items(source: &str) -> Vec<Item> {
    outline(source).unwrap().items
}

#[test]
fn outlines_top_level_declarations_in_source_order() {
    let outline = outline(NAVIGATION).unwrap();
    assert_eq!(
        shape(&outline.items),
        [
            (ItemKind::Function, "helper", 1),
            (ItemKind::Class, "Wallet", 5),
            (ItemKind::Enum, "Status", 17),
            (ItemKind::Alias, "assist", 22),
            (ItemKind::Statement, "", 23),
            (ItemKind::Function, "run", 25),
        ]
    );
    let wallet = &outline.items[1];
    assert_eq!(wallet.position, at(5, 1));
    assert_eq!(
        shape(&wallet.children),
        [
            (ItemKind::Property, "cents", 6),
            (ItemKind::Method, "balance", 7),
            (ItemKind::Alias, "total", 10),
            (ItemKind::ClassMethod, "empty", 12),
        ]
    );
    assert_eq!(wallet.children[0].position, at(6, 12));
    assert_eq!(wallet.children[2].target.as_deref(), Some("balance"));
    assert_eq!(
        wallet.children[2].function, wallet.children[1].function,
        "an alias reports the facts of the method it copies"
    );
    let status = &outline.items[2];
    assert_eq!(
        shape(&status.children),
        [
            (ItemKind::EnumMember, "Draft", 18),
            (ItemKind::EnumMember, "Published", 19),
        ]
    );
    assert_eq!(status.children[1].position, at(19, 3));
    let alias = &outline.items[3];
    assert_eq!(alias.target.as_deref(), Some("helper"));
    assert_eq!(alias.function.as_ref().unwrap().params[0].name, "n");
}

#[test]
fn outlines_modules_constants_and_nested_modules() {
    let outline = outline(MODULES).unwrap();
    let billing = &outline.items[0];
    assert_eq!(
        (billing.kind, billing.position),
        (ItemKind::Module, at(1, 1))
    );
    assert_eq!(
        shape(&billing.children),
        [
            (ItemKind::Constant, "LIMIT", 2),
            (ItemKind::Statement, "", 3),
            (ItemKind::Module, "Codes", 5),
            (ItemKind::ClassMethod, "code", 13),
        ]
    );
    assert_eq!(
        shape(&billing.children[2].children),
        [
            (ItemKind::Constant, "PREFIX", 6),
            (ItemKind::ClassMethod, "tag", 8),
        ]
    );
    // Class bodies run per instance, so their assignments are statements.
    let class = outline_items("class A\n  LIMIT = 1\nend\n");
    assert_eq!(shape(&class[0].children), [(ItemKind::Statement, "", 2)]);
}

#[test]
fn modifiers_and_setters_keep_their_declaration_lines() {
    let items = outline_items(
        "private def secret(token)\n  token\nend\n\nclass Counter\n  protected def guard\n  end\n  def value=(n)\n    @value = n\n  end\nend\n",
    );
    assert_eq!(shape(&items)[0], (ItemKind::Function, "secret", 1));
    assert_eq!(items[0].position, at(1, 1));
    assert_eq!(
        shape(&items[1].children),
        [
            (ItemKind::Method, "guard", 6),
            (ItemKind::Method, "value=", 8)
        ]
    );
}

#[test]
fn reports_signatures_in_canonical_form() {
    let items = outline_items(
        "def charge(amount: int, currency = \"USD\", note: string? = nil, *rest: array<int>, host:, port: 8080, name: string:, **options) -> money?\nend\nclass User\n  def initialize(@name, @age: int)\n  end\nend\n",
    );
    let function = items[0].function.as_ref().unwrap();
    let params: Vec<_> = function
        .params
        .iter()
        .map(|param| {
            (
                param.name.as_str(),
                param.kind,
                param.type_annotation.as_deref(),
                param.default,
            )
        })
        .collect();
    assert_eq!(
        params,
        [
            ("amount", ParameterKind::Positional, Some("int"), false),
            ("currency", ParameterKind::Positional, None, true),
            ("note", ParameterKind::Positional, Some("string?"), true),
            ("rest", ParameterKind::Rest, Some("array<int>"), false),
            ("host", ParameterKind::Keyword, None, false),
            ("port", ParameterKind::Keyword, None, true),
            ("name", ParameterKind::Keyword, Some("string"), false),
            ("options", ParameterKind::KeywordRest, None, false),
        ]
    );
    assert_eq!(function.return_type.as_deref(), Some("money?"));
    let initialize = items[1].children[0].function.as_ref().unwrap();
    assert!(initialize.params.iter().all(|param| param.instance));
    assert_eq!(initialize.params[0].name, "name");
}

#[test]
fn collects_locals_outside_blocks() {
    let items = outline_items(
        "def run(flag)
  first, *rest, (left, right) = [1, 2, [3, 4]]
  for item in [1]
    looped = item
  end
  if flag; x = 1; end.to_s
  begin
    value = 1
  rescue RuntimeError => err
    fallback = 2
  else
    from_else = value
  ensure
    cleanup = 3
  end
  [1].each { |n| hidden = n }
  total = [1].map do |n|
    inner = n
  end
  y = 1 if flag
  @field = 1
  first += 1
end
",
    );
    let function = items[0].function.as_ref().unwrap();
    assert_eq!(
        function.locals,
        [
            "first",
            "rest",
            "left",
            "right",
            "item",
            "looped",
            "x",
            "value",
            "fallback",
            "from_else",
            "cleanup",
            "total",
            "y",
        ]
    );
}

#[test]
fn lists_rescue_bindings_in_nesting_order() {
    let items = outline_items(
        "def run()
  begin
    begin
      raise(\"inner\")
    rescue RuntimeError => inner
      inner
    end
  rescue RuntimeError => outer
    begin
      raise(\"again\")
    rescue => nested
      nested
      nested
    end
    outer
  rescue ArgumentError
    nil
  end
  [1].each do
    begin
      1
    rescue => hidden
      hidden
    end
  end
end
",
    );
    let function = items[0].function.as_ref().unwrap();
    let rescues: Vec<_> = function
        .rescues
        .iter()
        .map(|rescue| {
            (
                rescue.binding.as_str(),
                rescue.position.line,
                rescue.last_statement.map(|position| position.line),
            )
        })
        .collect();
    assert_eq!(
        rescues,
        [
            ("outer", 8, Some(15)),
            ("inner", 5, Some(6)),
            ("nested", 11, Some(13)),
        ]
    );
    assert_eq!(function.last_statement.map(|p| p.line), Some(19));
}

#[test]
fn last_statement_descends_into_control_flow_only() {
    let last = |source: &str| {
        outline_items(source)[0]
            .function
            .as_ref()
            .unwrap()
            .last_statement
            .map(|position| (position.line, position.column))
    };
    assert_eq!(last("def f\nend\n"), None);
    assert_eq!(last("def f; 1; end\n"), Some((1, 8)));
    // An empty trailing branch counts from its condition's line.
    assert_eq!(
        last("def f(a)\n  if a\n    1\n  elsif a > 1\n  end\nend\n").map(|(line, _)| line),
        Some(4)
    );
    assert_eq!(
        last("def f(a)\n  while a\n    a = nil\n  end\nend\n"),
        Some((3, 5))
    );
    // A call spanning lines counts from its start, and blocks are not entered.
    assert_eq!(
        last("def f\n  g(\n    1\n  )\n  [1].each do |x|\n    x\n  end\nend\n"),
        Some((5, 3))
    );
    // A function-level rescue wraps the body.
    assert_eq!(
        last("def f\n  1\nrescue => e\n  e\n  2\nend\n"),
        Some((5, 3))
    );
}

#[test]
fn outline_errors_match_compilation() {
    for source in ["def run(\n  1\nend\n", "def 123()\nend\n", "x = [1,\n"] {
        let compiled = Engine::new().compile(source).err().unwrap();
        assert_eq!(outline(source).unwrap_err(), compiled, "{source}");
    }
    let oversized = " ".repeat(crate::syntax::MAX_SOURCE + 1);
    assert_eq!(
        outline(&oversized).unwrap_err(),
        Engine::new().compile(&oversized).err().unwrap()
    );
}

// WASI preview 1 cannot spawn the small-stack thread.
#[cfg(not(target_os = "wasi"))]
#[test]
fn walks_deep_syntax_without_native_recursion() {
    let depth = 900;
    let mut source = String::from("def f(a)\n");
    for _ in 0..depth {
        source.push_str("if a\n");
    }
    source.push_str("deepest = 1\n");
    for _ in 0..depth {
        source.push_str("end\n");
    }
    source.push_str("end\n");
    let mut modules = String::new();
    for index in 0..300 {
        modules.push_str(&format!("module M{index}\n"));
    }
    modules.push_str("def self.leaf\nend\n");
    for _ in 0..300 {
        modules.push_str("end\n");
    }
    let (function, nested) = std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            let function = outline(&source).unwrap().items.remove(0).function.unwrap();
            let mut item = outline(&modules).unwrap().items.remove(0);
            let mut nested = 1;
            while let Some(child) = item.children.pop() {
                if child.kind == ItemKind::Module {
                    nested += 1;
                }
                item = child;
            }
            (function, nested)
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(function.locals, ["deepest"]);
    assert_eq!(function.last_statement.map(|p| p.line), Some(depth + 2));
    assert_eq!(nested, 300);
}

#[test]
fn keywords_are_the_reserved_words() {
    // The reference parser's keyword table.
    assert_eq!(
        keywords(),
        [
            "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure",
            "enum", "export", "false", "for", "getter", "if", "in", "next", "nil", "private",
            "property", "raise", "rescue", "retry", "return", "self", "setter", "then", "true",
            "unless", "until", "when", "while", "yield",
        ]
    );
    for word in keywords() {
        assert!(crate::syntax::keyword(word), "{word}");
    }
    for word in ["alias", "module", "public", "protected", "puts", "it"] {
        assert!(!crate::syntax::keyword(word), "{word}");
    }
}

#[test]
fn member_names_resolve_on_their_receivers() {
    let names = member_names();
    let kinds: Vec<_> = names.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        [
            "string", "symbol", "array", "hash", "int", "float", "money", "duration", "time",
            "range", "nil", "bool", "regex"
        ]
    );
    let receivers = [
        "\"x\"",
        ":x",
        "[1]",
        "{a: 1}",
        "1",
        "1.5",
        "money(\"1.00 USD\")",
        "1.seconds",
        "Time.now",
        "(1..2)",
        "nil",
        "true",
        "/x/",
    ];
    for ((kind, members), receiver) in names.iter().zip(receivers) {
        assert!(
            members.contains(&"tap") && members.contains(&"respond_to?"),
            "{kind}"
        );
        let mut seen = HashSet::new();
        for member in members {
            assert!(seen.insert(member), "{kind}.{member} repeats");
            let source = format!("({receiver}).respond_to?(:{member}, true)");
            let value = Engine::new()
                .compile(&source)
                .unwrap()
                .run(Default::default())
                .unwrap()
                .value;
            assert!(value.truthy(), "{source}");
        }
    }
}

#[test]
fn member_receivers_come_from_literals_and_annotations() {
    let probe = "vibesCompletionProbe__";
    for (source, expected) in [
        (
            "def f(s: string)\n  s.vibesCompletionProbe__\nend",
            Some("string"),
        ),
        (
            "def f(items: array<int>)\n  items.vibesCompletionProbe__\nend",
            Some("array"),
        ),
        (
            "def f(m: money)\n  m.vibesCompletionProbe__\nend",
            Some("money"),
        ),
        (
            "def f(d: duration, r: range)\n  r.vibesCompletionProbe__\nend",
            Some("range"),
        ),
        ("x = \"abc\".vibesCompletionProbe__\n", Some("string")),
        ("x = \"a#{1}\".vibesCompletionProbe__\n", Some("string")),
        ("x = :sym.vibesCompletionProbe__\n", Some("symbol")),
        ("x = [1].vibesCompletionProbe__\n", Some("array")),
        ("x = ({a: 1}).vibesCompletionProbe__\n", Some("hash")),
        ("x = 1.vibesCompletionProbe__\n", Some("int")),
        ("x = 1.5.vibesCompletionProbe__\n", Some("float")),
        ("x = true.vibesCompletionProbe__\n", Some("bool")),
        ("x = /a/.vibesCompletionProbe__\n", Some("regex")),
        ("x = [1]&.vibesCompletionProbe__\n", Some("array")),
        ("x = [1].vibesCompletionProbe__(2)\n", Some("array")),
        // A later syntax error does not hide an earlier probe.
        ("x = [1].vibesCompletionProbe__\ndef broken(", Some("array")),
        ("def f(x)\n  x.vibesCompletionProbe__\nend", None),
        ("def f(s: string?)\n  s.vibesCompletionProbe__\nend", None),
        (
            "def f(s: string | int)\n  s.vibesCompletionProbe__\nend",
            None,
        ),
        ("def f(u: User)\n  u.vibesCompletionProbe__\nend", None),
        ("def f()\n  x = 1\n  x.vibesCompletionProbe__\nend", None),
        ("def f()\n  build().vibesCompletionProbe__\nend", None),
        ("x = nil.vibesCompletionProbe__\n", None),
        ("x = \"#{[1].vibesCompletionProbe__}\"\n", None),
        ("x = )\ny = [1].vibesCompletionProbe__\n", None),
        ("x = 1\n", None),
    ] {
        assert_eq!(member_receiver(source, probe), expected, "{source}");
    }
    let mut deep = String::from("x = [1].vibesCompletionProbe__\n");
    deep.push_str(&"[".repeat(2000));
    assert_eq!(member_receiver(&deep, probe), None);
}
