use super::support::{clean, codes, errors_with, fixed};
use vibescript::{Capability, Engine, HostMethod, Value};

#[test]
fn familiar_foreign_names_explain_the_canonical_operation() {
    for (source, canonical, expected) in [
        ("len([1, 2])", "[1, 2].length", "x.length"),
        ("str(42)", "42.to_s", "x.to_s"),
        ("int(\"42\")", "\"42\".to_i", "x.to_i"),
        ("float(\"4.2\")", "\"4.2\".to_f", "x.to_f"),
        ("Integer(\"42\")", "\"42\".to_i", "x.to_i"),
        ("Float(\"4.2\")", "\"4.2\".to_f", "x.to_f"),
        ("String(42)", "42.to_s", "x.to_s"),
        ("sorted([2, 1])", "[2, 1].sort", "items.sort"),
        ("reversed([1, 2])", "[1, 2].reverse", "items.reverse"),
        (
            "enumerate([1, 2])",
            "[1, 2].each_with_index { |item, index| puts item, index }",
            "each_with_index",
        ),
        ("zip([1], [2])", "[1].zip([2])", "a.zip(b)"),
        ("sum([1, 2])", "[1, 2].sum", "items.sum"),
        ("min([1, 2])", "[1, 2].min", "items.min"),
        ("max([1, 2])", "[1, 2].max", "items.max"),
        ("abs(-2)", "(-2).abs", "x.abs"),
        ("round(1.2)", "1.2.round", "x.round"),
        ("range(3)", "0...3", "0...n"),
        (
            "isinstance(\"s\", string)",
            "\"s\".is_type?(:string)",
            "x.is_type?(:string)",
        ),
        ("parseInt(\"42\")", "\"42\".to_i", "text.to_i"),
        ("parseFloat(\"4.2\")", "\"4.2\".to_f", "text.to_f"),
        (
            "fmt.Sprintf(\"%d\", 42)",
            "format(\"%d\", 42)",
            "format(pattern, ...)",
        ),
        ("fmt.Println(42)", "puts 42", "puts ..."),
        ("fmt.Print(42)", "print 42", "print ..."),
        (
            "strings.ToLower(\"HI\")",
            "\"HI\".downcase",
            "text.downcase",
        ),
        ("strings.ToUpper(\"hi\")", "\"hi\".upcase", "text.upcase"),
        (
            "strings.TrimSpace(\" hi \" )",
            "\" hi \".strip",
            "text.strip",
        ),
        (
            "strings.Contains(\"hi\", \"h\")",
            "\"hi\".include?(\"h\")",
            "text.include?(part)",
        ),
        (
            "strings.HasPrefix(\"hi\", \"h\")",
            "\"hi\".start_with?(\"h\")",
            "text.start_with?(prefix)",
        ),
        (
            "strings.HasSuffix(\"hi\", \"i\")",
            "\"hi\".end_with?(\"i\")",
            "text.end_with?(suffix)",
        ),
        (
            "strings.Split(\"a,b\", \",\")",
            "\"a,b\".split(\",\")",
            "text.split(separator)",
        ),
        (
            "strings.Join([\"a\", \"b\"], \",\")",
            "[\"a\", \"b\"].join(\",\")",
            "items.join(separator)",
        ),
        (
            "strings.ReplaceAll(\"a\", \"a\", \"b\")",
            "\"a\".gsub(\"a\", \"b\")",
            "text.gsub(old, new)",
        ),
        ("strconv.Atoi(\"42\")", "\"42\".to_i", "text.to_i"),
        ("strconv.Itoa(42)", "42.to_s", "n.to_s"),
        (
            "json.loads(\"42\")",
            "JSON.parse(\"42\")",
            "JSON.parse(text)",
        ),
        (
            "json.dumps(42)",
            "JSON.stringify(42)",
            "JSON.stringify(value)",
        ),
        ("console.log(42)", "puts 42", "puts ..."),
        ("Object.keys({ a: 1 })", "{ a: 1 }.keys", "value.keys"),
        ("Object.values({ a: 1 })", "{ a: 1 }.values", "value.values"),
    ] {
        let diagnostics = codes(source, &["V0201"]);
        assert_eq!(diagnostics[0].labels.len(), 1, "{source}: {diagnostics:?}");
        assert!(
            diagnostics[0].labels[0].message.contains(expected),
            "{source}: {diagnostics:?}"
        );
        clean(canonical);
    }
}

