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
fn shadowed_assignments_do_not_widen_enclosing_locals() {
    for body in [
        "[1].each { |x| x = 2 }",
        "[1].each { |x| [1].each { |y| x = y } }",
        "[[1, 2]].each { |(x, y)| x = y }",
        "begin; raise 'failure'; rescue => x; x = x; end",
    ] {
        let source = format!(
            "def f(c: bool) -> int\n  x: int? = 1\n  while c\n    {body}\n    x + 1\n    c = false\n  end\n  x + 1\nend\nf(true) + f(false)\n"
        );
        clean(&source);
        assert_eq!(
            vibescript::Engine::new()
                .compile(&source)
                .unwrap()
                .run(Default::default())
                .unwrap()
                .value
                .as_int(),
            Some(4)
        );
    }
    clean("x: int? = 1\nbegin\n  [1].each { |x| x = 2 }\nensure\n  p(x + 1)\nend\nx + 1\n");
    codes(
        "def f(c: bool)\n  x: int? = 1\n  [1, nil].each { |x: int?| x = 1; while c; x = nil; c = false; end; x + 1 }\nend\n",
        &["V0107"],
    );
}

#[test]
fn conditional_ensure_writes_preserve_the_body_state() {
    for (ensure, sum) in [
        ("x = 2 if c", 8),
        ("if c; x = 2 if d; end", 7),
        ("while c; x = 2; c = false; end", 8),
        ("x = 2 if c; x = 3 if d", 9),
        ("begin; x = 2 if c; ensure; nil; end", 8),
        ("if c; x = nil; return 0; end", 2),
        ("if c; return 0; x = nil; end", 2),
        ("[c].each { |go| x = 2 if go }", 8),
    ] {
        let source = format!(
            "def f(c: bool, d: bool) -> int\n  x: int? = nil\n  begin\n    x = 1\n  ensure\n    {ensure}\n  end\n  x + 1\nend\nf(false, false) + f(true, false) + f(true, true)\n"
        );
        clean(&source);
        assert_eq!(
            vibescript::Engine::new()
                .compile(&source)
                .unwrap()
                .run(Default::default())
                .unwrap()
                .value
                .as_int(),
            Some(sum),
            "{ensure}"
        );
    }
    for ensure in ["x = nil", "x = nil if c", "if c; x = nil if d; end"] {
        codes(
            &format!(
                "def f(c: bool, d: bool) -> int\n  x: int? = nil\n  begin\n    x = 1\n  ensure\n    {ensure}\n  end\n  x + 1\nend\n"
            ),
            &["V0107"],
        );
    }
    clean(
        "def f(c: bool) -> int\n  x: int? = nil\n  begin\n    x = 1\n  ensure\n    x = nil if c\n    return 0 if x == nil\n  end\n  x + 1\nend\n",
    );
    clean(
        "def f(c: bool) -> int\n  x: int | string | nil = nil\n  begin\n    x = 1\n  ensure\n    if c\n      begin\n        x = 's'\n      ensure\n        x = 2\n      end\n    end\n  end\n  x + 1\nend\n",
    );
    for name in ["it", "_1"] {
        codes(
            &format!(
                "def f -> int\n  {name}: int? = nil\n  begin\n    {name} = 1\n  ensure\n    [nil].each {{ {name} = nil }}\n  end\n  {name} + 1\nend\n"
            ),
            &["V0107"],
        );
    }
}

