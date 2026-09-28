//! Unions, nil and narrowing: `== nil`, `!= nil`, `is_type?` and early
//! returns narrow locals and parameters, never member reads or indexes.

use super::support::{clean, codes, error};

#[test]
fn nil_tests_narrow_in_the_branches_they_guard() {
    clean("def len(s: string?) -> int\n  if s != nil\n    s.length\n  else\n    0\n  end\nend\n");
    clean("def len(s: string?) -> int\n  if s == nil\n    0\n  else\n    s.length\n  end\nend\n");
    clean("def len(s: string?) -> int\n  s == nil ? 0 : s.length\nend\n");
    clean("def len(s: string?) -> int\n  s != nil && s.length > 2 ? 1 : 0\nend\n");
    clean("def empty(s: string?) -> bool\n  s == nil || s.length == 0\nend\n");
}

#[test]
fn an_optional_value_needs_a_nil_test_before_use() {
    let source = "def len(s: string?) -> int\n  s.length\nend\n";
    error(source, "V0107", "`length` is not defined for nil");
    codes("def add(n: int?) -> int\n  n + 1\nend\n", &["V0107"]);
}

#[test]
fn early_returns_narrow_the_rest_of_the_function() {
    clean("def len(s: string?) -> int\n  return 0 if s == nil\n  s.length\nend\n");
    clean(
        "def len(s: string?) -> int\n  if s == nil\n    raise \"missing\"\n  end\n  s.length\nend\n",
    );
    clean(
        "def total(xs: array<int?>) -> int\n  sum = 0\n  xs.each { |x|\n    next if x == nil\n    sum += x\n  }\n  sum\nend\n",
    );
}

#[test]
fn assignment_narrows_an_optional_local() {
    clean("label: string? = nil\nlabel = \"ready\"\nlabel.upcase\n");
    codes(
        "label: string? = \"a\"\nlabel = nil\nlabel.upcase\n",
        &["V0203"],
    );
}

#[test]
fn is_type_narrows_unions_and_any() {
    clean(
        "def show(v: int | string) -> string\n  if v.is_type?(:int)\n    (v + 1).to_s\n  else\n    v.upcase\n  end\nend\n",
    );
    clean(
        "def show(v: any) -> int\n  if v.is_type?(:string)\n    v.length\n  else\n    0\n  end\nend\n",
    );
}

#[test]
fn member_reads_and_indexes_do_not_narrow() {
    codes(
        "def first(h: { name: string? }) -> int\n  if h[\"name\"] != nil\n    h[\"name\"].length\n  else\n    0\n  end\nend\n",
        &["V0107"],
    );
    clean(
        "def first(h: { name: string? }) -> int\n  name = h[\"name\"]\n  if name != nil\n    name.length\n  else\n    0\n  end\nend\n",
    );
}

#[test]
fn a_loop_widens_what_its_body_assigns() {
    codes(
        "def f(xs: array<int>) -> int\n  last: int? = 0\n  xs.each { |x|\n    last.abs\n    last = nil\n  }\n  0\nend\n",
        &["V0107"],
    );
}

#[test]
fn an_optional_block_must_be_guarded() {
    clean("def maybe(msg: string, &block?: string)\n  yield msg if block_given?\nend\n");
    error(
        "def maybe(msg: string, &block?: string)\n  yield msg\nend\n",
        "V0307",
        "guarded by `block_given?`",
    );
}

#[test]
fn tests_the_type_already_decides_are_warnings() {
    let source = "def f(n: int) -> int\n  if n == nil\n    0\n  else\n    n\n  end\nend\n";
    let found = vibescript::Engine::new()
        .type_check(source)
        .unwrap()
        .diagnostics;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code.to_string(), "V0121");
    assert!(!found[0].is_error());
    let source =
        "def f(n: int) -> int\n  if n.is_type?(:string)\n    1\n  else\n    n\n  end\nend\n";
    let found = vibescript::Engine::new()
        .type_check(source)
        .unwrap()
        .diagnostics;
    assert_eq!(found[0].code.to_string(), "V0120");
    assert!(!found[0].is_error());
}