#[test]
fn len_fixes_preserve_argument_boundaries_and_only_rewrite_collections() {
    for source in [
        "len([1, 2])",
        "len [1, 2]",
        "len({ a: 1, b: 2 })",
        "pair: [int, string] = [1, \"s\"]; len(pair)",
        "h: hash<string, int> = { a: 1, b: 2 }; len(h)",
        "len(if true; [1, 2]; else; [3]; end)",
        "def items -> array<int>; [1, 2]; end; len(items)",
        "# é\nn: int = len([1, 2]); n",
        "def take(n: int) -> int; n; end; take len([1, 2])",
        "def take(n: int) -> int; n; end; take len [1, 2]",
        "(len([1, 2]))",
    ] {
        let diagnostics = codes(source, &["V0201"]);
        let repaired = fixed(source, &diagnostics[0]);
        let value = Engine::new()
            .compile(&repaired)
            .unwrap()
            .run(Default::default())
            .unwrap()
            .value;
        assert_eq!(value.as_int(), Some(2), "{repaired}");
    }
    for source in [
        "len(\"é\")",
        "len(42)",
        "len",
        "len([1], [2])",
        "len(*[[1]])",
        "len(items: [1])",
        "len([1]) { 2 }",
        "len(nil)",
        "fmt.Sprintf(\"%d\", 42)",
        "strings.ToLower(\"A\")",
        "def f(x: any); len(x); end",
        "def f(x: array<int>?); len(x); end",
        "class Box; def length -> int; 2; end; end; len(Box.new)",
    ] {
        let diagnostics = codes(source, &["V0201"]);
        assert!(diagnostics[0].fixes.is_empty(), "{source}: {diagnostics:?}");
    }
}

#[test]
fn foreign_hints_do_not_change_name_resolution() {
    clean("def len(x: int) -> int; x + 1; end; len(1)");
    clean(
        "module Fmt; def self.Sprintf(s: string) -> string; s; end; end; fmt = Fmt; fmt.Sprintf(\"ok\")",
    );
    clean(
        "class Client; def ToLower(s: string) -> string; s; end; end; strings = Client.new; strings.ToLower(\"ok\")",
    );
    let mut engine = Engine::new();
    engine.register_method("len", HostMethod::new("len", |_, _, _| Ok(Value::int(42))));
    for (namespace, member) in [("fmt", "Sprintf"), ("strings", "ToLower")] {
        let method = HostMethod::new(member, |ctx, _, _| ctx.bytes(b"host"));
        engine
            .declare_capability(&Capability::from_value(
                namespace,
                Value::object(vec![(member.as_bytes().to_vec(), method.value())]),
            ))
            .unwrap();
    }
    assert!(
        errors_with(
            &engine,
            "len(1); fmt.Sprintf(\"ok\"); strings.ToLower(\"ok\")"
        )
        .is_empty()
    );
    engine.declare_global("Object", "{ keys: int }").unwrap();
    assert!(errors_with(&engine, "Object[\"keys\"]").is_empty());
    for source in [
        "missing(1)",
        "fmt.Unknown(1)",
        "strings.Unknown(1)",
        "fmt",
        "strings",
        "LEN([1])",
    ] {
        let diagnostics = codes(source, &["V0201"]);
        assert!(
            diagnostics[0].labels.is_empty(),
            "{source}: {diagnostics:?}"
        );
        assert!(diagnostics[0].fixes.is_empty());
    }
    let diagnostics = codes("len = 1; len(1)", &["V0310"]);
    assert!(diagnostics[0].labels.is_empty());
    let diagnostics = codes("fmt = 1; fmt.Sprintf(\"ok\")", &["V0203"]);
    assert!(diagnostics[0].labels.is_empty());
}
