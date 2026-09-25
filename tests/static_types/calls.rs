//! Calls to script functions and builtins: arity, keywords, overloads,
//! generics, blocks and `yield`.

use super::support::{clean, codes, error, fixed, spanned};

#[test]
fn script_function_arguments_are_checked() {
    clean("def add(a: int, b: int = 1) -> int\n  a + b\nend\nadd(1)\nadd(1, 2)\n");
    let source = "def add(a: int, b: int = 1) -> int\n  a + b\nend\nadd(\"x\")\n";
    let diagnostic = error(
        source,
        "V0101",
        "argument 1 (`a`) of `add` is int, found string",
    );
    assert_eq!(spanned(source, &diagnostic), "\"x\"");
    codes("def add(a: int) -> int\n  a\nend\nadd(1, 2)\n", &["V0301"]);
    codes("def add(a: int) -> int\n  a\nend\nadd\n", &["V0301"]);
}

#[test]
fn keyword_arguments_are_checked_by_name() {
    clean("def f(a: int, *, loud: bool = false) -> int\n  a\nend\nf(1, loud: true)\nf(1)\n");
    clean(
        "def f(*items: array<int>, sep: string = \",\") -> string\n  sep\nend\nf(1, 2, sep: \"-\")\n",
    );
    codes(
        "def f(a: int, *, loud: bool) -> int\n  a\nend\nf(1)\n",
        &["V0303"],
    );
    codes(
        "def f(a: int) -> int\n  a\nend\nf(1, loud: true)\n",
        &["V0302"],
    );
    codes(
        "def f(a: int, *, loud: bool = false) -> int\n  a\nend\nf(1, loud: 3)\n",
        &["V0101"],
    );
    // A keyword parameter is not passed by position.
    codes(
        "def f(a: int, *, loud: bool = false) -> int\n  a\nend\nf(1, true)\n",
        &["V0301"],
    );
    // A removed keyword form is reported with its rewrite, and its literal
    // default still types the calls.
    codes(
        "def f(a: int, loud: false) -> int\n  a\nend\nf(1, loud: 3)\n",
        &["V0414", "V0101"],
    );
    // A keyword after `*` declares its type like any other parameter; a
    // literal default gives the fix.
    let source = "def f(a: int, *, loud = false) -> int\n  a\nend\n";
    let diagnostic = error(source, "V0118", "`loud`");
    assert_eq!(
        fixed(source, &diagnostic),
        "def f(a: int, *, loud: bool = false) -> int\n  a\nend\n"
    );
    codes("def f(a: int, *, loud)\nend\n", &["V0118"]);
    clean("def f(**opts: hash<string, int>) -> int\n  opts.length\nend\nf(a: 1, b: 2)\n");
}

#[test]
fn rest_parameters_collect_their_element_type() {
    clean("def total(*values: array<int>) -> int\n  values.length\nend\ntotal(1, 2, 3)\n");
    codes(
        "def total(*values: array<int>) -> int\n  values.length\nend\ntotal(1, \"2\")\n",
        &["V0101"],
    );
}

#[test]
fn builtin_overloads_are_selected_by_the_call_shape() {
    clean("def f(xs: array<int>) -> int?\n  xs.first\nend\n");
    clean("def f(xs: array<int>) -> array<int>\n  xs.first(2)\nend\n");
    codes(
        "def f(xs: array<int>) -> int?\n  xs.first(1, 2)\nend\n",
        &["V0301"],
    );
    clean("def f(s: string) -> string\n  s.gsub(\"a\", \"b\")\nend\n");
    clean("def f(s: string) -> string\n  s.gsub(\"a\") { |m| m.upcase }\nend\n");
}

#[test]
fn generic_builtins_bind_from_the_receiver_arguments_and_block() {
    clean("def f(xs: array<int>) -> array<string>\n  xs.map { |x| x.to_s }\nend\n");
    // The table gives `reduce` one signature, `-> A?`, with or without `initial`.
    clean("def f(xs: array<int>) -> int?\n  xs.reduce(0) { |sum, x| sum + x }\nend\n");
    clean("def f(xs: array<int>) -> int?\n  xs.reduce { |sum, x| sum + x }\nend\n");
    clean(
        "def f(xs: array<string>) -> hash<string, array<string>>\n  xs.group_by { |x| x }\nend\n",
    );
    clean("def f(xs: array<int?>) -> array<int>\n  xs.compact\nend\n");
    clean("def f(xs: array<int>) -> array<int>\n  xs.filter_map { |x| x > 1 ? x : nil }\nend\n");
    codes(
        "def f(xs: array<int>) -> array<int>\n  xs.map { |x| x.to_s }\nend\n",
        &["V0101"],
    );
}

