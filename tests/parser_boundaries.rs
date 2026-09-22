use vibescript::{CallOptions, Engine, ErrorKind, stringify_json};

#[test]
fn standalone_begin_rejects_statement_modifiers() {
    for body in [
        "begin;1;end",
        "begin;raise 'x';rescue;1;end",
        "begin;1;ensure;2;end",
    ] {
        for modifier in ["if true", "unless false", "while false", "until true"] {
            let source = format!("{body} {modifier}");
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
            assert!(error.to_string().contains("modifier"), "{source}: {error}");
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
            ("unless false", 6),
            ("while i<3", 5),
            ("until i>3", 5),
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
        ("(begin;1;ensure;2;end) unless false", 1),
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
    let source = r#"[:"@x", :'@@x', %i[@x @@x][0], %i[@x @@x][1], :name?, :if, :+ ]"#;
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
    let source = "def run
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
    let source = "def id(v)\n  v\nend\ndef run\n  then = 1\n  then += 1\n  x = unless then then 0 else then end\n  y = case then when 2 then then + 5 end\n  [then, x, y, id(then), (if then then 7 end), (if [then].first then then end)]\nend";
    assert_eq!(result(source), serde_json::json!([2, 2, 7, 2, 7, 2]));
    let batch = "def contextual_then()\n  then = 1\n  then\nend\ndef unless_expr(flag)\n  value = unless flag then \"open\" else \"closed\" end\n  [value, unless flag then \"body\" end]\nend\ndef run\n  [contextual_then(), unless_expr(false)]\nend";
    assert_eq!(result(batch), serde_json::json!([1, ["open", "body"]]));
    let error = Engine::new()
        .compile("def run\n  then = 2\n  if (1..then) === 2 then 1 end\nend")
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}

#[test]
fn compound_statements_continue_as_expressions_after_end() {
    let source = "def run\n  a = []\n  a << if true then 5 end\n  if true\n    [5]\n  end.map { |v| v + 1 }\nend";
    assert_eq!(result(source), serde_json::json!([6]));
    // These once parsed as two statements and silently discarded the first.
    for (statement, expected) in [
        ("if true then 5 end + 1", serde_json::json!(6)),
        ("if true then [5] end [0]", serde_json::json!(5)),
        ("while false; end.to_s", serde_json::json!("")),
        ("until true; end.nil?", serde_json::json!(true)),
        ("for i in [1, 2] do end.size", serde_json::json!(2)),
    ] {
        assert_eq!(
            result(&format!("def run\n  {statement}\nend")),
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
    let source = "def accept(opts)\n  opts\nend\ndef boom\n  raise \"x\"\nend\ndef run\n  [accept(rescue: 1), (accept rescue: \"retry\"), (accept begin: 1, ensure: 3), (boom rescue 5)]\nend";
    assert_eq!(
        result(source),
        serde_json::json!([{"rescue": 1}, {"rescue": "retry"}, {"begin": 1, "ensure": 3}, 5])
    );
    let source = "def boom\n  raise \"x\"\nend\ndef run\n  boom() rescue :fallback\nend";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}
