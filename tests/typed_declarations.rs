//! The declarations ADR-007 adds, enforced at runtime like parameter
//! annotations: typed locals, typed block parameters, instance-variable
//! declarations, type aliases, tuple types and the newer type names.

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

/// The runtime failure of `source`, with its one-based line and column.
fn failure(source: &str) -> (ErrorKind, String, (usize, usize)) {
    let error = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .expect_err(source);
    let position = error.diagnostic.as_ref().unwrap().position;
    (error.kind, error.message, (position.line, position.column))
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
        ("x: int? = nil\nx ||= 4\nx", serde_json::json!(4)),
        (
            "def sum(items: array<int>) -> int\n  total: int = 0\n  items.each { |item| total += item }\n  for item in items\n    total = total + item\n  end\n  total\nend\nsum([1, 2])",
            serde_json::json!(6),
        ),
        (
            "a: int = 1\nb = 2\na, b = b, a\n[a, b]",
            serde_json::json!([2, 1]),
        ),
        (
            "x: int = 1\n[1].map { |x| x = \"shadowed\" }",
            serde_json::json!(["shadowed"]),
        ),
        ("x: int = 1 if true\nx", serde_json::json!(1)),
        ("type = 3\ntype + 1", serde_json::json!(4)),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn typed_locals_check_every_assignment() {
    for (source, message, at) in [
        (
            "x: int = \"a\"",
            "local variable x expected int, got string",
            (1, 1),
        ),
        (
            "x: int = 1\nx = \"a\"",
            "local variable x expected int, got string",
            (2, 1),
        ),
        (
            "x: int = 1\nx += 1.5",
            "local variable x expected int, got float",
            (2, 1),
        ),
        (
            "x: int = 1\nx = x + \"\".length.to_f",
            "local variable x expected int, got float",
            (2, 1),
        ),
        (
            "x: int? = nil\nx ||= \"a\"",
            "local variable x expected int?, got string",
            (2, 1),
        ),
        (
            "x: int = 1\n[1, 2].each { |i| x = \"s\" }",
            "local variable x expected int, got string",
            (2, 19),
        ),
        (
            "x: int = 1\n[[1]].each { |pair| [2].each { |i| x = nil } }",
            "local variable x expected int, got nil",
            (2, 36),
        ),
        (
            "x: int = 1\nfor x in [\"a\"]\nend",
            "local variable x expected int, got string",
            (2, 5),
        ),
        (
            "x: int = 1\ny = 2\nx, y = \"a\", 1",
            "local variable x expected int, got string",
            (3, 1),
        ),
        (
            "names: array<string> = [1]",
            "local variable names expected array<string>, got array<int>",
            (1, 1),
        ),
        (
            "x: Missing = 1",
            "local variable x type check failed: unknown type Missing",
            (1, 1),
        ),
    ] {
        let (kind, actual, position) = failure(source);
        assert_eq!(kind, ErrorKind::Type, "{source}");
        assert_eq!((actual.as_str(), position), (message, at), "{source}");
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
            "def each_pair(h: hash<string, int>, &block: (string, int))\n  h.keys.map { |k| yield k, h.fetch(k) }\nend\neach_pair({ a: 1 }) { |k, v| v }".into(),
            // Without a result type the block's value is discarded.
            serde_json::json!([null]),
        ),
        (
            "def pairs(&block: [string, int] -> string)\n  yield [\"a\", 1]\nend\npairs { |pair| pair[0] }".into(),
            serde_json::json!("a"),
        ),
        (
            "def maybe(msg: string, &block?: string -> nil)\n  yield msg if block_given?\n  block_given?\nend\n[maybe(\"m\"), maybe(\"m\") { |m| nil }]".into(),
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
            "def f(&block: int)\n  [1].map { |block| block + 1 }\nend\nf { |x| x }".into(),
            serde_json::json!([2]),
        ),
    ] {
        assert_eq!(evaluate(&source), expected, "{source}");
    }
    for (source, message, at) in [
        (
            format!("{KEEP}keep([1]) {{ |i| i }}"),
            "block result expected bool, got int",
            (3, 39),
        ),
        (
            "def f(&block: (string, int))\n  yield 1, 2\nend\nf { |a, b| a }".into(),
            "yield argument 1 expected string, got int",
            (2, 3),
        ),
        (
            "def f(&block: (string, int))\n  yield \"a\", \"b\"\nend\nf { |a, b| a }".into(),
            "yield argument 2 expected int, got string",
            (2, 3),
        ),
        (
            "def f(&block: [string, int])\n  yield [\"a\"]\nend\nf { |pair| pair }".into(),
            "yield argument 1 expected [string, int], got array<string>",
            (2, 3),
        ),
    ] {
        let (kind, actual, position) = failure(&source);
        assert_eq!(kind, ErrorKind::Type, "{source}");
        assert_eq!((actual.as_str(), position), (message, at), "{source}");
    }
    // A required block that is missing keeps its runtime error.
    let (kind, message, _) = failure("def f(&block: int)\n  yield 1\nend\nf");
    assert_eq!(
        (kind, message.as_str()),
        (ErrorKind::Argument, "no block given")
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
  def add(item)
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
            "class P\n  @x: int = 1\n  def initialize(@x)\n  end\n  def x\n    @x\n  end\nend\n[P.new(5).x]"
        ),
        serde_json::json!([5])
    );
    assert_eq!(
        evaluate("class P\n  @x: int = 1\n  def x\n    @x\n  end\nend\nP.new.x"),
        serde_json::json!(1)
    );
    for (source, message, at) in [
        (
            format!("{counter}Counter.new(1)"),
            "argument name expected string, got int",
            (14, 1),
        ),
        (
            "class C\n  @count: int = 0\n  def bump\n    @count = \"x\"\n  end\nend\nC.new.bump".into(),
            "instance variable @count expected int, got string",
            (4, 5),
        ),
        (
            "class C\n  @items: array<int> = []\n  def add\n    @items << \"x\"\n  end\nend\nC.new.add".into(),
            "instance variable @items expected array<int>, got array<string>",
            (4, 13),
        ),
        (
            "class C\n  @name: string\n  def initialize\n    @name = 1\n  end\nend\nC.new".into(),
            "instance variable @name expected string, got int",
            (4, 5),
        ),
        (
            "class C\n  @x: int = \"no\"\nend\nC.new".into(),
            "instance variable @x expected int, got string",
            (2, 3),
        ),
    ] {
        let (kind, actual, position) = failure(&source);
        assert_eq!(kind, ErrorKind::Type, "{source}");
        assert_eq!((actual.as_str(), position), (message, at), "{source}");
    }
    for (source, message) in [
        (
            "class C\n  @x: int\n  @x: string\nend",
            "parse error at 3:3: duplicate instance variable declaration @x",
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
    for (source, message) in [
        (
            "type Reward = { points: int }\ndef f(r: Reward)\nend\nf({ points: \"x\" })",
            "argument r expected { points: int }, got { points: string }",
        ),
        (
            "type Id = string\nx: Id? = nil\nx = 1",
            "local variable x expected string?, got int",
        ),
        (
            "type Choice = int | string\ndef f(x: Choice?)\nend\nf(1.5)",
            "argument x expected int | string | nil, got float",
        ),
        (
            "module Geo\n  type Point = [float, float]\n  def self.bad -> Point\n    [0, 0]\n  end\nend\nGeo.bad",
            "return value for bad expected [float, float], got array<int>",
        ),
    ] {
        let (kind, actual, _) = failure(source);
        assert_eq!(
            (kind, actual.as_str()),
            (ErrorKind::Type, message),
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
    for (source, expected) in [
        (
            "def f(x: [int, string]) -> [int, string]\n  x\nend\nf([1, \"a\"])".to_owned(),
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
        // A bracket whose leaves are not all types stays a default.
        (
            "def f(a, b, pair: [a, b])\n  pair\nend\nf(1, 2)".to_owned(),
            serde_json::json!([1, 2]),
        ),
        (
            "def f(x: [])\n  x\nend\nf()".to_owned(),
            serde_json::json!([]),
        ),
        (
            format!("{enums}def f(x: [Status, int]) -> string\n  x[0].name\nend\nf([:done, 1])"),
            serde_json::json!("Done"),
        ),
        (
            "[[1, \"a\"]].map { |p: [int, string]| p[1] }".to_owned(),
            serde_json::json!(["a"]),
        ),
    ] {
        assert_eq!(evaluate(&source), expected, "{source}");
    }
    for (source, message) in [
        (
            "def f(pair: [int, string])\nend\nf([1, 2])",
            "argument pair expected [int, string], got array<int>",
        ),
        (
            "def f(pair: [int, string])\nend\nf([1, \"a\", 3])",
            "argument pair expected [int, string], got array<int | string>",
        ),
        (
            "def f(pair: [int, string])\nend\nf({ a: 1 })",
            "argument pair expected [int, string], got { a: int }",
        ),
        (
            "def f -> [int, [string, bool]]\n  [1, [\"a\", 2]]\nend\nf",
            "return value for f expected [int, [string, bool]], got array<array<int | string> | int>",
        ),
    ] {
        let (kind, actual, _) = failure(source);
        assert_eq!(
            (kind, actual.as_str()),
            (ErrorKind::Type, message),
            "{source}"
        );
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
            "{error}def f(r: regex, m: match_data, e: error, t: type<array<int>>)\n  [r.source, m[0], e.message]\nend\nf(/a/, \"abc\".match(\"b\"), e, array<int>)"
        )),
        serde_json::json!(["a", "b", "bad"])
    );
    for (source, message) in [
        (
            "def f(r: regex)\nend\nf(\"a\")",
            "argument r expected regex, got string",
        ),
        (
            "def f(m: match_data)\nend\nf({ captures: [] })",
            "argument m expected match_data, got { captures: array<empty> }",
        ),
        (
            "def f(e: error)\nend\nf(\"x\")",
            "argument e expected error, got string",
        ),
        (
            "def f(t: type<int>)\nend\nf(1)",
            "argument t expected type<int>, got int",
        ),
        (
            "def f(m: match_data?) -> match_data\n  m\nend\nf(\"a\".match(\"z\"))",
            "return value for f expected match_data, got nil",
        ),
    ] {
        let (kind, actual, _) = failure(source);
        assert_eq!(
            (kind, actual.as_str()),
            (ErrorKind::Type, message),
            "{source}"
        );
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
            "def show(x)\n  x.message\nend\nbegin\n  raise \"bad\"\nrescue => error\n  show(error)\nend"
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
    let script = Engine::new().compile(&source).unwrap();
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
