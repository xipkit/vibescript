//! Collection, data and member error messages match the reference
//! implementation's wording exactly, since scripts read `e.message` and hosts
//! log it. Each expected message was checked against the Go reference.
//! What static types now refuse at compile time is checked by its first
//! diagnostic instead.

mod common;

use vibescript::CallOptions;

fn message(body: &str) -> String {
    let source = format!("def run -> any\n{body}\nend");
    let script = common::runtime_engine()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{body}: {error}"));
    match script.call("run", &[], CallOptions::default()) {
        Ok(outcome) => panic!("{body}: expected an error, got {}", outcome.value),
        Err(error) => error.message,
    }
}

/// The message of the error `body` raises without static types, which
/// refuse a removed spelling at compile time.
fn runtime_message(body: &str) -> String {
    let source = format!("def run -> any\n{body}\nend");
    let script = common::gradual_engine()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{body}: {error}"));
    match script.call("run", &[], CallOptions::default()) {
        Ok(outcome) => panic!("{body}: expected an error, got {}", outcome.value),
        Err(error) => error.message,
    }
}

/// The code of the first diagnostic that refuses `source` at compile time,
/// and the text it points at.
fn refusal(source: &str) -> (String, String) {
    let error = vibescript::Engine::new()
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    let first = &error.diagnostics()[0];
    (
        first.code.to_string(),
        source[first.span.start..first.span.end].to_owned(),
    )
}

const KEY_RULE: &str = "hash keys must be strings or symbols; convert the key with to_s";

