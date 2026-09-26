//! Regressions for the remaining static checker soundness gaps.

use super::support::{clean, codes};

#[test]
fn optional_defaults_do_not_definitely_assign_body_locals() {
    clean("def f(a: int = (while true; x=7; break x; end)) -> int\n  a\nend\nf(1)\n");
    codes(
        "def f(a: int = (while true; x=7; break x; end)) -> int\n  x\nend\nf(1)\n",
        &["V0202"],
    );
}

#[test]
fn namespace_blocks_do_not_capture_assigned_ambient_locals() {
    clean("x=1;module M;y=x;[2].each { y+=1 };C=y;end;[x,M.C]\n");
    clean("x=1;module M;[2].each { x=2 };C=x;end;[x,M.C]\n");
    codes(
        "x=1;module M;[2].each { x+=1 };C=x;end;[x,M.C]\n",
        &["V0202"],
    );
    let mut engine = vibescript::Engine::new();
    engine.set_static_types(true);
    let result = engine
        .compile("x=1;module M;[2].each { x=2 };C=x;end;[x,M.C]\n")
        .unwrap()
        .run(vibescript::CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[1, 1]");
}

#[test]
fn removed_names_keep_their_diagnostic_and_fix_with_invalid_arguments() {
    clean("1.seconds.to_s\n[1].length\n");
    for source in [
        "1.seconds.string { 1 }\n",
        "1.seconds.string(k: 1)\n",
        "[1].count(*[])\n",
    ] {
        let found = codes(source, &["V0401"]);
        assert!(found[0].applicable_fix().is_some(), "{source}");
    }
    let source = "x: string = [1].size\n";
    let found = codes(source, &["V0101", "V0401"]);
    assert!(
        found
            .iter()
            .find(|d| d.code.to_string() == "V0401")
            .unwrap()
            .applicable_fix()
            .is_some()
    );
    codes("\"ab\".replace\n", &["V0401"]);
}

#[test]
fn defaults_match_parameter_types_in_declaration_order() {
    clean("def f(a: int = 1, b: int = a + 1, *, c: string = \"x\")\nend\n");
    codes("def f(a: int = \"x\")\nend\n", &["V0101"]);
    codes("def f(*, a: int = \"x\")\nend\n", &["V0101"]);
    clean("def f(a: array<int> = [])\nend\n");
}

#[test]
fn loop_and_rescue_bindings_keep_existing_local_types() {
    clean("x = 0\nfor x in [1, 2]\n  x + 1\nend\n");
    codes("x = \"s\"\nfor x in [1, 2]\nend\n", &["V0102"]);
    clean("begin\n  raise \"x\"\nrescue => e\n  e.message\nend\n");
    codes(
        "e = 1\nbegin\n  raise \"x\"\nrescue => e\n  0\nend\n",
        &["V0102"],
    );
}

#[test]
fn control_forms_require_their_runtime_context() {
    clean("while true\n  break\nend\n[1].each { next }\n");
    codes("break\n", &["V0001"]);
    codes("next\n", &["V0001"]);
    clean("class A\n  @@x: int = 1\nend\n");
    codes("@@x = 3\n", &["V0204"]);
    clean("block_given?\n");
    codes("block_given?(1)\n", &["V0301"]);
}

#[test]
fn array_union_literals_fit_a_whole_alternative() {
    clean("a: array<int> | array<string> = [\"s\"]\n");
    clean("a: array<int> | array<string> = [1]\n");
    codes("a: array<int> | array<string> = [1, \"s\"]\n", &["V0101"]);
    clean("a: array<array<int>> | array<array<string>> = [[]]\n");
}

#[test]
fn every_instance_union_member_must_accept_the_call() {
    let classes = "class A\n  def f(x: int) -> int\n    x\n  end\nend\nclass B\n  def f(x: string) -> int\n    x.length\n  end\nend\n";
    clean(&format!(
        "{classes}def g(v: A | B) -> string\n  v.to_s\nend\n"
    ));
    codes(
        &format!("{classes}def g(v: A | B) -> int\n  v.f(1)\nend\n"),
        &["V0101"],
    );
}

#[test]
fn generic_array_arguments_require_arrays() {
    clean("[1].product([\"a\"])\n");
    codes("[1].product(1)\n", &["V0101"]);
    codes("[1].zip(1)\n", &["V0101"]);
}

#[test]
fn comparator_sort_does_not_require_comparable_elements() {
    clean("[{ n: 2 }, { n: 1 }].sort { |a, b| a[\"n\"] <=> b[\"n\"] }\n");
    codes("[{ n: 2 }, { n: 1 }].sort\n", &["V0115"]);
    codes("[1].sort { |a, b| a == b }\n", &["V0101"]);
}

#[test]
fn index_assignment_yields_the_assigned_value() {
    let class = "class A\n  def []=(i: int, v: int)\n  end\nend\n";
    clean(&format!(
        "{class}def f -> int\n  a = A.new\n  a[0] = 7\nend\n"
    ));
    codes(
        &format!("{class}def f -> string\n  a = A.new\n  a[0] = 7\nend\n"),
        &["V0101"],
    );
}

#[test]
fn match_data_indexes_distinguish_data_from_methods() {
    clean("def f(m: match_data) -> array<string?>\n  m[\"captures\"]\nend\n");
    clean("def f(m: match_data) -> int?\n  m.begin(0)\nend\n");
    codes("def f(m: match_data)\n  m[\"begin\"]\nend\n", &["V0310"]);
}

#[test]
fn class_values_are_not_instances() {
    clean("class A\nend\na = A\nb: A = a.new\n");
    codes("class A\nend\na: A = A\n", &["V0101"]);
}

#[test]
fn any_narrows_from_json_without_using_the_function_body() {
    clean("v = JSON.parse(\"1\")\nif v.is_type?(:int)\n  x: int = v + 1\nend\n");
    codes(
        "v = JSON.parse(\"1\")\nif v.is_type?(:int)\n  x: string = v\nend\n",
        &["V0101"],
    );
}

#[test]
fn namespace_bodies_read_earlier_top_level_locals() {
    clean("x = 1\nmodule M\n  VALUE = x + 1\nend\ny: int = M::VALUE\n");
    codes("x = \"s\"\nmodule M\n  VALUE = x + 1\nend\n", &["V0108"]);
    codes("module M\n  VALUE = x\nend\nx = 1\n", &["V0201"]);
}

#[test]
fn nested_namespace_constants_are_initialized_before_the_parent() {
    clean(
        "module Outer\n  module Inner\n    VALUE = 2\n  end\n  TOTAL = Inner::VALUE + 1\nend\nx: int = Outer::TOTAL\n",
    );
    codes(
        "module Outer\n  module Inner\n    VALUE = \"s\"\n  end\n  TOTAL = Inner::VALUE + 1\nend\n",
        &["V0108"],
    );
}

#[test]
fn for_targets_are_assigned_even_when_the_loop_is_empty() {
    clean("for x in [1]\nend\ny: int = x\n");
    clean("def f(xs: array<int>) -> int?\n  for x in xs\n    x + 1\n  end\n  x\nend\n");
    codes(
        "def f(xs: array<int>) -> int\n  for x in xs\n  end\n  x\nend\n",
        &["V0107"],
    );
}

#[test]
fn implicit_block_parameters_are_values_not_functions() {
    clean("[1].map { it + 1 }\n");
    codes("[1].map { it(2) }\n", &["V0310"]);
}

#[test]
fn namespace_rebinding_is_rejected_and_local_shadowing_is_tracked() {
    clean("math = 7\nmath + 1\n");
    codes("Math = 7\n", &["V0102"]);
    codes("for Math in [7]\nend\n", &["V0102"]);
    clean("p = 7\np + 1\n");
    codes("puts = 7\nputs(1)\n", &["V0310"]);
    codes("class A\nend\nA = 7\n", &["V0102"]);
    codes(
        "def g(x: int = 1) -> int\n  x\nend\ng = 7\ng(1)\n",
        &["V0310"],
    );
}

#[test]
fn union_receiver_blocks_are_checked_with_every_parameter_type() {
    let classes = "class A\n  def f(&block: int -> int) -> int\n    yield 1\n  end\nend\nclass B\n  def f(&block: string -> int) -> int\n    yield \"s\"\n  end\nend\n";
    clean(&format!(
        "{classes}def g(v: A | B) -> int\n  v.f {{ |x| x.to_s.length }}\nend\n"
    ));
    codes(
        &format!("{classes}def g(v: A | B) -> int\n  v.f {{ |x| x + 1 }}\nend\n"),
        &["V0108"],
    );
}

#[test]
fn capitalized_assignments_in_functions_are_rejected() {
    clean("def bump -> int\n  count = 1\n  count = 2\n  count\nend\n");
    codes("def bad\n  COUNT = 1\nend\n", &["V0102"]);
}

#[test]
fn splats_select_overloads_and_check_every_element() {
    clean("x: array<int> = [1, 2].first(*[1])\n");
    clean("x: int? = [1, 2].first(*[])\n");
    codes("[1, 2].first(*[\"s\"])\n", &["V0101"]);
    codes("[1, 2].first(*[1, 2])\n", &["V0301"]);
    codes("def f(x: int)\nend\nf(*[])\n", &["V0301"]);
    clean("def f(*xs: array<int>)\nend\na = [1, 2]\nf(*a)\n");
    codes(
        "def f(*xs: array<int>)\nend\na = [\"s\"]\nf(*a)\n",
        &["V0101"],
    );
    codes("def f(x: int)\nend\na = [1]\nf(*a)\n", &["V0301"]);
    clean("def f(*, x: int)\nend\nf(**{x: 1})\n");
    codes("def f(*, x: int)\nend\nf(**{x: \"s\"})\n", &["V0101"]);
    codes("def f(*, x: int)\nend\nf(**{})\n", &["V0303"]);
}
