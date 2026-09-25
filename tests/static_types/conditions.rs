//! Conditions are `bool`, and `!`, `&&` and `||` take `bool` (ADR-008).

use super::support::{clean, codes, error, fixed, spanned};

#[test]
fn conditions_must_be_bool() {
    clean("def f(n: int) -> int\n  if n > 0\n    1\n  else\n    2\n  end\nend\n");
    let source = "def f(n: int) -> int\n  if n\n    1\n  else\n    2\n  end\nend\n";
    let diagnostic = error(source, "V0104", "a condition must be a bool, found int");
    assert_eq!(spanned(source, &diagnostic), "n");
    assert!(diagnostic.fixes.is_empty(), "no single repair for an int");
    codes(
        "def f(n: int)\n  while n\n    n -= 1\n  end\nend\n",
        &["V0104"],
    );
    codes("def f(n: int) -> int\n  n ? 1 : 2\nend\n", &["V0104"]);
}

#[test]
fn an_optional_condition_offers_a_nil_test() {
    let source = "def f(name: string?) -> int\n  if name\n    1\n  else\n    2\n  end\nend\n";
    let diagnostic = error(source, "V0104", "found string?");
    assert_eq!(
        fixed(source, &diagnostic),
        "def f(name: string?) -> int\n  if name != nil\n    1\n  else\n    2\n  end\nend\n"
    );
    // A nil test would change what an optional bool means.
    let source = "def f(flag: bool?) -> int\n  if flag\n    1\n  else\n    2\n  end\nend\n";
    assert!(error(source, "V0104", "bool?").fixes.is_empty());
}

#[test]
fn logical_operators_take_bool() {
    clean("def f(a: bool, b: bool) -> bool\n  !a && (b || a)\nend\n");
    error(
        "def f(a: int) -> bool\n  !a\nend\n",
        "V0105",
        "`!` takes a bool",
    );
    codes(
        "def f(a: int, b: bool) -> bool\n  a && b\nend\n",
        &["V0105"],
    );
    codes(
        "def f(a: string?, b: bool) -> bool\n  b || a\nend\n",
        &["V0105"],
    );
}

#[test]
fn or_assignment_tests_a_bool() {
    clean("def f(done: bool) -> bool\n  done ||= true\n  done\nend\n");
    codes(
        "def f(name: string?) -> string?\n  name ||= \"x\"\n  name\nend\n",
        &["V0104"],
    );
}
