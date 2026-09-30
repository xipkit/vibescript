mod common;

use vibescript::{CallOptions, Engine, ErrorKind, stringify_json};

#[test]
fn standalone_begin_rejects_statement_modifiers() {
    for body in [
        "begin;1;end",
        "begin;raise 'x';rescue;1;end",
        "begin;1;ensure;2;end",
    ] {
        for modifier in ["if true", "while false"] {
            let source = format!("{body} {modifier}");
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
            assert!(error.to_string().contains("modifier"), "{source}: {error}");
        }
        // `unless` and `until` are no modifiers at all (ADR-008).
        for keyword in ["unless", "until"] {
            let source = format!("{body} {keyword} true");
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
            assert!(
                error.message.starts_with("unexpected token") && error.message.contains(keyword),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn begin_values_preserve_expression_modifiers() {
    for expression in [
        "(begin;i+=1;end)",
        "value=begin;i+=1;end",
        "begin;i+=1;end.to_s",
    ] {
        for (modifier, expected) in [
            ("if true", 6),
            ("if !false", 6),
            ("while i<3", 5),
            ("while !(i>3)", 5),
        ] {
            let source = format!("i=5;{expression} {modifier};i");
            let result = Engine::new()
                .compile(&source)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .run(CallOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(result.value.as_int(), Some(expected), "{source}");
        }
    }
    for (source, expected) in [
        ("(begin;raise 'x';rescue;3;end) if true", 3),
        ("(begin;1;ensure;2;end) if !false", 1),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{source}");
    }
}

#[test]
fn instance_variable_names_require_quoted_symbols() {
    for symbol in [":@x", ":@@x"] {
        for source in [
            symbol.to_string(),
            format!("[{symbol}]"),
            format!("{{key: {symbol}}}"),
            format!("def pass(x);x;end;pass({symbol})"),
            format!("def pass(x);x;end;pass {symbol}"),
        ] {
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        }
    }
    let source = r#"[:"@x", :'@@x', [:"@x", :"@@x"][0], [:"@x", :"@@x"][1], :name?, :if, :+ ]"#;
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let symbols = result.value.as_array().unwrap();
    assert_eq!(symbols.len(), 7);
    for (symbol, expected) in symbols
        .iter()
        .zip(["@x", "@@x", "@x", "@@x", "name?", "if", "+"])
    {
        assert_eq!(symbol.type_name(), "symbol");
        assert_eq!(symbol.as_bytes(), Some(expected.as_bytes()));
    }
}

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let json = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn ternary_separators_may_start_a_later_line() {
    let source = "def run -> array<string | int | array<int>>
  a = true ? \"multi\"\n    : \"other\"\n  b = false ?\n    1\n\n  :\n    2\n  [a, b, [true ? 3\n    : 4]]\nend";
    assert_eq!(result(source), serde_json::json!(["multi", 2, [3]]));
    for source in [
        "def run\n  false ? 1\n  :sym\nend",
        "def run\n  false ? 1 ; : 2\nend",
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}");
    }
}

#[test]
fn then_names_a_local_except_where_it_ends_a_condition() {
    let source = "def id(v: int) -> int\n  v\nend\ndef run -> array<int?>\n  then = 1\n  then += 1\n  x = if then == nil then 0 else then end\n  y = case then when 2 then then + 5 end\n  [then, x, y, id(then), (if then != nil then 7 end), (if [then].first != nil then then end)]\nend";
    assert_eq!(result(source), serde_json::json!([2, 2, 7, 2, 7, 2]));
    let batch = "def contextual_then -> int\n  then = 1\n  then\nend\ndef negated_expr(flag: bool) -> array<string?>\n  value = if !flag then \"open\" else \"closed\" end\n  [value, if !flag then \"body\" end]\nend\ndef run -> array<int | array<string?>>\n  [contextual_then, negated_expr(false)]\nend";
    assert_eq!(result(batch), serde_json::json!([1, ["open", "body"]]));
    let error = Engine::new()
        .compile("def run\n  then = 2\n  if (1..then) === 2 then 1 end\nend")
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}

#[test]
fn compound_statements_continue_as_expressions_after_end() {
    let source = "def run -> array<int>\n  a: array<int?> = []\n  a << if true then 5 end\n  if true\n    [5]\n  else\n    [0]\n  end.map { |v| v + 1 }\nend";
    assert_eq!(result(source), serde_json::json!([6]));
    // These once parsed as two statements and silently discarded the first.
    for (statement, ty, expected) in [
        ("if true then 5 else 0 end + 1", "int", serde_json::json!(6)),
        (
            "if true then [5] else [0] end [0]",
            "int?",
            serde_json::json!(5),
        ),
        ("while false; end.to_s", "string", serde_json::json!("")),
        ("while !true; end == nil", "bool", serde_json::json!(true)),
        ("for i in [1, 2] do end.length", "int", serde_json::json!(2)),
    ] {
        assert_eq!(
            result(&format!("def run -> {ty}\n  {statement}\nend")),
            expected,
            "{statement}"
        );
    }
    let error = Engine::new()
        .compile("def run\n  if true\n    1\n  end\n    .to_s\nend")
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}

#[test]
fn reserved_words_label_parenless_keyword_arguments() {
    let source = "def accept(**opts: hash<string, int | string>) -> hash<string, int | string>\n  opts\nend\ndef boom\n  raise \"x\"\nend\ndef run -> array<hash<string, int | string> | int | nil>\n  [accept(rescue: 1), (accept rescue: \"retry\"), (accept begin: 1, ensure: 3), (boom rescue 5)]\nend";
    assert_eq!(
        result(source),
        serde_json::json!([{"rescue": 1}, {"rescue": "retry"}, {"begin": 1, "ensure": 3}, 5])
    );
    let source = "def boom\n  raise \"x\"\nend\ndef run\n  boom() rescue :fallback\nend";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}

#[test]
fn an_implicit_it_still_calls_a_function_with_a_percent_array() {
    // Percent literals are removed, but the parser still reads one after
    // an implicit `it` as the argument of a call, and after a local or
    // numbered parameter as a modulo.
    for (source, expected) in [
        (
            "def it(values: array<string>) -> string\n  values.join(\"-\")\nend\n\
             def run -> array<array<string>>\n  [[1].map { it %w[a b] }]\nend",
            &[("V0310", "it %w"), ("V0410", "%w")][..],
        ),
        (
            "def run -> any\n  it = 7\n  it %w[a b]\nend",
            &[("V0201", "w["), ("V0201", "a "), ("V0201", "b]")],
        ),
        (
            "def run -> any\n  [5].map { _1 %w[a] }\nend",
            &[("V0201", "w["), ("V0201", "a]")],
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        let found: Vec<(String, usize)> = error
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code.to_string(), diagnostic.span.start))
            .collect();
        let expected: Vec<(String, usize)> = expected
            .iter()
            .map(|(code, at)| (code.to_string(), source.find(at).unwrap()))
            .collect();
        assert_eq!(found, expected, "{source}");
    }
    let source = "def run -> array<int>\n  [5].map { it % 2 }\nend";
    assert_eq!(result(source), serde_json::json!([1]));
}

#[test]
fn instance_and_class_variable_names_do_not_need_method_suffixes() {
    let source = "class Record\n  @done: int\n  @respond_to: int\n  def initialize\n    @respond_to = 1\n    @done = 2\n  end\n  def state -> array<int>\n    [@respond_to, @done]\n  end\nend\nclass Widget\n  @@respond_to: int = 3\n  def self.build -> int\n    @@respond_to\n  end\nend\ndef run -> array<int | array<int>>\n  [Record.new.state, Widget.build]\nend";
    assert_eq!(result(source), serde_json::json!([[1, 2], 3]));
}

#[test]
fn one_line_definitions_start_their_body_after_a_bare_name() {
    let source = "def at -> int [1, 2, 3].fetch(1) end\ndef literal -> int 42 end\ndef sum a: int, b: int = 2 -> int a + b end\ndef run -> array<int>\n  [at, literal, sum(1)]\nend";
    assert_eq!(result(source), serde_json::json!([2, 42, 3]));
}

#[test]
fn aliases_declare_top_level_functions_and_nowhere_but_classes() {
    let source = "def name() -> string\n  \"Ada\"\nend\nalias full_name name\nclass User\n  def name() -> string\n    \"Grace\"\n  end\n  alias_method :full_name, :name\nend\ndef run() -> array<string>\n  [full_name(), User.new.full_name]\nend";
    assert_eq!(result(source), serde_json::json!(["Ada", "Grace"]));
    for source in [
        "def foo\n  1\nend\ndef run\n  alias bar foo\nend",
        "def run\n  [1].map { alias x y }\nend",
        "module Naming\n  def self.tag\n    1\n  end\n  alias_method :label, :tag\nend",
        "alias full_name name\ndef name\n  1\nend",
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
    }
}

#[test]
fn adjacent_expressions_report_the_gap_without_guessing_a_repair() {
    for source in [
        "x = 1\"0\"",
        "1 2",
        "\"a\"\"b\"",
        "true false",
        "[1] 2",
        "x=1 x=2",
        "puts 1 2",
        "def f; 1\"0\"; end",
        "if true; 1\"0\"; end",
        "[1].each { 1\"0\" }",
        "class C; x=1\"0\"; end",
        "module M; x=1\"0\"; end",
        "x = \"#{1 2}\"",
        "\"é\" 2",
    ] {
        let error = Engine::new()
            .compile(source)
            .err()
            .unwrap_or_else(|| panic!("compiled {source}"));
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
        let diagnostics = error.diagnostics();
        assert_eq!(diagnostics.len(), 1, "{source}");
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.code.to_string(), "V0001", "{source}");
        assert!(
            diagnostic.message.contains("adjacent expressions"),
            "{source}: {error}"
        );
        assert!(diagnostic.message.contains("operator"));
        assert!(diagnostic.message.contains("comma"));
        assert!(diagnostic.message.contains("newline"));
        assert!(
            diagnostic.fixes.is_empty(),
            "the intended repair is ambiguous"
        );
    }
    for (source, start, end) in [
        ("x = 1\"0\"", 5, 5),
        ("x = \"#{1 2}\"", 8, 9),
        ("1  2", 1, 3),
        ("\"é\" 2", 4, 5),
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        assert_eq!(
            error.diagnostics()[0].span,
            vibescript::diagnostic::Span::new(start, end)
        );
    }
}

#[test]
fn separators_and_parenless_calls_keep_their_meaning() {
    for (source, expected) in [
        ("x=1\n2", 2),
        ("x=1;2", 2),
        ("x=1 # comment\n2", 2),
        (
            "def fetch(key: string) -> int; key.length; end; fetch \"a\"",
            1,
        ),
        ("def add(a: int, b: int) -> int; a+b; end; add 1, 2", 3),
        ("x=1+\n2; puts x; x", 3),
        ("[1].map { |x| x+1 }.fetch(0)", 2),
        ("if true then 1 else 2 end", 1),
        ("def f -> int 1 end\nf", 1),
    ] {
        let mut engine = Engine::new();
        engine.set_output_writer(|_, _| Ok(()));
        let value = engine
            .compile(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .run(CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}
