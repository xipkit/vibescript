//! Function signatures: every parameter declares its type, and a function
//! that returns a value declares `-> T`.

use super::support::{clean, codes, error, spanned};

#[test]
fn typed_parameters_and_results_check_clean() {
    clean("def add(a: int, b: int = 1) -> int\n  a + b\nend\n");
    clean(
        "def greet(name: string, times: int = 1, **opts: hash<string, any>) -> string\n  name * times\nend\n",
    );
    clean("def total(*values: array<int>) -> int\n  values.length\nend\n");
    clean("def label(name: string, *, loud: bool) -> string\n  loud ? name.upcase : name\nend\n");
    clean(
        "def label(name: string, *, loud: bool = false, suffix: string? = nil) -> string\n  loud ? name.upcase : name\nend\n",
    );
    clean(
        "def join(*parts: array<string>, sep: string = \",\") -> string\n  parts.join(sep)\nend\n",
    );
}

#[test]
fn a_parameter_without_a_type_is_an_error() {
    let source = "def add(a, b: int) -> int\n  b\nend\n";
    let diagnostic = error(source, "V0118", "parameter `a` of `add` has no type");
    assert_eq!(spanned(source, &diagnostic), "a");
}

#[test]
fn a_function_without_a_result_type_returns_nil() {
    // The final expression is evaluated for effect only.
    clean("def log(message: string)\n  message.upcase\nend\n");
    clean("def log(message: string)\n  return\nend\n");
    let source = "def log(message: string)\n  return message\nend\n";
    error(source, "V0117", "declares no result type");
}

#[test]
fn a_call_of_a_function_without_a_result_type_gives_nil() {
    let source = "def log(message: string)\n  message.upcase\nend\n\
                  class Box\n  def poke(n: int)\n    n + 1\n  end\nend\n\
                  def poked -> any\n  Box.new.poke(1)\nend\n";
    let script = vibescript::Engine::new().compile(source).unwrap();
    let options = vibescript::CallOptions::default;
    let logged = script.call("log", &[vibescript::Value::bytes("a")], options());
    assert_eq!(logged.unwrap().value.type_name(), "nil");
    let poked = script.call("poked", &[], options()).unwrap();
    assert_eq!(poked.value.type_name(), "nil");
    // The top-level statements still give their last value.
    let top = vibescript::Engine::new().compile("x = 2\nx + 1\n").unwrap();
    assert_eq!(top.run(options()).unwrap().value.as_int(), Some(3));
}

#[test]
fn the_body_must_produce_the_declared_result() {
    error(
        "def name(id: int) -> string\n  id\nend\n",
        "V0101",
        "`name` returns string, found int",
    );
    error(
        "def name(id: int) -> string\n  return id if id > 3\n  \"x\"\nend\n",
        "V0101",
        "returns string, found int",
    );
    error("def name -> string\nend\n", "V0101", "body is empty");
    clean("def maybe -> string?\nend\n");
}

#[test]
fn every_branch_of_the_final_statement_is_checked_where_it_ends() {
    let source = "def pick(flag: bool) -> int\n  if flag\n    1\n  else\n    \"two\"\n  end\nend\n";
    let diagnostic = error(source, "V0101", "returns int, found string");
    assert_eq!(spanned(source, &diagnostic), "\"two\"");
    clean("def pick(flag: bool) -> int\n  if flag\n    return 1\n  end\n  2\nend\n");
}

#[test]
fn an_if_without_else_may_return_nil() {
    error(
        "def pick(flag: bool) -> int\n  if flag\n    1\n  end\nend\n",
        "V0101",
        "has no `else`",
    );
    clean("def pick(flag: bool) -> int?\n  if flag\n    1\n  end\nend\n");
}

#[test]
fn loops_that_only_return_leave_no_final_value() {
    clean(
        "def first_even(values: array<int>) -> int\n  index = 0\n  while true\n    value = values.fetch(index)\n    return value if value.even?\n    index += 1\n  end\nend\n",
    );
    error(
        "def find(limit: int) -> int\n  for value in 1..limit\n    return value if value > 3\n  end\n  nil\nend\n",
        "V0101",
        "returns int, found nil",
    );
}

#[test]
fn a_while_loop_gives_the_value_break_gives() {
    clean("def f -> int\n  while true\n    break 3\n  end\nend\n");
    codes(
        "def f -> int\n  while 1 > 2\n    break 3\n  end\nend\n",
        &["V0107"],
    );
}

#[test]
fn an_unknown_type_name_is_reported_where_it_is_written() {
    let source = "def run(value: Missing) -> int\n  1\nend\n";
    let diagnostic = error(source, "V0116", "unknown type `Missing`");
    assert_eq!(spanned(source, &diagnostic), "Missing");
}

#[test]
fn type_aliases_are_transparent() {
    clean(
        "type Reward = { id: string, points: int }\ndef points(reward: Reward) -> int\n  reward[\"points\"]\nend\ndef run -> int\n  points({ id: \"a\", points: 3 })\nend\n",
    );
    clean("type Id = string\ntype Ids = array<Id>\ndef first(ids: Ids) -> Id?\n  ids.first\nend\n");
}

#[test]
fn removed_members_report_their_removal_even_with_wrong_arity() {
    for (source, code) in [
        ("1.nil?(2)", "V0402"),
        ("1.eql?", "V0403"),
        ("1.equal?(2, 3)", "V0403"),
        ("1.itself(2)", "V0404"),
        ("[1].at(0, 1)", "V0401"),
        ("[1].size(1)", "V0401"),
        ("'s'.clear(1)", "V0401"),
        ("now(1)", "V0401"),
    ] {
        let diagnostics = super::support::codes(source, &[code]);
        assert!(diagnostics[0].fixes.is_empty(), "{source}: {diagnostics:?}");
    }
    super::support::clean("class C; def nil?(n: int) -> int; n; end; end; C.new.nil?(2)");
    super::support::clean("[1].fetch(0)");
}
