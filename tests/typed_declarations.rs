//! The declarations ADR-007 adds: typed locals, typed block parameters,
//! instance-variable declarations, type aliases, tuple types and the newer
//! type names. The static checker enforces them before a program runs;
//! values that arrive at runtime, such as host arguments, are checked when
//! they arrive.

mod common;

use vibescript::{CallOptions, Engine, ErrorKind, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

/// The code and one-based line and column of each static diagnostic that
/// refuses `source`.
fn refused(source: &str) -> Vec<(String, (usize, usize))> {
    let Err(error) = vibescript::Engine::new().compile(source) else {
        panic!("{source} compiled");
    };
    error
        .diagnostics()
        .iter()
        .map(|d| {
            let before = &source[..d.span.start];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
            (d.code.to_string(), (line, column))
        })
        .collect()
}

fn diagnostics(expected: &[(&str, (usize, usize))]) -> Vec<(String, (usize, usize))> {
    expected
        .iter()
        .map(|(code, at)| ((*code).to_owned(), *at))
        .collect()
}

/// The runtime failure of `source` as a host call of `function` with
/// `args`, with its message.
fn call_failure(source: &str, function: &str, args: &[vibescript::Value]) -> (ErrorKind, String) {
    let error = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call(function, args, CallOptions::default())
        .expect_err(source);
    (error.kind, error.message)
}

/// A compile error as `vibes` prints it, with its one-based position.
fn compile_error(source: &str) -> String {
    let Err(error) = Engine::new().compile(source) else {
        panic!("{source} compiled");
    };
    let position = error.diagnostic.as_ref().unwrap().position;
    format!(
        "parse error at {}:{}: {}",
        position.line, position.column, error.message
    )
}

#[test]
fn typed_locals_take_their_declared_type() {
    for (source, expected) in [
        ("x: int = 1\nx = x + 2\nx", serde_json::json!(3)),
        (
            "label: string? = nil\nlabel = \"ready\"\nlabel",
            serde_json::json!("ready"),
        ),
        (
            "names: array<string> = []\nnames << \"Ada\"\nnames",
            serde_json::json!(["Ada"]),
        ),
        (
            "counts: hash<string, int> = {}\ncounts[\"a\"] = 1\ncounts",
            serde_json::json!({"a": 1}),
        ),
        (
            "total: number = 1\ntotal = 2.5\ntotal",
            serde_json::json!(2.5),
        ),
        ("x: int = 1\nx += 2\nx *= 3\nx", serde_json::json!(9)),
        ("x: int? = nil\nx = 4 if x == nil\nx", serde_json::json!(4)),
        (
            "def sum(items: array<int>) -> int\n  total: int = 0\n  items.each { |item| total += item }\n  for item in items\n    total = total + item\n  end\n  total\nend\nsum([1, 2])",
            serde_json::json!(6),
        ),
        (
            "a: int = 1\nb = 2\na, b = b, a\n[a, b]",
            serde_json::json!([2, 1]),
        ),
        (
            "x: int = 1\n[\"a\"].map { |x| x = \"shadowed\" }",
            serde_json::json!(["shadowed"]),
        ),
        ("type = 3\ntype + 1", serde_json::json!(4)),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn typed_locals_check_every_assignment() {
    for (source, expected) in [
        ("x: int = \"a\"", vec![("V0101", (1, 10))]),
        ("x: int = 1\nx = \"a\"", vec![("V0102", (2, 5))]),
        ("x: int = 1\nx += 1.5", vec![("V0102", (2, 6))]),
        (
            "x: int = 1\nx = x + \"\".length.to_f",
            vec![("V0102", (2, 5))],
        ),
        (
            "x: int? = nil\nx ||= \"a\"",
            vec![("V0104", (2, 1)), ("V0102", (2, 7))],
        ),
        (
            "x: int = 1\n[1, 2].each { |i| x = \"s\" }",
            vec![("V0102", (2, 23))],
        ),
        (
            "x: int = 1\n[[1]].each { |pair| [2].each { |i| x = nil } }",
            vec![("V0102", (2, 40))],
        ),
        (
            "x: int = 1\ny = 2\nx, y = \"a\", 1",
            vec![("V0102", (3, 1))],
        ),
        ("names: array<string> = [1]", vec![("V0101", (1, 25))]),
        ("x: Missing = 1", vec![("V0116", (1, 4))]),
        // `||=` tests a bool, and a declaration under a modifier may not run.
        ("x: int? = nil\nx ||= 4\nx", vec![("V0104", (2, 1))]),
        ("x: int = 1 if true\nx", vec![("V0202", (2, 1))]),
        // A `for` variable keeps the type of the local it assigns.
        ("x: int = 1\nfor x in [\"a\"]\nend", vec![("V0102", (2, 5))]),
    ] {
        assert_eq!(refused(source), diagnostics(&expected), "{source}");
    }
}

#[test]
fn malformed_typed_locals_are_positioned_parse_errors() {
    for (source, message) in [
        (
            "x: int\n",
            "parse error at 1:7: typed local x needs a value; write x: T = value",
        ),
        (
            "def f\n  total: array<int>\nend",
            "parse error at 2:20: typed local total needs a value; write total: T = value",
        ),
        (
            "x: [] = []",
            "parse error at 1:4: a tuple type needs at least one element type, as in [int, string]",
        ),
        (
            "x: array<int, string> = []",
            "parse error at 1:4: array type expects exactly 1 type argument",
        ),
        (
            "x: type<int, string> = int",
            "parse error at 1:4: type expects exactly 1 type argument",
        ),
        (
            "x: [int string] = []",
            "parse error at 1:9: expected \"]\", got identifier",
        ),
        // What only resembles a declaration keeps the error it always had.
        ("x: Missing", "parse error at 1:2: unexpected token \":\""),
        ("x:int = 1", "parse error at 1:2: unexpected token \":\""),
        ("id : a", "parse error at 1:4: unexpected token \":\""),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

const KEEP: &str = "def keep(items: array<int>, &block: int -> bool) -> array<int>
  kept: array<int> = []
  items.each { |item| kept << item if yield(item) }
  kept
end
";

#[test]
fn typed_block_parameters_check_yields_and_results() {
    for (source, expected) in [
        (
            format!("{KEEP}keep([1, 2, 3]) {{ |i| i > 1 }}"),
            serde_json::json!([2, 3]),
        ),
        (
            "def pairs(&block: [string, int] -> string) -> string\n  yield [\"a\", 1]\nend\npairs { |pair| pair[0] }".into(),
            serde_json::json!("a"),
        ),
        (
            "def maybe(msg: string, &block?: string -> nil) -> bool\n  yield msg if block_given?\n  block_given?\nend\n[maybe(\"m\"), maybe(\"m\") { |m| nil }]".into(),
            serde_json::json!([false, true]),
        ),
        (
            "def twice(&block: ())\n  yield\n  yield\nend\nn = 0\ntwice { n += 1 }\nn".into(),
            serde_json::json!(2),
        ),
        (
            "def name(&block: () -> string | nil) -> string?\n  yield\nend\n[name { \"a\" }, name { nil }]".into(),
            serde_json::json!(["a", null]),
        ),
        (
            "def f(&block: int) -> array<int>\n  [1].map { |block| block + 1 }\nend\nf { |x| x }".into(),
            serde_json::json!([2]),
        ),
    ] {
        assert_eq!(evaluate(&source), expected, "{source}");
    }
    for (source, expected) in [
        (format!("{KEEP}keep([1]) {{ |i| i }}"), ("V0101", (6, 17))),
        (
            "def f(&block: (string, int))\n  yield 1, 2\nend\nf { |a, b| a }".into(),
            ("V0101", (2, 9)),
        ),
        (
            "def f(&block: (string, int))\n  yield \"a\", \"b\"\nend\nf { |a, b| a }".into(),
            ("V0101", (2, 14)),
        ),
        (
            "def f(&block: [string, int])\n  yield [\"a\"]\nend\nf { |pair| pair }".into(),
            ("V0101", (2, 9)),
        ),
        // Without a result type the block's value is discarded, so it cannot
        // be used.
        (
            "def each_pair(h: hash<string, int>, &block: (string, int))\n  h.keys.map { |k| yield k, h.fetch(k) }\nend\neach_pair({ a: 1 }) { |k, v| v }".into(),
            ("V0119", (2, 20)),
        ),
        // A required block must be given.
        ("def f(&block: int)\n  yield 1\nend\nf".into(), ("V0304", (4, 1))),
    ] {
        assert_eq!(refused(&source), diagnostics(&[expected]), "{source}");
    }
    // A host that calls without a block gets the runtime error.
    assert_eq!(
        call_failure("def f(&block: int)\n  yield 1\nend", "f", &[]),
        (ErrorKind::Argument, "no block given".to_owned())
    );
}

#[test]
fn block_parameter_names_are_declarations_only() {
    for (source, message) in [
        (
            "def f(&block: int)\n  block\nend",
            "parse error at 2:3: block parameter block is not a value; run the block with `yield`, and ask `block_given?` when it is optional",
        ),
        (
            "def f(&each: int)\n  g(each)\nend",
            "parse error at 2:5: block parameter each is not a value; run the block with `yield`, and ask `block_given?` when it is optional",
        ),
        (
            "def f(&block?: int)\n  block.call(1)\nend",
            "parse error at 2:3: block parameter block is not a value; run the block with `yield`, and ask `block_given?` when it is optional",
        ),
        (
            "def f(&block: int, x: int)\nend",
            "parse error at 1:18: the block parameter must be the last parameter",
        ),
        (
            "def f(block: int, &block: int)\nend",
            "parse error at 1:20: duplicate parameter block",
        ),
        (
            "def f(&block: (int, string)\nend",
            "parse error at 2:1: expected \")\", got 'end'",
        ),
        // An untyped block parameter keeps its refusal.
        (
            "def f(&block)\nend",
            "parse error at 1:7: block capture parameters are not supported; a block is not a value. Run the caller's block with `yield`, and ask `block_given?` when it is optional",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn instance_variable_declarations_give_each_instance_its_default() {
    let counter = "class Counter
  @count: int = 0
  @items: array<int> = []
  @name: string
  def initialize(name: string)
    @name = name
  end
  def add(item: int) -> array<any>
    @count += 1
    @items << item
    [@name, @count, @items]
  end
end
";
    assert_eq!(
        evaluate(&format!(
            "{counter}a = Counter.new(\"a\")\nb = Counter.new(\"b\")\na.add(1)\n[a.add(2), b.add(3)]"
        )),
        serde_json::json!([["a", 2, [1, 2]], ["b", 1, [3]]])
    );
    // Defaults apply before `initialize` binds, so a parameter wins.
    assert_eq!(
        evaluate(
            "class P\n  @x: int = 1\n  def initialize(@x: int)\n  end\n  def x -> int\n    @x\n  end\nend\n[P.new(5).x]"
        ),
        serde_json::json!([5])
    );
    assert_eq!(
        evaluate("class P\n  @x: int = 1\n  def x -> int\n    @x\n  end\nend\nP.new.x"),
        serde_json::json!(1)
    );
    for (source, at) in [
        (format!("{counter}Counter.new(1)"), (14, 13)),
        (
            "class C\n  @count: int = 0\n  def bump\n    @count = \"x\"\n  end\nend\nC.new.bump".into(),
            (4, 14),
        ),
        (
            "class C\n  @items: array<int> = []\n  def add\n    @items << \"x\"\n  end\nend\nC.new.add".into(),
            (4, 12),
        ),
        (
            "class C\n  @name: string\n  def initialize\n    @name = 1\n  end\nend\nC.new".into(),
            (4, 13),
        ),
        ("class C\n  @x: int = \"no\"\nend\nC.new".into(), (2, 13)),
    ] {
        assert_eq!(refused(&source), diagnostics(&[("V0101", at)]), "{source}");
    }
    for (source, message) in [
        (
            "class C\n  @x: int\n  @x: string\nend",
            "parse error at 3:3: duplicate instance variable declaration @x",
        ),
        // A default starts on the declaration's line.
        (
            "class C\n  @x: int\n  = 1\nend",
            "parse error at 3:3: unexpected token \"=\"",
        ),
        // A module has no instances, so the declaration stays an error.
        (
            "module M\n  @x: int = 1\nend",
            "parse error at 2:5: unexpected token \":\"",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn class_variable_declarations_assign_in_body_order() {
    let source = "class C\n  @@base: int = 2\n  @@next: int = @@base + 1\n  def self.bump -> int\n    @@next += 1\n  end\nend\nmodule M\n  @@names: array<string> = []\n  def self.add(name: string) -> array<string>\n    @@names << name\n  end\nend\n[C.bump, C.bump, M.add(\"a\"), M.add(\"b\")]";
    assert_eq!(
        evaluate(source),
        serde_json::json!([4, 5, ["a"], ["a", "b"]])
    );
    for (source, message) in [
        (
            "class C\n  @@x: int\nend",
            "parse error at 2:11: class variable @@x needs a value; write @@x: T = value",
        ),
        // The value starts on the declaration's line.
        (
            "class C\n  @@x: int\n  = 1\nend",
            "parse error at 2:11: class variable @@x needs a value; write @@x: T = value",
        ),
        (
            "module M\n  @@x: int = 1\n  @@x: int = 2\nend",
            "parse error at 3:3: duplicate class variable declaration @@x",
        ),
        // Only a class or module body declares one.
        ("@@x: int = 1", "parse error at 1:4: unexpected token \":\""),
        // A member that only resembles a declaration keeps its old error.
        (
            "class C\n  @@x: = 1\nend",
            "parse error at 2:6: unexpected token \":\"",
        ),
        (
            "class C\n  @@x: 1\nend",
            "parse error at 2:6: unexpected token \":\"",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn type_aliases_name_types_anywhere() {
    for (source, expected) in [
        (
            "def total(rewards: array<Reward>) -> int\n  rewards.map { |r| r[\"points\"] }.sum\nend\ntype Reward = { id: string, points: int }\ntotal([{ id: \"a\", points: 3 }, { id: \"b\", points: 4 }])",
            serde_json::json!(7),
        ),
        (
            "type Id = string\ntype Ids = array<Id>\nids: Ids = [\"a\"]\nids",
            serde_json::json!(["a"]),
        ),
        ("type Id = string\nx: Id? = nil\nx", serde_json::json!(null)),
        (
            "type Reward = { id: string }\n[JSON.parse_as(\"{\\\"id\\\": \\\"a\\\"}\", Reward), JSON.parse_as(\"[{\\\"id\\\": \\\"b\\\"}]\", array<Reward>)]",
            serde_json::json!([{"id": "a"}, [{"id": "b"}]]),
        ),
        (
            "module Geo\n  type Point = [float, float]\n  def self.origin -> Point\n    [0.5, 1.5]\n  end\nend\nGeo.origin",
            serde_json::json!([0.5, 1.5]),
        ),
        (
            "type Amount = int\nclass Wallet\n  type Entry = [string, Amount]\n  @entries: array<Entry> = []\n  def add(entry: Entry) -> array<Entry>\n    @entries << entry\n  end\nend\nWallet.new.add([\"a\", 1])",
            serde_json::json!([["a", 1]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
    // The signature table's aliases name types like the builtin names.
    assert_eq!(
        evaluate(
            "def f(x: comparable) -> comparable\n  x\nend\n[f(1), f(\"a\"), f(:b), f(2.5), f(90.seconds).as(duration).to_i]"
        ),
        serde_json::json!([1, "a", "b", 2.5, 90])
    );
    for (source, expected) in [
        ("def f(x: comparable)\nend\nf([1])", vec![("V0101", (3, 3))]),
        (
            "type Reward = { points: int }\ndef f(r: Reward)\nend\nf({ points: \"x\" })",
            vec![("V0101", (4, 13))],
        ),
        (
            "type Id = string\nx: Id? = nil\nx = 1",
            vec![("V0102", (3, 5))],
        ),
        (
            "type Choice = int | string\ndef f(x: Choice?)\nend\nf(1.5)",
            vec![("V0101", (4, 3))],
        ),
        (
            "module Geo\n  type Point = [float, float]\n  def self.bad -> Point\n    [0, 0]\n  end\nend\nGeo.bad",
            vec![("V0101", (4, 6)), ("V0101", (4, 9))],
        ),
    ] {
        assert_eq!(refused(source), diagnostics(&expected), "{source}");
    }
    // Host arguments are checked against what the aliases name.
    for (source, argument, message) in [
        (
            "def f(x: comparable)\nend",
            vibescript::Value::array(vec![vibescript::Value::int(1)]),
            "argument x expected number | string | symbol | time | duration | money, got array<int>",
        ),
        (
            "type Reward = { points: int }\ndef f(r: Reward)\nend",
            vibescript::Value::hash(vec![(b"points".to_vec(), vibescript::Value::bytes("x"))]),
            "argument r expected { points: int }, got { points: string }",
        ),
        (
            "type Choice = int | string\ndef f(x: Choice?)\nend",
            vibescript::Value::float(1.5),
            "argument x expected int | string | nil, got float",
        ),
    ] {
        assert_eq!(
            call_failure(source, "f", &[argument]),
            (ErrorKind::Type, message.to_owned()),
            "{source}"
        );
    }
    for (source, message) in [
        (
            "type A = A?\n1",
            "parse error at 1:6: type alias A refers to itself",
        ),
        (
            "type A = B\ntype B = array<C>\ntype C = { a: A }\n1",
            "parse error at 1:6: type alias cycle: A -> B -> C -> A",
        ),
        (
            "type A = int\ntype A = string",
            "parse error at 2:6: duplicate type alias A",
        ),
        (
            "type int = string",
            "parse error at 1:6: type alias int conflicts with a built-in type",
        ),
        (
            "def f\n  type A = int\nend",
            "parse error at 2:3: type aliases are only supported at the top level and in module or class bodies",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn tuple_types_are_arrays_of_exactly_their_elements() {
    let enums = "enum Status\n  Draft\n  Done\nend\n";
    // A tuple-typed parameter without a default is written through an
    // alias: the checker reads `x: [int, string]` as the removed keyword
    // form.
    for (source, expected) in [
        (
            "type Pair = [int, string]\ndef f(x: Pair) -> [int, string]\n  x\nend\nf([1, \"a\"])"
                .to_owned(),
            serde_json::json!([1, "a"]),
        ),
        (
            "def f(x: [int, string]?) -> [int, string]?\n  x\nend\nf(nil)".to_owned(),
            serde_json::json!(null),
        ),
        (
            "def f(pair: [int, int] = [1, 2]) -> array<int>\n  pair\nend\nf()".to_owned(),
            serde_json::json!([1, 2]),
        ),
        (
            format!(
                "{enums}type Entry = [Status, int]\ndef f(x: Entry) -> string\n  x[0].name\nend\nf([:done, 1])"
            ),
            serde_json::json!("Done"),
        ),
        (
            "pairs: array<[int, string]> = [[1, \"a\"]]\npairs.map { |p: [int, string]| p[1] }"
                .to_owned(),
            serde_json::json!(["a"]),
        ),
    ] {
        assert_eq!(evaluate(&source), expected, "{source}");
    }
    // A bracket whose leaves are not all types was a keyword default,
    // which is removed.
    for (source, at) in [
        (
            "def f(a: int, b: int, pair: [a, b])\n  pair\nend\nf(1, 2)",
            (1, 23),
        ),
        ("def f(x: [])\n  x\nend\nf()", (1, 7)),
    ] {
        assert_eq!(refused(source), diagnostics(&[("V0414", at)]), "{source}");
    }
    for (argument, message) in [
        (
            vibescript::Value::array(vec![vibescript::Value::int(1), vibescript::Value::int(2)]),
            "argument pair expected [int, string], got array<int>",
        ),
        (
            vibescript::Value::array(vec![
                vibescript::Value::int(1),
                vibescript::Value::bytes("a"),
                vibescript::Value::int(3),
            ]),
            "argument pair expected [int, string], got array<int | string>",
        ),
        (
            vibescript::Value::hash(vec![(b"a".to_vec(), vibescript::Value::int(1))]),
            "argument pair expected [int, string], got { a: int }",
        ),
    ] {
        assert_eq!(
            call_failure(
                "type Pair = [int, string]\ndef f(pair: Pair)\nend",
                "f",
                &[argument]
            ),
            (ErrorKind::Type, message.to_owned())
        );
    }
    for (source, expected) in [
        (
            "type Pair = [int, string]\ndef f(pair: Pair)\nend\nf([1, 2])",
            vec![("V0101", (4, 7))],
        ),
        (
            "def f -> [int, [string, bool]]\n  [1, [\"a\", 2]]\nend\nf",
            vec![("V0101", (2, 13))],
        ),
    ] {
        assert_eq!(refused(source), diagnostics(&expected), "{source}");
    }
    for (source, message) in [
        (
            "def f -> []\nend",
            "parse error at 1:10: a tuple type needs at least one element type, as in [int, string]",
        ),
        (
            "def f -> [int string]\nend",
            "parse error at 1:15: expected \"]\", got identifier",
        ),
        (
            "def f(x: array<int, string>)\nend",
            "parse error at 1:10: array type expects exactly 1 type argument",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn newer_type_names_validate_their_values() {
    let error = "e = begin\n  raise \"bad\"\nrescue => err\n  err\nend\n";
    assert_eq!(
        evaluate(&format!(
            "{error}def f(r: regex, m: match_data, e: error, t: type<array<int>>) -> array<string?>\n  [r.source, m[0], e.message]\nend\nf(/a/, \"abc\".match(\"b\").as(match_data), e, array<int>)"
        )),
        serde_json::json!(["a", "b", "bad"])
    );
    for (source, argument, message) in [
        (
            "def f(r: regex)\nend",
            vibescript::Value::bytes("a"),
            "argument r expected regex, got string",
        ),
        (
            "def f(m: match_data)\nend",
            vibescript::Value::hash(vec![(
                b"captures".to_vec(),
                vibescript::Value::array(vec![]),
            )]),
            "argument m expected match_data, got { captures: array<empty> }",
        ),
        (
            "def f(e: error)\nend",
            vibescript::Value::bytes("x"),
            "argument e expected error, got string",
        ),
        (
            "def f(t: type<int>)\nend",
            vibescript::Value::int(1),
            "argument t expected type<int>, got int",
        ),
    ] {
        assert_eq!(
            call_failure(source, "f", &[argument]),
            (ErrorKind::Type, message.to_owned()),
            "{source}"
        );
    }
    for (source, expected) in [
        ("def f(r: regex)\nend\nf(\"a\")", ("V0101", (3, 3))),
        (
            "def f(m: match_data)\nend\nf({ captures: [] })",
            ("V0101", (3, 3)),
        ),
        ("def f(e: error)\nend\nf(\"x\")", ("V0101", (3, 3))),
        ("def f(t: type<int>)\nend\nf(1)", ("V0101", (3, 3))),
        (
            "def f(m: match_data?) -> match_data\n  m\nend\nf(\"a\".match(\"z\"))",
            ("V0107", (2, 3)),
        ),
    ] {
        assert_eq!(refused(source), diagnostics(&[expected]), "{source}");
    }
    assert_eq!(
        compile_error("def f(t: type<int, string>)\nend"),
        "parse error at 1:10: type expects exactly 1 type argument"
    );
    // The newer names are lowercase only, so a class named Error keeps naming itself.
    assert_eq!(
        evaluate("class Error\nend\ndef f(e: Error) -> bool\n  true\nend\nf(Error.new)"),
        serde_json::json!(true)
    );
    // A rescued error bound to `error` is still a value where a type could be meant.
    assert_eq!(
        evaluate(
            "def show(x: error) -> string\n  x.message\nend\nbegin\n  raise \"bad\"\nrescue => error\n  show(error)\nend"
        ),
        serde_json::json!("bad")
    );
}

#[test]
fn the_gradual_checker_accepts_the_new_declarations() {
    let source = format!(
        "type Reward = {{ id: string, points: int }}
class Counter
  @count: int = 0
  def bump -> int
    @count += 1
  end
end
{KEEP}def run -> int
  rewards: array<Reward> = [{{ id: \"a\", points: 2 }}]
  pair: [int, string] = [1, \"a\"]
  kept = keep([1, 2]) {{ |i| i > 1 }}
  rewards[0][\"points\"] + pair[0] + kept.length + Counter.new.bump
end
"
    );
    let script = common::gradual_engine().compile(&source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(5)
    );
}

#[test]
fn a_value_may_follow_type_arguments_without_a_space() {
    let source = "class C\n  @@all: array<int>=[1]\n  @some: hash<string, int>={}\n  \
                  def self.all -> array<int>\n    @@all\n  end\n  def some -> hash<string, int>\n    @some\n  end\nend\n\
                  module M\n  @@nested: array<array<int>>=[[2]]\n  def self.nested -> array<array<int>>\n    @@nested\n  end\nend\n\
                  def f(n: int, z: array<int>=[3]) -> array<int>\n  z\nend\n\
                  x: array<string>=[\"a\"]\n[x, C.all, C.new.some, M.nested, f(1)]";
    let engine = Engine::new();
    let result = engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{error}"))
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
    assert_eq!(value, serde_json::json!([["a"], [1], {}, [[2]], [3]]));
    // The split `>` and `=` are what tools see.
    let tokens = vibescript::tooling::tokens("x: array<int>=[]\n").unwrap();
    let operators: Vec<_> = tokens
        .iter()
        .filter_map(|token| match token.kind {
            vibescript::tooling::TokenKind::Operator(op) => Some((op, token.span.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(operators, [("<", 8..9), (">", 12..13), ("=", 13..14)]);
    // A comparison is still one operator.
    assert_eq!(evaluate("a = 2\nb = 1\na >= b"), serde_json::json!(true));
}

#[test]
fn typed_constants_keep_their_declared_type() {
    let source = "module Limits\n  MAX: int = 3\n  NAMES: array<string> = []\n  \
                  def self.total -> int\n    MAX + NAMES.length\n  end\nend\n\
                  class Store\n  COUNTS: hash<string, int> = {}\n  LABEL: string? = nil\n  \
                  def self.size -> int\n    COUNTS.length\n  end\nend\n\
                  TOP: int | string = 1\n[Limits.total, Limits::MAX, Store.size, Store::LABEL, TOP]";
    let engine = Engine::new();
    let result = engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{error}"))
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
    assert_eq!(value, serde_json::json!([3, 3, 0, null, 1]));
    // The checker refuses a value, or a later assignment, of another type,
    // and reads the constant with its declared type.
    assert_eq!(
        refused("module M\n  MAX: int = \"a\"\nend\n"),
        [("V0101".to_owned(), (2, 14))]
    );
    assert_eq!(
        refused("module M\n  MAX: int = 3\n  MAX = \"b\"\nend\n"),
        [("V0101".to_owned(), (3, 9))]
    );
    assert_eq!(
        refused("class C\n  MAX: int? = nil\n  def self.max -> int\n    MAX\n  end\nend\n"),
        [("V0107".to_owned(), (4, 5))]
    );
}

#[test]
fn casts_name_classes_enums_and_scoped_aliases_inside_types() {
    let source = "enum Status\n  Draft\n  Done\nend\nclass Box\n  getter size: int\n  \
                  def initialize(@size: int)\n  end\nend\nmodule Shapes\n  type Point = { x: int }\nend\n\
                  boxes: any = [Box.new(2), Box.new(3)]\nheld: any = { box: Box.new(4), status: Status::Done }\n\
                  points: any = [{ x: 1 }]\npair: any = [Box.new(5), Status::Draft]\n\
                  [boxes.as(array<Box>).length, held.as({ box: Box, status: Status })[\"box\"].size,\n \
                  points.as(array<Shapes::Point>).length, pair.as([Box, Status])[0].size,\n \
                  (boxes.as(array<Status>) rescue \"refused\")]";
    let engine = Engine::new();
    let result = engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{error}"))
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
    assert_eq!(value, serde_json::json!([2, 4, 1, 5, "refused"]));
    // `JSON.parse_as` reads enum members by their symbols inside the type.
    let parsed = "enum Status\n  Draft\n  Done\nend\n\
                  a = JSON.parse_as(\"{\\\"s\\\": \\\"draft\\\"}\", { s: Status? })\n\
                  b = JSON.parse_as(\"[\\\"done\\\"]\", array<Status>)\n\
                  [a[\"s\"] == Status::Draft, b.fetch(0) == Status::Done]";
    let engine = Engine::new();
    let result = engine
        .compile(parsed)
        .unwrap_or_else(|error| panic!("{error}"))
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
    assert_eq!(value, serde_json::json!([true, true]));
    // Elsewhere, a braced group that names a class is a hash of values.
    assert_eq!(
        evaluate("class Box\nend\nh = { kind: Box }\nh.length"),
        serde_json::json!(1)
    );
}
