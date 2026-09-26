//! Diagnostic spans cover whole expressions: member names the tree does not
//! locate, chained calls, and the parentheses of a group an operand starts
//! with, so fixes built from them rewrite exactly that text.

use super::support::{codes, fixed, spanned};

#[test]
fn a_member_read_spans_its_name() {
    let source = "def f(xs: array<int>) -> int\n  xs.last + 1\nend\n";
    let found = codes(source, &["V0107"]);
    assert_eq!(spanned(source, &found[0]), "xs.last");
    let source = "def f(xs: array<int>?) -> int\n  xs&.length + 1\nend\n";
    let found = codes(source, &["V0107"]);
    assert_eq!(spanned(source, &found[0]), "xs&.length");
}

#[test]
fn a_chain_of_calls_spans_every_link() {
    let source = "def f(h: hash<string, int>) -> string\n  h.keys.first\nend\n";
    let found = codes(source, &["V0107"]);
    assert_eq!(spanned(source, &found[0]), "h.keys.first");
    let source = "def f(xs: array<int>) -> string\n  xs.map { |x| x * 2 }.reverse.first\nend\n";
    let found = codes(source, &["V0101"]);
    assert_eq!(
        spanned(source, &found[0]),
        "xs.map { |x| x * 2 }.reverse.first"
    );
    let source = "def f(xs: array<string>) -> int\n  xs.first(2).join(\",\").upcase\nend\n";
    let found = codes(source, &["V0101"]);
    assert_eq!(spanned(source, &found[0]), "xs.first(2).join(\",\").upcase");
}

#[test]
fn a_parenthesized_operand_keeps_its_parentheses() {
    let source = "def f(a: int, b: int) -> string\n  (a + b) * 2\nend\n";
    let found = codes(source, &["V0101"]);
    assert_eq!(spanned(source, &found[0]), "(a + b) * 2");
    let source = "def f(a: int, b: int) -> string\n  2 * (a + b)\nend\n";
    let found = codes(source, &["V0101"]);
    assert_eq!(spanned(source, &found[0]), "2 * (a + b)");
    // A group that is the whole expression is left to its parentheses.
    let source = "def f(a: int, b: int) -> string\n  (a + b)\nend\n";
    let found = codes(source, &["V0101"]);
    assert_eq!(spanned(source, &found[0]), "a + b");
}

#[test]
fn the_fetch_fix_rewrites_the_whole_selector() {
    let source = "def pick(values: array<int>, i: int, n: int) -> int\n  values[(i + n) % values.length] + 1\nend\n";
    let found = codes(source, &["V0107"]);
    assert_eq!(
        spanned(source, &found[0]),
        "values[(i + n) % values.length]"
    );
    let repaired = fixed(source, &found[0]);
    assert_eq!(
        repaired,
        "def pick(values: array<int>, i: int, n: int) -> int\n  values.fetch((i + n) % values.length) + 1\nend\n"
    );
    codes(&repaired, &[]);
}

#[test]
fn undeclared_instance_parameter_spans_the_parameter() {
    let source = "class C\n  def initialize(@n: int)\n  end\nend";
    let diagnostic = super::support::codes(source, &["V0204"]);
    assert_eq!(super::support::spanned(source, &diagnostic[0]), "@n");
    super::support::clean("class C; @n: int; def initialize(@n: int); end; end");
}
