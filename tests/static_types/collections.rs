//! Arrays, shapes, dictionaries and tuples.

use super::support::{clean, codes, error, fixed, spanned};

#[test]
fn array_literals_take_the_union_of_their_elements() {
    clean("def f -> array<int>\n  [1, 2]\nend\n");
    clean("def f -> array<int | string>\n  [1, \"a\"]\nend\n");
    codes("def f -> array<int>\n  [1, \"a\"]\nend\n", &["V0101"]);
    clean("xs = [1, \"a\"]\nys: array<int | string> = xs\n");
}

#[test]
fn indexing_an_array_or_hash_may_give_nil_and_fetch_does_not() {
    clean("def f(xs: array<int>) -> int?\n  xs[0]\nend\n");
    clean("def f(xs: array<int>) -> int\n  xs.fetch(0)\nend\n");
    clean("def f(h: hash<string, int>) -> int\n  h.fetch(\"a\")\nend\n");
    let source = "def f(xs: array<int>) -> int\n  xs[0]\nend\n";
    let diagnostic = error(source, "V0107", "may be nil");
    assert_eq!(
        fixed(source, &diagnostic),
        "def f(xs: array<int>) -> int\n  xs.fetch(0)\nend\n"
    );
    let source = "def f(xs: array<int>) -> int\n  xs[1] + 1\nend\n";
    let diagnostic = error(source, "V0107", "may be nil");
    assert_eq!(
        fixed(source, &diagnostic),
        "def f(xs: array<int>) -> int\n  xs.fetch(1) + 1\nend\n"
    );
}

#[test]
fn compound_assignment_to_a_missing_element_reads_it_with_fetch() {
    for (source, expected) in [
        (
            "def f(xs: array<int>, i: int)\n  xs[i] += 1\nend\n",
            "def f(xs: array<int>, i: int)\n  xs[i] = xs.fetch(i) + 1\nend\n",
        ),
        (
            "def f(counts: hash<string, int>, key: string)\n  counts[key] += 1\nend\n",
            "def f(counts: hash<string, int>, key: string)\n  counts[key] = counts.fetch(key) + 1\nend\n",
        ),
        // The value keeps its grouping under the new operator.
        (
            "def f(totals: hash<string, float>)\n  totals[\"a\"] -= 2.0 - 1.5\nend\n",
            "def f(totals: hash<string, float>)\n  totals[\"a\"] = totals.fetch(\"a\") - (2.0 - 1.5)\nend\n",
        ),
    ] {
        let diagnostic = error(source, "V0107", "read it with `fetch`");
        assert!(spanned(source, &diagnostic).ends_with(']'), "{source}");
        let repaired = fixed(source, &diagnostic);
        assert_eq!(repaired, expected);
        clean(&repaired);
    }
    // A shape's declared field is present, so it needs no fix.
    clean("def f(point: { x: int })\n  point[\"x\"] += 1\nend\n");
    // Without a fix: an element type that includes nil, where `fetch` gives
    // nil too, and a receiver or index that evaluating twice could change.
    for source in [
        "def f(xs: array<int?>, i: int)\n  xs[i] += 1\nend\n",
        "def g -> array<int>\n  [1]\nend\ndef f\n  g[0] += 1\nend\n",
        "def g -> int\n  0\nend\ndef f(xs: array<int>)\n  xs[g] += 1\nend\n",
    ] {
        let diagnostic = error(source, "V0107", "may be nil");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
}

#[test]
fn hash_literals_are_exact_shapes() {
    clean("def f -> { name: string, age: int }\n  { name: \"Ada\", age: 3 }\nend\n");
    clean("user = { name: \"Ada\", age: 3 }\nname: string = user[\"name\"]\n");
    let source = "user = { name: \"Ada\" }\nuser[\"email\"]\n";
    let diagnostic = error(source, "V0110", "has no field \"email\"");
    assert_eq!(spanned(source, &diagnostic), "\"email\"");
    codes(
        "user = { name: \"Ada\" }\nuser[\"email\"] = \"x\"\n",
        &["V0110"],
    );
    codes(
        "def f -> { name: string }\n  { name: \"Ada\", age: 3 }\nend\n",
        &["V0110"],
    );
    codes(
        "def f -> { name: string, age: int }\n  { name: \"Ada\" }\nend\n",
        &["V0101"],
    );
}

#[test]
fn optional_and_open_shapes() {
    clean("def f -> { name: string, age?: int }\n  { name: \"Ada\" }\nend\n");
    clean("def f(u: { name: string, age?: int }) -> int?\n  u[\"age\"]\nend\n");
    clean(
        "def f(u: { name: string, ... }) -> string\n  u[\"name\"]\nend\ndef g -> string\n  f({ name: \"a\", extra: 1 })\nend\n",
    );
}

#[test]
fn a_shape_indexed_with_a_runtime_key_offers_a_dictionary() {
    let source = "counts = { a: 1, b: 2 }\ndef key -> string\n  \"a\"\nend\ncounts[key]\n";
    let diagnostic = error(source, "V0111", "a record, not a dictionary");
    assert_eq!(
        fixed(source, &diagnostic),
        "counts: hash<string, int> = { a: 1, b: 2 }\ndef key -> string\n  \"a\"\nend\ncounts[key]\n"
    );
    let source = "user = { name: \"Ada\", age: 3 }\ndef key -> string\n  \"a\"\nend\nuser[key]\n";
    assert!(
        error(source, "V0111", "record").fixes.is_empty(),
        "fields differ in type"
    );
}

#[test]
fn a_uniform_shape_is_a_dictionary() {
    clean(
        "def total(h: hash<string, int>) -> int\n  h.values.length\nend\ndef run -> int\n  total({ a: 1, b: 2 })\nend\n",
    );
    clean("counts: hash<string, int> = { a: 1 }\ncounts[\"b\"] = 2\n");
}

#[test]
fn tuples_index_by_literal() {
    clean("pair = 7.divmod(2)\nq: int = pair[0]\nr: int = pair[1]\n");
    clean("def f -> [int, string]\n  [1, \"a\"]\nend\n");
    error("pair = 7.divmod(2)\npair[2]\n", "V0113", "has 2 elements");
    clean(
        "def f(pairs: array<[string, int]>) -> int\n  total = 0\n  pairs.each { |pair| total += pair[1] }\n  total\nend\n",
    );
}

#[test]
fn builtins_use_tuples_for_pairs() {
    clean("def f(h: hash<string, int>) -> array<[string, int]>\n  h.to_a\nend\n");
    clean(
        "def f(xs: array<int>) -> array<int>\n  evens, odds = xs.partition { |x| x.even? }\n  evens + odds\nend\n",
    );
    clean(
        "def f(h: hash<string, int>) -> int\n  total = 0\n  h.each { |key, value| total += value + key.length }\n  total\nend\n",
    );
    clean(
        "def f(h: hash<string, int>) -> array<string>\n  out: array<string> = []\n  h.each { |pair| out << pair[0] }\n  out\nend\n",
    );
}

#[test]
fn fetching_a_declared_field_gives_its_type() {
    clean("def f(u: { name: string, age?: int }) -> string\n  u.fetch(\"name\")\nend\n");
    clean("def f(u: { name: string, age?: int }) -> int\n  u.fetch(\"age\")\nend\n");
}