#[test]
fn unsupported_hash_keys_name_the_kind_and_the_member_input() {
    let plain = |kind: &str| format!("unsupported hash key type {kind}: {KEY_RULE}");
    let at = |site: &str, kind: &str| format!("{site} unsupported hash key: {}", plain(kind));
    let cases = [(
        "[1].to_h { |x| [x, x] }",
        at("array.to_h pair key is an", "int"),
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("h = {a: 1}\nh[[1]]", "V0101", "[1]"),
        ("h = {a: 1}\nh[1] = 2", "V0111", "1"),
        ("h = {a: 1}\nh[/a/]", "V0101", "/a/"),
        ("{a: {b: 1}}.dig(:a, 1)", "V0101", ":a"),
        ("{a: 1}.store(1, 2)", "V0401", "store"),
        ("{a: 1}.fetch(nil)", "V0101", "nil"),
        ("{a: 1}.fetch(1) { |k| 2 }", "V0101", "1"),
        ("{a: 1}.fetch_values(1.5)", "V0101", "1.5"),
        ("{a: 1}.values_at([1])", "V0101", "[1]"),
        ("{a: 1}.key?(1)", "V0101", "1"),
        ("{a: 1}.key?(1)", "V0101", "1"),
        ("{a: 1}.key?(1)", "V0101", "1"),
        ("{a: 1}.delete(true)", "V0101", "true"),
        ("{a: 1}.slice(1)", "V0101", "1"),
        ("{a: 1}.except({})", "V0101", "{}"),
        ("{a: 1}.transform_keys { |k| 1 }", "V0101", "1"),
        (
            "{a: {b: 1}}.deep_transform_keys { |k| nil }",
            "V0101",
            "nil",
        ),
        ("{a: 1}.remap_keys({a: 1})", "V0101", "1"),
        ("[[1, 2]].to_h", "V0304", "to_h"),
        ("[1].group_by { |x| [x] }", "V0115", "group_by"),
        ("[1].group_by_stable { |x| x }", "V0115", "group_by_stable"),
        ("[1, 2].tally", "V0115", "tally"),
        ("[1].tally { |x| 1.5 }", "V0115", "tally"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn unknown_members_name_the_receiver_kind_and_suggest_close_names() {
    let cases = [(
        "{a: 1}.to_s",
        "unknown hash method to_s (did you mean \"to_a\"?)",
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("[1].frobnicate", "V0203", "frobnicate"),
        ("[1].lengt", "V0203", "lengt"),
        ("[1].uniq!", "V0203", "uniq!"),
        ("[1].to_a", "V0203", "to_a"),
        ("\"x\".uppcase", "V0203", "uppcase"),
        ("\"x\".push(1)", "V0203", "push"),
        ("({counter: 1})[\"countr\"]", "V0110", "\"countr\""),
        ("nil.empty?", "V0203", "empty?"),
        ("nil.inspct", "V0203", "inspct"),
        ("true.foo", "V0203", "foo"),
        ("5.tims", "V0203", "tims"),
        ("5.chr", "V0203", "chr"),
        ("1.5.foo", "V0203", "foo"),
        (":a.id2nam", "V0203", "id2nam"),
        ("(1..2).reverse", "V0203", "reverse"),
        ("/a/.matches?", "V0203", "matches?"),
        ("money(\"1.00 USD\").nope", "V0203", "nope"),
        ("Time.now.yer", "V0203", "yer"),
        ("1.seconds.in_minuts", "V0203", "in_minuts"),
        ("JSON.foo", "V0203", "foo"),
        ("5.each { |x| x }", "V0203", "each"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

fn function_message(source: &str, function: &str) -> String {
    let script = common::runtime_engine()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    match script.call(function, &[], CallOptions::default()) {
        Ok(outcome) => panic!("{source}: expected an error, got {}", outcome.value),
        Err(error) => error.message,
    }
}

#[test]
fn missing_names_read_as_undefined_variables_or_unknown_members() {
    for (source, code, text) in [
        (
            "def run -> any\n  length = 5\n  lengtt\nend",
            "V0201",
            "lengtt",
        ),
        ("def run -> any\n  asert true\nend", "V0201", "asert"),
        (
            "def helper\n  1\nend\ndef run -> any\n  helpr()\nend",
            "V0201",
            "helpr",
        ),
        ("def run -> any\n  zzzzzz\nend", "V0201", "zzzzzz"),
        (
            "class Greeter\n  def greet\n    1\n  end\nend\ndef run -> any\n  Greeter.new.gret\nend",
            "V0203",
            "gret",
        ),
        (
            "class Vault\n  private def secret\n    1\n  end\n  def probe\n    secrez\n  end\nend\ndef run -> any\n  Vault.new.probe\nend",
            "V0201",
            "secrez",
        ),
        (
            "class Vault\n  private def secret\n    1\n  end\nend\ndef run -> any\n  Vault.new.secrez\nend",
            "V0203",
            "secrez",
        ),
        (
            "class Counter\n  def self.instances\n    1\n  end\nend\ndef run -> any\n  Counter.instnces\nend",
            "V0203",
            "instnces",
        ),
        (
            "class A\n  attr_reader :x\nend\ndef run -> any\n  1\nend",
            "V0201",
            "attr_reader",
        ),
        (
            "module Config\n  LIMIT = 1\nend\ndef run -> any\n  Config::LIMT\nend",
            "V0203",
            "LIMT",
        ),
        (
            "enum Status\n  Draft\nend\ndef run -> any\n  Status::Drafd\nend",
            "V0206",
            "Drafd",
        ),
        (
            "enum Status\n  Draft\nend\ndef run -> any\n  Status::Draft.strng\nend",
            "V0203",
            "strng",
        ),
    ] {
        let (found, at) = refusal(source);
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{source}");
    }
}

#[test]
fn index_operator_errors_name_the_selector_or_receiver() {
    let cases = [
        ("x = [1]\nx[2 ** 70]", "index must fit in a 64-bit integer"),
        ("x = [1]\nx[5] = 1", "array index out of bounds"),
        ("x = [1]\nx[-5] = 1", "array index out of bounds"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("x = [1]\nx[\"a\"]", "V0101", "\"a\""),
        ("\"abc\"[nil]", "V0107", "nil"),
        ("x = [1]\nx[\"a\", 3]", "V0101", "\"a\""),
        ("5[1]", "V0112", "5[1]"),
        ("nil[0]", "V0107", "nil"),
        ("x = [1, 2, 3]\nx[1, 2, 3]", "V0112", "x[1, 2, 3]"),
        ("\"abc\"[1, 2, 3]", "V0112", "\"abc\"[1, 2, 3]"),
        ("{a: 1}[1, 2]", "V0112", "{a: 1}[1, 2]"),
        ("x = [1]\nx[0, 1] = 1", "V0112", "x[0, 1]"),
        ("x = {a: 1}\nx[:a, 1] = 1", "V0112", "x[:a, 1]"),
        ("x = \"abc\"\nx[0] = \"z\"", "V0112", "x[0]"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
    // An instance whose class defines no index members cannot be indexed.
    let plain = "class Plain\nend\n";
    for (body, text) in [
        ("Plain.new[1]", "Plain.new[1]"),
        ("p = Plain.new\n  p[1] = 2", "p[1]"),
    ] {
        let (found, at) = refusal(&format!("{plain}def run -> any\n  {body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), ("V0203", text), "{body}");
    }
}

#[test]
fn protected_records_name_the_rejected_operation() {
    for (body, code, text) in [
        ("m = \"ab\".match(/(a)(b)/)\nm[0] = \"z\"", "V0107", "m"),
        (
            "m = \"ab\".match(/(a)(b)/)\nm.pre_match = \"z\"",
            "V0107",
            "pre_match",
        ),
        (
            "m = \"ab\".match(/(a)(b)/)\nm.replace({})",
            "V0107",
            "replace",
        ),
        (
            "m = \"ab\".match(/(a)(b)/)\nm.delete_if { |k, v| true }",
            "V0107",
            "delete_if",
        ),
        ("m = \"ab\".match(/(a)(b)/)\nm.clear", "V0107", "clear"),
        (
            "e = begin\n  raise \"x\"\nrescue RuntimeError => e\n  e\nend\ne[\"message\"] = \"y\"",
            "V0112",
            "e[\"message\"]",
        ),
        (
            "e = begin\n  raise \"x\"\nrescue RuntimeError => e\n  e\nend\ne.message = \"y\"",
            "V0203",
            "message",
        ),
        (
            "e = begin\n  raise \"x\"\nrescue RuntimeError => e\n  e\nend\ne.store(:a, 1)",
            "V0203",
            "store",
        ),
        (
            "e = begin\n  raise \"x\"\nrescue RuntimeError => e\n  e\nend\ne.delete(:message)",
            "V0203",
            "delete",
        ),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn member_misuse_names_the_member_or_module() {
    for (source, code, text) in [
        (
            "class ReadOnly\n  getter name: string\n  def initialize(name: string)\n    @name = name\n  end\nend\ndef run -> any\n  r = ReadOnly.new(\"a\")\n  r.name = \"b\"\nend",
            "V0203",
            "name",
        ),
        (
            "module Billing\nend\ndef run -> any\n  Billing.new\nend",
            "V0203",
            "new",
        ),
        (
            "def run -> any\n  x = JSON.stringify\n  x\nend",
            "V0301",
            "stringify",
        ),
        (
            "def run -> any\n  x = Regex.escape\n  x\nend",
            "V0301",
            "escape",
        ),
    ] {
        let (found, at) = refusal(source);
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{source}");
    }
}

#[test]
fn block_driven_array_members_check_calls_in_reference_order() {
    let cases = [(
        "[1].index(1, -1)",
        "array.index offset must be non-negative integer",
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("[1].each", "V0304", "each"),
        ("[1].map(1)", "V0301", "map"),
        (
            "[1].each_with_index(1, a: 1) { |x| x }",
            "V0301",
            "each_with_index",
        ),
        ("[1].map_with_index(a: 1)", "V0304", "map_with_index"),
        ("[1].flat_map(1)", "V0301", "flat_map"),
        ("[1].reject(a: 1)", "V0304", "reject"),
        ("[1].each_slice", "V0301", "each_slice"),
        ("[1].each_slice(0)", "V0304", "each_slice"),
        ("[1].each_slice(\"a\")", "V0304", "each_slice"),
        ("[1].each_slice(2)", "V0304", "each_slice"),
        ("[1].each_cons(1.5) { |x| x }", "V0101", "1.5"),
        ("[1].cycle(1, 2)", "V0301", "cycle"),
        ("[1].cycle(1.5)", "V0304", "cycle"),
        ("[1].cycle(2**70)", "V0304", "cycle"),
        ("[1].find(nil, nil)", "V0301", "find"),
        ("[1].index(1) { |x| x }", "V0301", "index"),
        ("[1].rindex", "V0301", "rindex"),
        ("[1].reduce", "V0301", "reduce"),
        ("[1].reduce(1, 2, 3)", "V0301", "reduce"),
        ("[1].reduce(1)", "V0401", "reduce"),
        ("[1].count(1, 2)", "V0301", "count"),
        ("[1].none?(1, 2, a: 1)", "V0301", "none?"),
        ("[1].one?(1)", "V0301", "one?"),
        ("[1].to_h(1, a: 1)", "V0301", "to_h"),
        ("[1].uniq(a: 1)", "V0302", "a:"),
        ("[1].fetch", "V0301", "fetch"),
        ("[1].fetch(\"a\") { |i| i }", "V0101", "\"a\""),
        ("[1].fetch(1.5)", "V0101", "1.5"),
        ("[1].sum(1, 2)", "V0301", "sum"),
        ("[1].grep", "V0301", "grep"),
        ("[1].fill", "V0301", "fill"),
        ("[1].fill(1, 2, 3) { |i| i }", "V0305", "{"),
        ("[1].delete(1, 2) { |x| x }", "V0301", "delete"),
        ("[1].sort(1)", "V0301", "sort"),
        ("[1].min_by", "V0304", "min_by"),
        ("[1].min { |x| x }", "V0305", "{"),
        ("[1].minmax { |x| x }", "V0305", "{"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn array_members_name_themselves_in_count_and_index_errors() {
    let cases = [
        ("[1].at(1, 2)", "array.at expects exactly one index"),
        ("[1].last(-1)", "array.last expects non-negative integer"),
        ("[1].take", "array.take expects exactly one count"),
        (
            "[1].first(-(2**70))",
            "array.first expects non-negative integer",
        ),
        ("[1].drop(-1)", "array.drop attempted with negative size"),
        (
            "[1].values_at(2**70)",
            "array.values_at index must be integer",
        ),
        (
            "[1].window(0)",
            "array.window size must be a positive integer",
        ),
        (
            "[1].fill(0, 0..1, 2)",
            "array.fill does not accept a length with a range",
        ),
        ("[1].fill(0, 0, 2**70)", "array.fill length must be integer"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("[1, 2].size(1)", "V0401", "size"),
        ("[1].empty?(1)", "V0301", "empty?"),
        ("[1].include?", "V0301", "include?"),
        ("[1][nil]", "V0107", "nil"),
        ("[1].slice", "V0401", "slice"),
        ("[1][1..2, 1]", "V0101", "1..2"),
        ("[1][0, \"a\"]", "V0101", "\"a\""),
        ("[1].first(1, 2)", "V0301", "first"),
        ("[1].drop(nil)", "V0101", "nil"),
        ("[1].dig", "V0301", "dig"),
        ("{a: 1}.dig", "V0301", "dig"),
        ("{a: 1}.fetch", "V0301", "fetch"),
        ("[1].flatten(1, 2)", "V0301", "flatten"),
        ("[1].flatten(\"a\")", "V0101", "\"a\""),
        ("[1].chunk", "V0301", "chunk"),
        ("[1].chunk(\"a\")", "V0101", "\"a\""),
        ("[1].join(\",\", \",\")", "V0301", "join"),
        ("[1].reverse(1)", "V0301", "reverse"),
        ("[1].transpose(1)", "V0203", "transpose"),
        ("[1].pop(1, 2)", "V0301", "pop"),
        ("[1].shift(\"a\")", "V0101", "\"a\""),
        ("[1].insert", "V0301", "insert"),
        ("[1].insert(nil, 1)", "V0101", "nil"),
        ("[1].clear(1)", "V0301", "clear"),
        ("[1].fill(0, \"a\")", "V0101", "\"a\""),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn array_keyword_and_block_refusals_follow_the_reference_order() {
    let cases = [(
        "[1].at(1, a: 1)",
        "array.at does not take keyword arguments",
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("[1].push(a: 1)", "V0302", "a:"),
        ("[1].prepend(a: 1)", "V0302", "a:"),
        ("[1].reverse(1, a: 1)", "V0301", "reverse"),
        ("[1].compact(a: 1)", "V0302", "a:"),
        ("[1].shift(1, 2, a: 1)", "V0301", "shift"),
        ("[1].pop(1, 2, a: 1)", "V0301", "pop"),
        ("[1].transpose(a: 1)", "V0203", "transpose"),
        ("[1].clear(a: 1) { |x| x }", "V0302", "a:"),
        ("[1].to_s(1, a: 1)", "V0301", "to_s"),
        ("[1].string(a: 1)", "V0401", "string"),
        ("[1].union(1, a: 1)", "V0101", "1"),
        ("[1].inspect(1, a: 1)", "V0301", "inspect"),
        ("[1].inspect { |x| x }", "V0305", "{"),
        ("\"s\".inspect(a: 1)", "V0302", "a:"),
        ("{a: 1}.inspect { |x| x }", "V0305", "{"),
        ("nil.inspect(1)", "V0301", "inspect"),
        ("(1..2).inspect(a: 1)", "V0302", "a:"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn array_element_bounds_and_comparison_errors_use_reference_wording() {
    let cases = [
        (
            "[1].to_h { |x| x }",
            "array.to_h expects an array of two-element pairs",
        ),
        (
            "[1].to_h { |x| [x] }",
            "array.to_h pair must have exactly two elements",
        ),
        (
            "[[1], [1, 2]].transpose",
            "array.transpose requires equal-length rows, but element at index 1 has length 2 (expected 1)",
        ),
        (
            "[1, 2, 3].fetch(-7)",
            "array.fetch index -7 outside of array bounds: -3...3",
        ),
        (
            "[].fetch(0)",
            "array.fetch index 0 outside of array bounds: 0...0",
        ),
        (
            "[1, 2, 3].values_at(-5...-4)",
            "array.values_at range -5...-4 out of range",
        ),
        (
            "[1, 2, 3].values_at(-5...)",
            "array.values_at range -5.. out of range",
        ),
        ("[1, 2].insert(-4, 1)", "array.insert index -4 out of range"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("[1].to_h", "V0304", "to_h"),
        ("[[1]].to_h", "V0304", "to_h"),
        ("[[1], 2].transpose", "V0203", "transpose"),
        ("[1].zip([1], 2)", "V0101", "2"),
        ("[1].difference([1], 1)", "V0101", "1"),
        ("[1].join(nil)", "V0101", "nil"),
        ("[1, \"a\"].sum", "V0115", "sum"),
        ("[1, nil].sum", "V0115", "sum"),
        ("[1].sum(0) { |x| nil }", "V0101", "nil"),
        ("[1].sum { |x| \"a\" }", "V0101", "\"a\""),
        ("[1, 2, 3].fill(-5..) { |i| i }", "V0101", "5.."),
        ("[1, \"a\"].sort", "V0115", "sort"),
        ("[1, 2].sort { |a, b| \"x\" }", "V0101", "\"x\""),
        (
            "[1, 2].sort_by { |x| x == 1 ? \"a\" : 1 }",
            "V0115",
            "sort_by",
        ),
        ("[1, \"a\"].minmax", "V0115", "minmax"),
        (
            "[1, 2].max_by { |x| x == 1 ? \"a\" : 1 }",
            "V0115",
            "max_by",
        ),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn string_members_name_themselves_in_argument_count_errors() {
    let cases = [
        ("\"ab\".clear(1)", "string.clear does not take arguments"),
        (
            "\"ab\".replace",
            "string.replace expects exactly one replacement",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("\"ab\".size(1)", "V0401", "size"),
        ("\"ab\".length(1, 2)", "V0301", "length"),
        ("\"ab\".bytesize(1)", "V0301", "bytesize"),
        ("\"ab\".empty?(nil)", "V0301", "empty?"),
        ("\"ab\".ord(1)", "V0301", "ord"),
        ("\"ab\".chr(1)", "V0301", "chr"),
        ("\"ab\".chars(1)", "V0301", "chars"),
        ("\"ab\".lines(1)", "V0301", "lines"),
        ("\"ab\".bytes(1)", "V0301", "bytes"),
        ("\"ab\".codepoints(1)", "V0301", "codepoints"),
        ("\"ab\".reverse(1)", "V0301", "reverse"),
        ("\"ab\".reverse!(1)", "V0301", "reverse!"),
        ("\"ab\".strip(1)", "V0301", "strip"),
        ("\"ab\".lstrip!(1)", "V0301", "lstrip!"),
        ("\"ab\".rstrip(1)", "V0301", "rstrip"),
        ("\"ab\".squish!(1)", "V0301", "squish!"),
        ("\"ab\".chop(1)", "V0301", "chop"),
        ("\"ab\".chomp!(\"a\", \"b\")", "V0301", "chomp!"),
        ("\"ab\".delete_prefix", "V0301", "delete_prefix"),
        (
            "\"ab\".delete_suffix!(\"a\", \"b\")",
            "V0301",
            "delete_suffix!",
        ),
        ("\"ab\".start_with?", "V0301", "start_with?"),
        ("\"ab\".end_with?", "V0301", "end_with?"),
        ("\"ab\".include?", "V0301", "include?"),
        ("\"ab\".index", "V0301", "index"),
        ("\"ab\".rindex(\"a\", 1, 2)", "V0301", "rindex"),
        ("\"ab\".casecmp", "V0301", "casecmp"),
        ("\"ab\".casecmp?(\"a\", \"b\")", "V0301", "casecmp?"),
        ("\"ab\".partition", "V0301", "partition"),
        ("\"ab\".rpartition(\"a\", \"b\")", "V0301", "rpartition"),
        ("\"ab\".center", "V0301", "center"),
        ("\"ab\".rjust(1, \"a\", \"b\")", "V0301", "rjust"),
        ("\"ab\".split(\" \", 1, 2)", "V0301", "split"),
        ("\"ab\".count", "V0301", "count"),
        ("\"ab\".delete!", "V0301", "delete!"),
        ("\"ab\".tr(\"a\")", "V0301", "tr"),
        ("\"ab\".upcase(:ascii, :ascii)", "V0301", "upcase"),
        ("\"ab\".swapcase!(1, 2)", "V0301", "swapcase!"),
        ("\"ab\".template", "V0301", "template"),
        ("\"ab\".insert(1)", "V0301", "insert"),
        ("\"ab\".each_char", "V0304", "each_char"),
        ("\"ab\".each_codepoint", "V0304", "each_codepoint"),
        ("\"ab\".each_line(1) { |line| line }", "V0301", "each_line"),
        ("\"ab\".each_byte(1) { |byte| byte }", "V0301", "each_byte"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn string_members_name_themselves_in_index_offset_and_width_errors() {
    let cases = [
        (
            "\"ab\".slice(2**70)",
            "string.slice index must be an integer, range, or substring",
        ),
        (
            "\"ab\".slice(\"a\", 1)",
            "string.slice index must be integer",
        ),
        (
            "\"ab\".slice(0..1, 1)",
            "string.slice index must be integer",
        ),
        (
            "\"ab\".byteslice(0..1, 1)",
            "string.byteslice start must be an integer",
        ),
        (
            "\"ab\".index(\"a\", 2**70)",
            "string.index offset must be integer",
        ),
        (
            "\"ab\".insert(10, \"x\")",
            "string.insert index 10 out of string",
        ),
        (
            "\"ab\".insert(-4, \"x\")",
            "string.insert index -4 out of string",
        ),
        ("\"ab\".rjust(2**70)", "string.rjust width is out of range"),
        (
            "\"ab\".split(\",\", 2**70)",
            "string.split limit must fit in a 64-bit integer",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("\"ab\".slice", "V0301", "slice"),
        ("\"ab\".slice(1, 2, 3)", "V0301", "slice"),
        ("\"ab\".slice(nil)", "V0101", "nil"),
        ("\"ab\".slice(0, nil)", "V0101", "nil"),
        ("\"ab\".byteslice", "V0301", "byteslice"),
        ("\"ab\".byteslice(:a)", "V0101", ":a"),
        ("\"ab\".byteslice(\"a\", 1)", "V0101", "\"a\""),
        ("\"ab\".byteslice(0, 1.0 / 0)", "V0101", "1.0 / 0"),
        ("\"ab\".getbyte", "V0301", "getbyte"),
        ("\"ab\".getbyte(0, 1)", "V0301", "getbyte"),
        ("\"ab\".getbyte(\"a\")", "V0101", "\"a\""),
        ("\"ab\".index(\"a\", \"b\")", "V0101", "\"b\""),
        ("\"ab\".rindex(\"a\", nil)", "V0101", "nil"),
        ("\"ab\".insert(\"a\", \"b\")", "V0101", "\"a\""),
        ("\"ab\".insert(nil, 1)", "V0101", "nil"),
        ("\"ab\".insert(3.5, \"x\")", "V0101", "3.5"),
        ("\"ab\".center(\"a\")", "V0101", "\"a\""),
        ("\"ab\".ljust(nil, \"x\")", "V0101", "nil"),
        ("\"ab\".center(1.0 / 0)", "V0101", "1.0 / 0"),
        ("\"ab\".ljust(0.0 / 0)", "V0101", "0.0 / 0"),
        ("\"ab\".rjust(1e30)", "V0101", "1e30"),
        ("\"ab\".split(\",\", \"a\")", "V0101", "\"a\""),
        ("\"ab\".split(\",\", 1.5)", "V0101", "1.5"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn string_member_arguments_report_type_and_value_errors_in_reference_wording() {
    let cases = [
        (
            "\"ab\".ljust(5, \"\")",
            "string.ljust pad must not be empty",
        ),
        (
            "\"ab\".count(\"z-a\")",
            "string.count invalid character range z-a",
        ),
        (
            "\"ab\".delete!(\"é-a\")",
            "string.delete! invalid character range é-a",
        ),
        (
            "\"ab\".squeeze(\"a-\\xff\")",
            "string.squeeze invalid mixed byte/rune character range",
        ),
        (
            "\"ab\".tr(\"\\xff-\\xfe\", \"a\")",
            "string.tr invalid character range ff-fe",
        ),
        (
            "\"ab\".tr(\"a\", \"c-b\")",
            "string.tr invalid character range c-b",
        ),
        (
            "\"{{ user.name }}\".template({user: {}}, strict: true)",
            "string.template missing placeholder user.name",
        ),
        (
            "\"{{a..b}}\".template({a: {}}, strict: true)",
            "string.template missing placeholder a..b",
        ),
        (
            "\"{{ items }}\".template({items: [1]})",
            "string.template placeholder items value must be scalar",
        ),
        (
            "\"{{ a.b }}\".template({a: {b: {c: 1}}})",
            "string.template placeholder a.b value must be scalar",
        ),
        ("\"\".ord", "string.ord requires non-empty string"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("\"ab\".concat(\"c\", 1)", "V0101", "1"),
        ("\"ab\".prepend(:a)", "V0101", ":a"),
        ("\"ab\".replace(nil)", "V0401", "replace"),
        ("\"ab\".insert(0, 1)", "V0101", "1"),
        ("\"ab\".start_with?(\"z\", 1)", "V0101", "1"),
        ("\"ab\".end_with?(nil)", "V0101", "nil"),
        ("\"ab\".include?(1)", "V0101", "1"),
        ("\"ab\".chomp(1)", "V0101", "1"),
        ("\"ab\".chomp!(:b)", "V0101", ":b"),
        ("\"ab\".delete_prefix(1)", "V0101", "1"),
        ("\"ab\".delete_suffix!(nil)", "V0101", "nil"),
        ("\"ab\".partition(1)", "V0101", "1"),
        ("\"ab\".rpartition(nil)", "V0101", "nil"),
        ("\"ab\".center(5, 1)", "V0101", "1"),
        ("\"ab\".index(1)", "V0101", "1"),
        ("\"ab\".rindex(nil, 0)", "V0101", "nil"),
        ("\"ab\".split(1)", "V0101", "1"),
        ("\"ab\".count(1)", "V0101", "1"),
        ("\"ab\".delete(\"a\", nil)", "V0101", "nil"),
        ("\"ab\".squeeze!(1)", "V0101", "1"),
        ("\"ab\".tr(1, \"a\")", "V0101", "1"),
        ("\"ab\".tr!(\"a\", nil)", "V0101", "nil"),
        ("\"ab\".tr(\"z-a\", 1)", "V0101", "1"),
        ("\"ab\".upcase(1)", "V0101", "1"),
        ("\"ab\".downcase!(\"ascii\")", "V0101", "\"ascii\""),
        ("\"ab\".upcase(:fold)", "V0101", ":fold"),
        ("\"ab\".capitalize!(:turkic)", "V0101", ":turkic"),
        ("\"ab\".swapcase(:bogus)", "V0101", ":bogus"),
        ("\"ab\".template(1)", "V0101", "1"),
        ("\"ab\".template([])", "V0101", "[]"),
        ("\"ab\".template({}, strict: 1)", "V0101", "1"),
        ("\"ab\".template({}, other: true)", "V0302", "other:"),
        (
            "\"ab\".template({}, strict: true, other: true)",
            "V0302",
            "other:",
        ),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn string_keyword_and_block_refusals_follow_the_reference_order() {
    for (body, code, text) in [
        ("\"ab\".to_s(1)", "V0301", "to_s"),
        ("\"ab\".string(1, a: 1)", "V0401", "string"),
        ("\"ab\".to_sym(a: 1)", "V0302", "a:"),
        ("\"ab\".intern { |x| x }", "V0401", "intern"),
        ("\"ab\".to_s(a: 1) { |x| x }", "V0302", "a:"),
        ("\"ab\".to_sym(1) { |x| x }", "V0301", "to_sym"),
        ("\"ab\".getbyte(0, a: 1)", "V0302", "a:"),
        ("\"ab\".getbyte(a: 1)", "V0301", "getbyte"),
        ("\"ab\".byteslice(a: 1)", "V0301", "byteslice"),
        ("\"ab\".chars(a: 1)", "V0302", "a:"),
        ("\"ab\".lines(a: 1)", "V0302", "a:"),
        ("\"ab\".bytes(a: 1)", "V0302", "a:"),
        ("\"ab\".codepoints(a: 1)", "V0302", "a:"),
        ("\"ab\".each_char(a: 1) { |c| c }", "V0302", "a:"),
        ("\"ab\".count(a: 1)", "V0301", "count"),
        ("\"ab\".count(\"a\", a: 1)", "V0302", "a:"),
        ("\"ab\".count(\"a\") { |c| c }", "V0305", "{"),
        ("\"ab\".delete!(a: 1) { |c| c }", "V0301", "delete!"),
        ("\"ab\".delete(\"a\", a: 1)", "V0302", "a:"),
        ("\"ab\".tr(\"a\", a: 1)", "V0301", "tr"),
        ("\"ab\".tr!(\"a\", \"b\", a: 1)", "V0302", "a:"),
        ("\"ab\".tr(\"a\", \"b\") { |c| c }", "V0305", "{"),
        ("\"ab\".squeeze(a: 1)", "V0302", "a:"),
        ("\"ab\".squeeze! { |c| c }", "V0305", "{"),
        ("\"ab\".center(5, a: 1)", "V0302", "a:"),
        ("\"ab\".ljust(a: 1)", "V0301", "ljust"),
        ("\"ab\".partition(\"a\", a: 1)", "V0302", "a:"),
        ("\"ab\".rpartition(a: 1)", "V0301", "rpartition"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn hash_members_name_themselves_in_argument_count_and_shape_errors() {
    let cases = [("{a: 1}.store(:a)", "hash.store expects a key and a value")];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("{a: 1}.size(1)", "V0401", "size"),
        ("{a: 1}.empty?(1, k: 1)", "V0301", "empty?"),
        ("{a: 1}.keys(1)", "V0301", "keys"),
        ("{a: 1}.has_key?", "V0401", "has_key?"),
        ("{a: 1}.include?(:a, :b)", "V0401", "include?"),
        ("{a: 1}.has_value?(1, 2)", "V0401", "has_value?"),
        ("{a: 1}.to_a(1)", "V0301", "to_a"),
        ("{a: 1}.delete(:a, :b)", "V0301", "delete"),
        ("{a: 1}.clear(1)", "V0301", "clear"),
        ("{a: 1}.replace({}, {})", "V0301", "replace"),
        ("{a: 1}.replace(1)", "V0101", "1"),
        ("{a: 1}.flatten(1, 2)", "V0301", "flatten"),
        ("{a: 1}.flatten(nil)", "V0101", "nil"),
        ("{a: 1}.remap_keys([])", "V0101", "[]"),
        ("{a: 1}.remap_keys()", "V0301", "remap_keys"),
        ("{a: 1}.compact(1)", "V0301", "compact"),
        ("h = {a: 1}\nh.delete", "V0301", "delete"),
        ("{a: 1}.remap_keys", "V0301", "remap_keys"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn hash_keyword_and_block_refusals_follow_the_reference_order() {
    let cases = [(
        "{a: 1}.store(:a, 1, k: 1)",
        "hash.store does not accept keyword arguments".to_owned(),
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("{a: 1}.each(1)", "V0301", "each"),
        ("{a: 1}.each", "V0301", "each"),
        (
            "{a: 1}.each_with_index(1, k: 1)",
            "V0301",
            "each_with_index",
        ),
        ("{a: 1}.each_with_index(k: 1)", "V0304", "each_with_index"),
        ("{a: 1}.map(k: 1) { |k, v| k }", "V0301", "map"),
        ("{a: 1}.map_with_index", "V0304", "map_with_index"),
        ("{a: 1}.select(1) { |k, v| k }", "V0301", "select"),
        ("{a: 1}.transform_values", "V0304", "transform_values"),
        ("{a: 1}.delete_if(1, k: 1)", "V0301", "delete_if"),
        ("{a: 1}.keep_if(k: 1) { |k, v| k }", "V0302", "k:"),
        ("{a: 1}.fetch(1, 2, 3) { |k| k }", "V0301", "fetch"),
        ("{a: 1}.delete(:a, :b) { |k| k }", "V0301", "delete"),
        ("{a: 1}.delete(k: 1) { |k| k }", "V0301", "delete"),
        (
            "{a: 1}.deep_transform_keys(1) { |k| k }",
            "V0301",
            "deep_transform_keys",
        ),
        ("{a: 1}.deep_transform_keys", "V0304", "deep_transform_keys"),
        ("{a: 1}.merge(1, k: 1)", "V0101", "1"),
        ("{a: 1}.merge({}, 2)", "V0101", "2"),
        ("{a: 1}.to_a(1, k: 1)", "V0301", "to_a"),
        ("{a: 1}.to_a(k: 1)", "V0302", "k:"),
        ("{a: 1}.clear(k: 1) { |x| x }", "V0302", "k:"),
        ("{a: 1}.values_at(:a, k: 1)", "V0101", ":a"),
        ("{a: 1}.flatten(1, 2, k: 1)", "V0301", "flatten"),
        ("JSON.inspect { 7 }", "V0305", "{"),
        (
            "m = \"ab\".match(/(a)(b)/)\nm.store(:a, 1, k: 1)",
            "V0107",
            "store",
        ),
        (
            "m = \"ab\".match(/(a)(b)/)\nm.clear { |x| x }",
            "V0107",
            "clear",
        ),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn hash_lookups_name_the_missing_key() {
    let cases = [(
        "{a: 1}.fetch(\"q\\\"t\\n\")",
        "hash.fetch key not found: \"q\\\"t\\n\"",
    )];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("{a: 1}.fetch(:missing)", "V0101", ":missing"),
        ("{a: 1}.fetch_values(:a, \"missing\")", "V0101", ":a"),
        ("JSON.fetch(:missing)", "V0203", "fetch"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn range_members_check_calls_in_reference_order() {
    let full = "(-9223372036854775807 - 1..9223372036854775807)";
    let cases = [
        (
            "(1..5).last(-1)",
            "range.last count must be non-negative".to_owned(),
        ),
        (
            "(1..5).first(2**70)",
            "range.first count must fit in a 64-bit integer".to_owned(),
        ),
        (
            "(..3).first",
            "cannot get the first element of a beginless range".to_owned(),
        ),
        (
            "(1..).last(2)",
            "cannot get the last element of an endless range".to_owned(),
        ),
        (
            &format!("{full}.length"),
            "range.length overflow".to_owned(),
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("(1..5).cover?(1, 2)", "V0401", "cover?"),
        ("(1..5).member?(k: 1)", "V0401", "member?"),
        ("(1..5).include?(1, k: 1)", "V0302", "k:"),
        ("(1..5).first(1, 2)", "V0301", "first"),
        ("(1..5).first(\"x\")", "V0101", "\"x\""),
        ("(..3).last(\"x\")", "V0101", "\"x\""),
        ("(1..5).first(k: 1)", "V0301", "first"),
        ("(1..5).size(1, k: 1)", "V0401", "size"),
        ("(1..5).exclude_end?(k: 1)", "V0302", "k:"),
        ("(1..5).to_a(1)", "V0301", "to_a"),
        ("(1..5).length(1, k: 1)", "V0301", "length"),
        ("(1..5).each(1, k: 1)", "V0301", "each"),
        ("(1..5).map(k: 1) { |x| x }", "V0302", "k:"),
        ("(1..5).select", "V0304", "select"),
        ("(1..5).find(1, k: 1)", "V0301", "find"),
        ("(1..5).find(nil)", "V0301", "find"),
        ("(1..5).reduce(1, 2, k: 1)", "V0301", "reduce"),
        ("(1..5).reduce(1)", "V0401", "reduce"),
        ("(1..5).count(1, k: 1)", "V0301", "count"),
        ("(1..5).to_s(1, k: 1)", "V0301", "to_s"),
        ("(1..5).string(k: 1) { |x| x }", "V0401", "string"),
        ("(1..5).to_s { |x| x }", "V0305", "{"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn universal_members_and_conversions_refuse_extra_input_in_reference_order() {
    // `nil?` is removed, with any arguments.
    for (body, expected) in [
        ("nil.nil?(1)", "nil.nil? does not take arguments"),
        (
            "[1].nil?(a: 1)",
            "array.nil? does not take keyword arguments",
        ),
        ("{a: 1}.nil? { 1 }", "hash.nil? does not take a block"),
        ("5.nil?(1, a: 1)", "int.nil? does not take arguments"),
    ] {
        assert_eq!(runtime_message(body), expected, "{body}");
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), ("V0402", "nil?"), "{body}");
    }
    let cases = [
        ("[1].itself(1)", "array.itself expects 0 arguments, got 1"),
        (
            "5.itself(1, a: 1)",
            "int.itself does not accept keyword arguments",
        ),
        ("/a/.itself { 1 }", "regex.itself does not accept a block"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("nil.to_s(1)", "V0301", "to_s"),
        ("nil.to_s(a: 1)", "V0302", "a:"),
        ("nil.to_s { 1 }", "V0305", "{"),
        ("true.string(1)", "V0401", "string"),
        (":a.id2name(a: 1)", "V0401", "id2name"),
        (":a.to_sym { 1 }", "V0305", "{"),
        ("5.to_s(1, a: 1)", "V0301", "to_s"),
        ("1.5.to_f { 1 }", "V0305", "{"),
        ("5.inspect(a: 1)", "V0302", "a:"),
        ("\"a\".to_i(1)", "V0301", "to_i"),
        ("\"a\".to_f(a: 1)", "V0302", "a:"),
        ("5.clamp(1, 2, a: 1)", "V0301", "clamp"),
        ("5.clamp(1, 2) { 1 }", "V0301", "clamp"),
        ("1.5.between?(1, 2) { 1 }", "V0305", "{"),
        ("\"a\".clamp(\"a\", \"b\") { 1 }", "V0305", "{"),
        ("\"a\".between?(\"a\", \"b\") { 1 }", "V0305", "{"),
        ("[1].dup(1)", "V0301", "dup"),
        ("[1].dup(a: 1)", "V0302", "a:"),
        ("1.seconds.dup { 1 }", "V0305", "{"),
        ("[1].tap(1) { |x| x }", "V0404", "tap"),
        ("[1].tap(a: 1) { |x| x }", "V0404", "tap"),
        ("5.yield_self(1) { |x| x }", "V0404", "yield_self"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
    let status = "enum Status\n  Draft\nend\n";
    for (body, code, text) in [
        ("Status.to_s(1)", "V0301", "to_s"),
        ("Status.inspect { 1 }", "V0305", "{"),
        ("Status::Draft.to_s(1)", "V0301", "to_s"),
        ("Status::Draft.inspect(a: 1)", "V0302", "a:"),
        ("Status::Draft.name(1)", "V0301", "name"),
    ] {
        let (found, at) = refusal(&format!("{status}def run -> any\n  {body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
}

#[test]
fn json_builtins_report_the_reference_parser_and_encoder_wording() {
    let cases = [
        (
            "JSON.parse(\"[1,\")",
            "JSON.parse invalid JSON: unexpected end of JSON input",
        ),
        (
            "JSON.parse(\"[1 2]\")",
            "JSON.parse invalid JSON: invalid character '2' after array element",
        ),
        (
            "JSON.parse(\"{\\\"a\\\" 1}\")",
            "JSON.parse invalid JSON: invalid character '1' after object key",
        ),
        (
            "JSON.parse(\"{\\\"a\\\": 1,}\")",
            "JSON.parse invalid JSON: invalid character '}' looking for beginning of object key string",
        ),
        (
            "JSON.parse(\"tru\")",
            "JSON.parse invalid JSON: invalid character 't' looking for beginning of value",
        ),
        (
            "JSON.parse(\"\\\"a\\\\qb\\\"\")",
            "JSON.parse invalid JSON: invalid character 'q' in string escape code",
        ),
        (
            "JSON.parse(\"\\\"a\\\\u12zz\\\"\")",
            "JSON.parse invalid JSON: invalid character 'z' in unicode escape",
        ),
        (
            "JSON.parse(\"\\\"a\\tb\\\"\")",
            "JSON.parse invalid JSON: invalid character '\\t' in string literal",
        ),
        (
            "JSON.parse(\"01\")",
            "JSON.parse invalid JSON: invalid number \"01\"",
        ),
        (
            "JSON.parse(\"1.\")",
            "JSON.parse invalid JSON: invalid number \"1.\"",
        ),
        (
            "JSON.parse(\"1e999\")",
            "JSON.parse invalid number \"1e999\"",
        ),
        (
            "JSON.parse(\"[1] x\")",
            "JSON.parse invalid JSON: trailing data",
        ),
        (
            "JSON.parse(\"[\" * 10001)",
            "JSON.parse invalid JSON: exceeded max depth",
        ),
        (
            "JSON.parse(\"1\" * 1048577)",
            "JSON.parse input exceeds limit 1048576 bytes",
        ),
        (
            "JSON.parse_as(\"1e999\", int)",
            "JSON.parse_as invalid number \"1e999\"",
        ),
        (
            "JSON.stringify({a: {b: [0.0/0]}})",
            "JSON.stringify key \"a\": JSON.stringify key \"b\": JSON.stringify array index 0: JSON.stringify failed: json: unsupported value: NaN",
        ),
        (
            "JSON.stringify(-1.0/0)",
            "JSON.stringify failed: json: unsupported value: -Infinity",
        ),
        (
            "JSON.stringify({\"a\\\"b\": /x/})",
            "JSON.stringify key \"a\\\"b\": JSON.stringify unsupported value type regex",
        ),
        (
            "JSON.stringify(1.seconds)",
            "JSON.stringify unsupported value type duration",
        ),
        (
            "JSON.stringify({data: \"a\" * 1048577})",
            "JSON.stringify key \"data\": JSON.stringify output exceeds limit 1048576 bytes",
        ),
        (
            "JSON.stringify([\"a\" * 1048570, \"b\" * 10])",
            "JSON.stringify array index 1: JSON.stringify output exceeds limit 1048576 bytes",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    for (body, code, text) in [
        ("JSON.parse(1)", "V0101", "1"),
        ("JSON.parse(\"1\") { 1 }", "V0305", "{"),
        ("JSON.parse_as(\"1\")", "V0301", "parse_as"),
        ("JSON.parse_as(\"1\", int, a: 1)", "V0302", "a:"),
        ("JSON.stringify(1, 2)", "V0301", "stringify"),
        ("JSON.stringify(1) { 1 }", "V0305", "{"),
        ("JSON.parse_as(\"1\", 1)", "V0101", "1"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
    // Without static types, the runtime refuses a schema that is not a type.
    let error = common::gradual_engine()
        .compile("JSON.parse_as(\"1\", 1)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(
        error.message,
        "JSON.parse_as expects a type literal as its second argument"
    );
}

#[test]
fn member_access_refusals_name_the_receiver() {
    let cases = [
        (
            "schema = {name: string}\nschema.itself { 7 }",
            "shape.itself does not accept a block",
        ),
        (
            "schema = {name: string}\nschema.itself(1)",
            "shape.itself expects 0 arguments, got 1",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    let body = "schema = {name: string}\nschema.nil?(1)";
    assert_eq!(runtime_message(body), "shape.nil? does not take arguments");
    let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
    assert_eq!((found.as_str(), at.as_str()), ("V0402", "nil?"));
    for (body, code, text) in [
        ("[1]..[2]", "V0101", "[1]"),
        ("{a: 7}::a", "V0203", "a"),
        ("f = JSON::parse\nf.foo", "V0416", "::"),
        ("a = [1]\na.length = 2", "V0203", "length"),
        ("x: int? = nil\nx.y = 1", "V0203", "y"),
        ("for n in 3\n  n\nend", "V0101", "3"),
    ] {
        let (found, at) = refusal(&format!("def run -> any\n{body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), (code, text), "{body}");
    }
    let status = "enum Status\n  Draft\nend\n";
    let source = format!("{status}def run -> any\n  Status::Draft::name\nend");
    assert_eq!(
        function_message(&source, "run"),
        "scoped member access is only supported on enums and namespaces"
    );
    for (body, text) in [
        ("Status::Draft()", "Draft"),
        ("Status::Draft.name = 3", "name"),
    ] {
        let (found, at) = refusal(&format!("{status}def run -> any\n  {body}\nend"));
        assert_eq!((found.as_str(), at.as_str()), ("V0203", text), "{body}");
    }
}