#[test]
fn bounds_reject_union_element_types() {
    clean("def f(xs: array<int>) -> array<int>\n  xs.sort\nend\n");
    clean("def f(xs: array<number>) -> array<number>\n  xs.sort\nend\n");
    error(
        "def f(xs: array<int | string>) -> array<int | string>\n  xs.sort\nend\n",
        "V0115",
        "union",
    );
}

#[test]
fn block_parameters_take_their_types_from_the_signature() {
    codes(
        "def f(xs: array<int>) -> array<int>\n  xs.map { |x| x.upcase }\nend\n",
        &["V0203"],
    );
    clean("def f(xs: array<int>) -> array<int>\n  xs.map { |x: int| x + 1 }\nend\n");
    codes(
        "def f(xs: array<int>) -> array<int>\n  xs.map { |x: string| 1 }\nend\n",
        &["V0101"],
    );
    clean("def f(xs: array<int>) -> array<int>\n  xs.map { it + 1 }\nend\n");
}

#[test]
fn block_results_are_checked() {
    error(
        "def f(xs: array<int>) -> array<int>\n  xs.select { |x| x }\nend\n",
        "V0101",
        "the block returns bool",
    );
    clean(
        "def f(xs: array<int>) -> array<int>\n  xs.select { |x| next false if x > 2\n    true }\nend\n",
    );
}

#[test]
fn a_required_block_must_be_passed_and_an_absent_one_refused() {
    codes(
        "def f(xs: array<int>) -> array<int>\n  xs.map\nend\n",
        &["V0304"],
    );
    codes(
        "def f(xs: array<int>) -> int\n  xs.length { |x| x }\nend\n",
        &["V0305"],
    );
    codes(
        "def each_twice(&block: int)\n  yield 1\n  yield 2\nend\neach_twice\n",
        &["V0304"],
    );
    codes("def plain -> int\n  1\nend\nplain { 2 }\n", &["V0305"]);
}

#[test]
fn yield_is_checked_against_the_declared_block() {
    clean(
        "def keep(items: array<int>, &block: int -> bool) -> array<int>\n  kept: array<int> = []\n  items.each { |item|\n    kept << item if yield(item)\n  }\n  kept\nend\nkeep([1, 2]) { |x| x > 1 }\n",
    );
    codes("def f(&block: int)\n  yield \"x\"\nend\n", &["V0101"]);
    error(
        "def f(&block: int)\n  x = yield 1\nend\n",
        "V0119",
        "no result type",
    );
    error("def f\n  yield 1\nend\n", "V0308", "declares no block");
    codes(
        "def keep(&block: int -> bool) -> bool\n  yield 1\nend\nkeep { |x| x + 1 }\n",
        &["V0101"],
    );
    clean(
        "def each_pair(h: hash<string, int>, &block: (string, int))\n  h.keys.each { |k| yield k, h.fetch(k) }\nend\n",
    );
}

#[test]
fn unknown_functions_and_members_are_reported() {
    let source = "def f -> int\n  missing(1)\nend\n";
    let diagnostic = error(source, "V0201", "`missing`");
    assert_eq!(spanned(source, &diagnostic), "missing");
    let source = "def f(x: int) -> int\n  x.upcase\nend\n";
    let diagnostic = error(source, "V0203", "int has no member `upcase`");
    assert_eq!(spanned(source, &diagnostic), "upcase");
}

#[test]
fn require_takes_literal_names() {
    clean("mod = require(\"helpers\")\n");
    clean("mod = require(\"helpers\", as: \"h\")\n");
    let source = "name = \"helpers\"\nmod = require(name)\n";
    let diagnostic = error(source, "V0309", "string literal");
    assert_eq!(spanned(source, &diagnostic), "name");
}

#[test]
fn blocks_declare_at_most_the_parameters_they_are_given() {
    clean("def f(xs: array<int>) -> array<int>\n  xs.map { |x| x }\nend\n");
    error(
        "def f(n: int) -> int\n  n.times { |i, j| i }\nend\n",
        "V0306",
        "declares 2 parameter(s), but it is given 1",
    );
    clean(
        "def f(pairs: array<[string, int]>) -> array<string>\n  pairs.map { |key, value| key }\nend\n",
    );
}

#[test]
fn only_functions_are_called() {
    error(
        "def f(x: int) -> int\n  x(1)\nend\n",
        "V0310",
        "is a local, not a function",
    );
    codes("def f(x: int) -> int\n  (x)(1)\nend\n", &["V0310"]);
}

#[test]
fn only_collections_strings_and_classes_with_brackets_are_indexed() {
    error(
        "def f(x: int) -> any\n  x[0]\nend\n",
        "V0112",
        "int cannot be indexed",
    );
    clean("class Grid\n  def [](i: int) -> int\n    i\n  end\nend\ng: int = Grid.new[3]\n");
}