#[test]
fn an_ensure_sees_only_what_holds_wherever_it_starts() {
    // Its guards and exits narrow the rest of the function, as the body's do.
    clean(
        "def f(x: int?) -> int\n  begin\n    1\n  ensure\n    return 0 if x == nil\n  end\n  x + 1\nend\n",
    );
    clean(
        "def f(x: int?) -> int\n  begin\n    return 0 if x == nil\n  ensure\n    p(1)\n  end\n  x + 1\nend\n",
    );
    clean(
        "def f -> int\n  x: int? = nil\n  begin\n    x = 5\n  ensure\n    p(0)\n  end\n  x + 1\nend\n",
    );
    clean(
        "def f(c: bool) -> int\n  x: int? = 1\n  begin\n    x = nil if c\n  ensure\n    return 0 if x == nil\n  end\n  x + 1\nend\n",
    );
    // It may start before the body's guards, assignments and `else`.
    codes(
        "def f(c: bool, y: int?) -> int\n  begin\n    raise \"e\" if c\n    return 0 if y == nil\n  ensure\n    p(y + 1)\n  end\n  y\nend\n",
        &["V0107"],
    );
    codes(
        "def f(x: int?) -> int\n  return 0 if x == nil\n  begin\n    x = nil\n    raise \"e\"\n  ensure\n    p(x + 1)\n  end\n  0\nend\n",
        &["V0107"],
    );
    codes(
        "def f(x: int?) -> int\n  return 0 if x == nil\n  begin\n    1\n  rescue\n    2\n  else\n    x = nil\n  ensure\n    p(x + 1)\n  end\n  0\nend\n",
        &["V0107"],
    );
    codes(
        "def f -> int\n  begin\n    x = 1\n  ensure\n    p(x + 1)\n  end\n  0\nend\n",
        &["V0202"],
    );
    // A guard of the ensure that what the body leaves never passes leaves
    // nothing after the `begin` to run: no value has both types.
    clean(
        "def f -> int\n  x: string? = \"a\"\n  begin\n    x = nil\n  ensure\n    raise \"none\" if x == nil\n  end\n  x.upcase.length\nend\n",
    );
}

#[test]
fn assignments_anywhere_in_a_loop_or_begin_end_its_narrowing() {
    // A block assigns wherever it is written: in an index's receiver or
    // selector, a member read's receiver, a range or a raise.
    for expression in [
        "[1].map { |q| x = nil; q }[0]",
        "[1][[1].map { |q| x = nil; q }.fetch(0)]",
        "[1].map { |q| x = nil; q }.length",
        "(0..[1].map { |q| x = nil; q }.fetch(0))",
    ] {
        codes(
            &format!(
                "def f(x: int?) -> int\n  return 0 if x == nil\n  t = 0\n  while t < 2\n    t += x\n    s = {expression}\n  end\n  t\nend\n"
            ),
            &["V0107"],
        );
    }
    codes(
        "def f(x: int?) -> int\n  return 0 if x == nil\n  begin\n    raise [\"e\"].map { |q| x = nil; q }.fetch(0)\n  rescue\n    return x + 1\n  end\n  0\nend\n",
        &["V0107"],
    );
}

#[test]
fn a_retry_reruns_the_innermost_begin_that_is_rescuing() {
    let outer = |inner: &str| {
        format!(
            "def f(c: bool) -> int\n  v: int? = 5\n  return 0 if v == nil\n  begin\n    t = v + 1\n    raise \"e\" if c\n  rescue\n    v = nil\n    {inner}\n  end\n  0\nend\n"
        )
    };
    // The nested `begin` is not rescuing in its body, `else` or ensure, so
    // a `retry` there reruns the enclosing body, which sees `v = nil`.
    for inner in [
        "begin\n      1\n    ensure\n      retry if c\n    end",
        "begin\n      1\n    rescue\n      2\n    else\n      retry if c\n    end",
        "begin\n      retry if c\n    rescue ArgumentError\n      2\n    end",
        "if c\n      begin\n        retry\n      ensure\n        p(1)\n      end\n    end",
    ] {
        codes(&outer(inner), &["V0107"]);
    }
    // A `retry` in the nested `begin`'s own rescue reruns only it, and one
    // in a block would cross a call.
    for inner in [
        "begin\n      1\n    rescue\n      retry if c\n    end",
        "[1].each { |q| retry if c }",
    ] {
        clean(&outer(inner));
    }
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
