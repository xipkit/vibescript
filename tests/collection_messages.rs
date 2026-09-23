//! Collection, data and member error messages match the reference
//! implementation's wording exactly, since scripts read `e.message` and hosts
//! log it. Each expected message was checked against the Go reference.

use vibescript::{CallOptions, Engine};

fn message(body: &str) -> String {
    let source = format!("def run\n{body}\nend");
    let script = Engine::new()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{body}: {error}"));
    match script.call("run", &[], CallOptions::default()) {
        Ok(outcome) => panic!("{body}: expected an error, got {}", outcome.value),
        Err(error) => error.message,
    }
}

const KEY_RULE: &str = "hash keys must be strings or symbols; convert the key with to_s";

#[test]
fn unsupported_hash_keys_name_the_kind_and_the_member_input() {
    let plain = |kind: &str| format!("unsupported hash key type {kind}: {KEY_RULE}");
    let at = |site: &str, kind: &str| format!("{site} unsupported hash key: {}", plain(kind));
    let cases = [
        ("h = {a: 1}\nh[[1]]", plain("array")),
        ("h = {a: 1}\nh[1] = 2", plain("int")),
        ("h = {a: 1}\nh[/a/]", plain("regex")),
        ("{a: {b: 1}}.dig(:a, 1)", plain("int")),
        ("{a: 1}.store(1, 2)", at("hash.store key is an", "int")),
        ("{a: 1}.fetch(nil)", at("hash.fetch key is an", "nil")),
        (
            "{a: 1}.fetch(1) { |k| 2 }",
            at("hash.fetch key is an", "int"),
        ),
        (
            "{a: 1}.fetch_values(1.5)",
            at("hash.fetch_values key is an", "float"),
        ),
        (
            "{a: 1}.values_at([1])",
            at("hash.values_at key is an", "array"),
        ),
        ("{a: 1}.has_key?(1)", at("hash.has_key? key is an", "int")),
        ("{a: 1}.include?(1)", at("hash.include? key is an", "int")),
        ("{a: 1}.member?(1)", at("hash.member? key is an", "int")),
        ("{a: 1}.delete(true)", at("hash.delete key is an", "bool")),
        ("{a: 1}.slice(1)", at("hash.slice key is an", "int")),
        ("{a: 1}.except({})", at("hash.except key is an", "hash")),
        (
            "{a: 1}.transform_keys { |k| 1 }",
            at("hash.transform_keys block returned an", "int"),
        ),
        (
            "{a: {b: 1}}.deep_transform_keys { |k| nil }",
            at("hash.deep_transform_keys block returned an", "nil"),
        ),
        (
            "{a: 1}.remap_keys({a: 1})",
            at("hash.remap_keys mapping value is an", "int"),
        ),
        ("[[1, 2]].to_h", at("array.to_h pair key is an", "int")),
        (
            "[1].to_h { |x| [x, x] }",
            at("array.to_h pair key is an", "int"),
        ),
        (
            "[1].group_by { |x| [x] }",
            at("array.group_by block returned an", "array"),
        ),
        (
            "[1].group_by_stable { |x| x }",
            at("array.group_by_stable block returned an", "int"),
        ),
        ("[1, 2].tally", at("array.tally value is an", "int")),
        (
            "[1].tally { |x| 1.5 }",
            at("array.tally value is an", "float"),
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn unknown_members_name_the_receiver_kind_and_suggest_close_names() {
    let cases = [
        ("[1].frobnicate", "unknown array method frobnicate"),
        (
            "[1].lengt",
            "unknown array method lengt (did you mean \"length\"?)",
        ),
        (
            "[1].uniq!",
            "unknown array method uniq! (did you mean \"uniq\" or \"union\"?)",
        ),
        (
            "[1].to_a",
            "unknown array method to_a (did you mean \"to_h\" or \"to_s\"?)",
        ),
        (
            "\"x\".uppcase",
            "unknown string method uppcase (did you mean \"upcase\" or \"upcase!\"?)",
        ),
        ("\"x\".push(1)", "unknown string method push"),
        (
            "{a: 1}.to_s",
            "unknown hash method to_s (did you mean \"to_a\"?)",
        ),
        (
            "{counter: 1}.countr",
            "unknown hash method countr (did you mean \"counter\"?)",
        ),
        ("nil.empty?", "unknown nil method empty?"),
        (
            "nil.inspct",
            "unknown nil method inspct (did you mean \"inspect\"?)",
        ),
        ("true.foo", "unknown bool method foo"),
        (
            "5.tims",
            "unknown int method tims (did you mean \"times\"?)",
        ),
        ("5.chr", "unknown int method chr"),
        ("1.5.foo", "unknown float method foo"),
        (
            ":a.id2nam",
            "unknown symbol method id2nam (did you mean \"id2name\"?)",
        ),
        ("(1..2).reverse", "unknown range method reverse"),
        (
            "/a/.matches?",
            "unknown regex method matches? (did you mean \"match?\"?)",
        ),
        ("money(\"1.00 USD\").nope", "unknown money member nope"),
        (
            "Time.now.yer",
            "unknown time method yer (did you mean \"year\"?)",
        ),
        (
            "1.second.in_minuts",
            "unknown duration method in_minuts (did you mean \"in_minutes\"?)",
        ),
        ("JSON.foo", "unknown hash method foo"),
        ("5.each { |x| x }", "unknown int method each"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

fn function_message(source: &str, function: &str) -> String {
    let script = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    match script.call(function, &[], CallOptions::default()) {
        Ok(outcome) => panic!("{source}: expected an error, got {}", outcome.value),
        Err(error) => error.message,
    }
}

#[test]
fn missing_names_read_as_undefined_variables_or_unknown_members() {
    let cases = [
        (
            "def run\n  length = 5\n  lengtt\nend",
            "undefined variable lengtt (did you mean \"length\"?)",
        ),
        (
            "def run\n  asert true\nend",
            "undefined variable asert (did you mean \"assert\"?)",
        ),
        (
            "def helper\n  1\nend\ndef run\n  helpr()\nend",
            "undefined variable helpr (did you mean \"helper\"?)",
        ),
        ("def run\n  zzzzzz\nend", "undefined variable zzzzzz"),
        (
            "class Greeter\n  def greet\n    1\n  end\nend\ndef run\n  Greeter.new.gret\nend",
            "unknown member gret (did you mean \"greet\"?)",
        ),
        (
            "class Vault\n  private def secret\n    1\n  end\n  def probe\n    secrez\n  end\nend\ndef run\n  Vault.new.probe\nend",
            "unknown member secrez (did you mean \"secret\"?)",
        ),
        (
            "class Vault\n  private def secret\n    1\n  end\nend\ndef run\n  Vault.new.secrez\nend",
            "unknown member secrez",
        ),
        (
            "class Counter\n  def self.instances\n    1\n  end\nend\ndef run\n  Counter.instnces\nend",
            "unknown class member instnces (did you mean \"instances\"?)",
        ),
        (
            "class A\n  attr_reader :x\nend\ndef run\n  1\nend",
            "unknown class member attr_reader (use \"getter x\"; the name is bare, not a symbol)",
        ),
        (
            "module Config\n  LIMIT = 1\nend\ndef run\n  Config::LIMT\nend",
            "unknown constant Config::LIMT (did you mean \"LIMIT\"?)",
        ),
        (
            "enum Status\n  Draft\nend\ndef run\n  Status::Drafd\nend",
            "unknown enum member Status::Drafd (did you mean \"Draft\"?)",
        ),
        (
            "enum Status\n  Draft\nend\ndef run\n  Status::Draft.strng\nend",
            "unknown enum member property strng (did you mean \"string\"?)",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(function_message(source, "run"), expected, "{source}");
    }
}

#[test]
fn index_operator_errors_name_the_selector_or_receiver() {
    let cases = [
        ("x = [1]\nx[\"a\"]", "index must be integer"),
        ("x = [1]\nx[2 ** 70]", "index must fit in a 64-bit integer"),
        ("\"abc\"[nil]", "index must be integer"),
        ("x = [1]\nx[\"a\", 3]", "index must be integer"),
        ("5[1]", "cannot index int"),
        ("nil[0]", "cannot index nil"),
        (
            "x = [1, 2, 3]\nx[1, 2, 3]",
            "array index expects one index, a start and length, or a range",
        ),
        (
            "\"abc\"[1, 2, 3]",
            "string index expects one index, a start and length, or a range",
        ),
        ("{a: 1}[1, 2]", "hash index expects a single key"),
        ("x = [1]\nx[5] = 1", "array index out of bounds"),
        ("x = [1]\nx[-5] = 1", "array index out of bounds"),
        (
            "x = [1]\nx[0, 1] = 1",
            "array index assignment expects a single index",
        ),
        (
            "x = {a: 1}\nx[:a, 1] = 1",
            "hash index assignment expects a single key",
        ),
        ("x = \"abc\"\nx[0] = \"z\"", "cannot index string"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
    let plain = "class Plain\nend\n";
    assert_eq!(
        function_message(&format!("{plain}def run\n  Plain.new[1]\nend"), "run"),
        "cannot index instance: Plain does not define []"
    );
    assert_eq!(
        function_message(
            &format!("{plain}def run\n  p = Plain.new\n  p[1] = 2\nend"),
            "run"
        ),
        "cannot index instance: Plain does not define []="
    );
}

#[test]
fn protected_records_name_the_rejected_operation() {
    let matched = "m = \"ab\".match(/(a)(b)/)\n";
    let rescued = "begin\n  raise \"x\"\nrescue RuntimeError => e\n  e\nend";
    let cases = [
        (
            format!("{matched}m[0] = \"z\""),
            "index assignment cannot modify match data",
        ),
        (
            format!("{matched}m.pre_match = \"z\""),
            "member assignment cannot modify match data",
        ),
        (
            format!("{matched}m.replace({{}})"),
            "replace cannot modify match data",
        ),
        (
            format!("{matched}m.delete_if {{ |k, v| true }}"),
            "delete_if cannot modify match data",
        ),
        (
            format!("{matched}m.send(:clear)"),
            "clear cannot modify match data",
        ),
        (
            format!("e = {rescued}\ne[:message] = \"y\""),
            "index assignment cannot modify a rescued error",
        ),
        (
            format!("e = {rescued}\ne.message = \"y\""),
            "member assignment cannot modify a rescued error",
        ),
        (
            format!("e = {rescued}\ne.store(:a, 1)"),
            "store cannot modify a rescued error",
        ),
        (
            format!("e = {rescued}\ne.delete(:message)"),
            "delete cannot modify a rescued error",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(&body), expected, "{body}");
    }
}

#[test]
fn member_misuse_names_the_member_or_module() {
    let cases = [
        (
            "class ReadOnly\n  getter name\n  def initialize(name)\n    @name = name\n  end\nend\ndef run\n  r = ReadOnly.new(\"a\")\n  r.name = \"b\"\nend",
            "cannot assign to read-only property name",
        ),
        (
            "module Billing\nend\ndef run\n  Billing.new\nend",
            "module Billing cannot be instantiated",
        ),
        (
            "def run\n  x = JSON.stringify\n  x\nend",
            "stringify is a method and cannot be used as a value; call it with stringify(...)",
        ),
        (
            "def run\n  x = Regexp.union\n  x\nend",
            "union is a method and cannot be used as a value; call it with union(...)",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(function_message(source, "run"), expected, "{source}");
    }
}

#[test]
fn block_driven_array_members_check_calls_in_reference_order() {
    let cases = [
        ("[1].each", "array.each requires a block"),
        ("[1].map(1)", "array.map requires a block"),
        (
            "[1].each_with_index(1, a: 1) { |x| x }",
            "array.each_with_index does not take arguments",
        ),
        (
            "[1].map_with_index(a: 1)",
            "array.map_with_index does not take keyword arguments",
        ),
        (
            "[1].collect_concat(1)",
            "array.flat_map does not take arguments",
        ),
        ("[1].reject(a: 1)", "array.reject requires a block"),
        ("[1].each_slice", "array.each_slice expects a slice size"),
        ("[1].each_slice(0)", "array.each_slice invalid slice size"),
        (
            "[1].each_slice(\"a\")",
            "array.each_slice invalid slice size",
        ),
        ("[1].each_slice(2)", "array.each_slice requires a block"),
        (
            "[1].each_cons(1.5) { |x| x }",
            "array.each_cons invalid size",
        ),
        ("[1].cycle(1, 2)", "array.cycle accepts at most one count"),
        ("[1].cycle(1.5)", "array.cycle count must be an integer"),
        ("[1].cycle(2**70)", "array.cycle count is out of range"),
        (
            "[1].find(nil, nil)",
            "array.find takes no fallback; a miss returns nil",
        ),
        (
            "[1].find_index(1) { |x| x }",
            "array.find_index takes a value or a block, not both",
        ),
        (
            "[1].find_index(1, -1)",
            "array.find_index offset must be non-negative integer",
        ),
        (
            "[1].rindex",
            "array.rindex expects a value (with optional offset) or a block",
        ),
        (
            "[1].reduce",
            "array.reduce requires a block or an operation",
        ),
        (
            "[1].reduce(1, 2, 3)",
            "array.reduce accepts at most an initial value and an operation",
        ),
        (
            "[1].reduce(1)",
            "array.reduce operation must be a symbol or string",
        ),
        (
            "[1].count(1, 2)",
            "array.count accepts at most one value argument",
        ),
        (
            "[1].none?(1, 2, a: 1)",
            "array.none? does not take keyword arguments",
        ),
        ("[1].one?(1)", "array.one? does not take arguments"),
        ("[1].to_h(1, a: 1)", "array.to_h does not take arguments"),
        (
            "[1].uniq(a: 1)",
            "array.uniq does not take keyword arguments",
        ),
        (
            "[1].fetch",
            "array.fetch expects index and optional default",
        ),
        (
            "[1].fetch(\"a\") { |i| i }",
            "array.fetch index must be integer",
        ),
        ("[1].fetch(1.5)", "array.fetch index must be integer"),
        (
            "[1].sum(1, 2)",
            "array.sum accepts at most an initial value",
        ),
        (
            "[1].grep",
            "array.grep expects exactly one pattern argument",
        ),
        ("[1].fill", "array.fill requires a value or a block"),
        (
            "[1].fill(1, 2, 3) { |i| i }",
            "array.fill accepts at most a start and length",
        ),
        (
            "[1].delete(1, 2) { |x| x }",
            "array.delete expects exactly one value",
        ),
        ("[1].sort(1)", "array.sort does not take arguments"),
        ("[1].min_by", "array.min_by requires a block"),
        (
            "[1].min { |x| x }",
            "array.min does not accept a block; use min_by or max_by for block-based selection",
        ),
        (
            "[1].minmax { |x| x }",
            "array.minmax does not accept a block",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn array_members_name_themselves_in_count_and_index_errors() {
    let cases = [
        ("[1, 2].size(1)", "array.size does not take arguments"),
        ("[1].empty?(1)", "array.empty? does not take arguments"),
        ("[1].include?", "array.include? expects exactly one value"),
        ("[1].at(1, 2)", "array.at expects exactly one index"),
        ("[1].at(nil)", "array.at index must be integer"),
        (
            "[1].slice",
            "array.slice expects an index, a start and length, or a range",
        ),
        ("[1].slice(1..2, 1)", "array.slice index must be integer"),
        ("[1].slice(0, \"a\")", "array.slice length must be integer"),
        ("[1].first(1, 2)", "array.first accepts at most one count"),
        ("[1].last(-1)", "array.last expects non-negative integer"),
        ("[1].take", "array.take expects exactly one count"),
        (
            "[1].take(-(2**70))",
            "array.take attempted with negative size",
        ),
        ("[1].drop(-1)", "array.drop attempted with negative size"),
        ("[1].drop(nil)", "array.drop count must be integer"),
        (
            "[1].values_at(2**70)",
            "array.values_at index must be integer",
        ),
        ("[1].dig", "array.dig expects at least one index"),
        ("{a: 1}.dig", "hash.dig expects at least one key"),
        (
            "{a: 1}.fetch",
            "hash.fetch expects key and optional default",
        ),
        (
            "[1].flatten(1, 2)",
            "array.flatten accepts at most one depth argument",
        ),
        (
            "[1].flatten(\"a\")",
            "array.flatten depth must be an integer",
        ),
        ("[1].chunk", "array.chunk expects a chunk size"),
        (
            "[1].chunk(\"a\")",
            "array.chunk size must be a positive integer",
        ),
        (
            "[1].window(0)",
            "array.window size must be a positive integer",
        ),
        (
            "[1].join(\",\", \",\")",
            "array.join accepts at most one separator",
        ),
        ("[1].reverse(1)", "array.reverse does not take arguments"),
        (
            "[1].transpose(1)",
            "array.transpose does not take arguments",
        ),
        ("[1].pop(1, 2)", "array.pop accepts at most one argument"),
        (
            "[1].shift(\"a\")",
            "array.shift expects non-negative integer",
        ),
        ("[1].insert", "array.insert expects an index"),
        ("[1].insert(nil, 1)", "array.insert index must be integer"),
        ("[1].clear(1)", "array.clear does not take arguments"),
        (
            "[1].fill(0, 0..1, 2)",
            "array.fill does not accept a length with a range",
        ),
        ("[1].fill(0, \"a\")", "array.fill start must be integer"),
        ("[1].fill(0, 0, 2**70)", "array.fill length must be integer"),
        // The index operator keeps its own wording.
        ("x = [1]\nx[1..2, 1]", "index must be integer"),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn array_keyword_and_block_refusals_follow_the_reference_order() {
    let cases = [
        (
            "[1].at(1, a: 1)",
            "array.at does not take keyword arguments",
        ),
        (
            "[1].append(a: 1)",
            "array.append does not take keyword arguments",
        ),
        (
            "[1].unshift(a: 1)",
            "array.unshift does not take keyword arguments",
        ),
        (
            "[1].reverse(1, a: 1)",
            "array.reverse does not take arguments",
        ),
        (
            "[1].compact(a: 1)",
            "array.compact does not take keyword arguments",
        ),
        (
            "[1].shift(1, 2, a: 1)",
            "array.shift accepts at most one argument",
        ),
        (
            "[1].pop(1, 2, a: 1)",
            "array.pop does not take keyword arguments",
        ),
        (
            "[1].transpose(a: 1)",
            "array.transpose does not take arguments",
        ),
        (
            "[1].clear(a: 1) { |x| x }",
            "array.clear does not take keyword arguments",
        ),
        ("[1].to_s(1, a: 1)", "array.to_s does not take arguments"),
        (
            "[1].string(a: 1)",
            "array.string does not take keyword arguments",
        ),
        (
            "[1].union(1, a: 1)",
            "array.union does not take keyword arguments",
        ),
        (
            "[1].inspect(1, a: 1)",
            "array.inspect does not take arguments",
        ),
        (
            "[1].inspect { |x| x }",
            "array.inspect does not take a block",
        ),
        (
            "\"s\".inspect(a: 1)",
            "string.inspect does not take keyword arguments",
        ),
        (
            "{a: 1}.inspect { |x| x }",
            "hash.inspect does not take a block",
        ),
        ("nil.inspect(1)", "nil.inspect does not take arguments"),
        (
            "(1..2).inspect(a: 1)",
            "range.inspect does not take keyword arguments",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn array_element_bounds_and_comparison_errors_use_reference_wording() {
    let cases = [
        (
            "[1].to_h",
            "array.to_h expects an array of two-element pairs",
        ),
        (
            "[[1]].to_h",
            "array.to_h pair must have exactly two elements",
        ),
        (
            "[1].to_h { |x| x }",
            "array.to_h expects an array of two-element pairs",
        ),
        (
            "[1].to_h { |x| [x] }",
            "array.to_h pair must have exactly two elements",
        ),
        (
            "[[1], 2].transpose",
            "array.transpose requires arrays as elements, but element at index 1 is a int",
        ),
        (
            "[[1], [1, 2]].transpose",
            "array.transpose requires equal-length rows, but element at index 1 has length 2 (expected 1)",
        ),
        ("[1].zip([1], 2)", "array.zip arguments must be arrays"),
        (
            "[1].difference([1], 1)",
            "array.difference arguments must be arrays",
        ),
        ("[1].join(nil)", "array.join separator must be string"),
        ("[1, \"a\"].sum", "array.sum cannot add incompatible values"),
        ("[1, nil].sum", "array.sum cannot add incompatible values"),
        (
            "[1].sum(0) { |x| nil }",
            "array.sum cannot add incompatible values",
        ),
        (
            "[1].sum { |x| \"a\" }",
            "array.sum cannot add incompatible values",
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
        (
            "[1, 2, 3].fill(-5..) { |i| i }",
            "array.fill range -5.. out of range",
        ),
        ("[1, 2].insert(-4, 1)", "array.insert index -4 out of range"),
        ("[1, \"a\"].sort", "array.sort values are not comparable"),
        (
            "[1, 2].sort { |a, b| \"x\" }",
            "array.sort block must return numeric comparator",
        ),
        (
            "[1, 2].sort_by { |x| x == 1 ? \"a\" : 1 }",
            "array.sort_by block values are not comparable",
        ),
        (
            "[1, \"a\"].minmax",
            "array.minmax values are not comparable",
        ),
        (
            "[1, 2].max_by { |x| x == 1 ? \"a\" : 1 }",
            "array.max_by block values are not comparable",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn string_members_name_themselves_in_argument_count_errors() {
    let cases = [
        ("\"ab\".size(1)", "string.size does not take arguments"),
        (
            "\"ab\".length(1, 2)",
            "string.length does not take arguments",
        ),
        (
            "\"ab\".bytesize(1)",
            "string.bytesize does not take arguments",
        ),
        (
            "\"ab\".empty?(nil)",
            "string.empty? does not take arguments",
        ),
        ("\"ab\".ord(1)", "string.ord does not take arguments"),
        ("\"ab\".chr(1)", "string.chr does not take arguments"),
        ("\"ab\".chars(1)", "string.chars does not take arguments"),
        ("\"ab\".lines(1)", "string.lines does not take arguments"),
        ("\"ab\".bytes(1)", "string.bytes does not take arguments"),
        (
            "\"ab\".codepoints(1)",
            "string.codepoints does not take arguments",
        ),
        (
            "\"ab\".reverse(1)",
            "string.reverse does not take arguments",
        ),
        (
            "\"ab\".reverse!(1)",
            "string.reverse! does not take arguments",
        ),
        ("\"ab\".strip(1)", "string.strip does not take arguments"),
        (
            "\"ab\".lstrip!(1)",
            "string.lstrip! does not take arguments",
        ),
        ("\"ab\".rstrip(1)", "string.rstrip does not take arguments"),
        (
            "\"ab\".squish!(1)",
            "string.squish! does not take arguments",
        ),
        ("\"ab\".chop(1)", "string.chop does not take arguments"),
        (
            "\"ab\".chomp!(\"a\", \"b\")",
            "string.chomp! accepts at most one separator",
        ),
        (
            "\"ab\".delete_prefix",
            "string.delete_prefix expects exactly one prefix",
        ),
        (
            "\"ab\".delete_suffix!(\"a\", \"b\")",
            "string.delete_suffix! expects exactly one suffix",
        ),
        (
            "\"ab\".start_with?",
            "string.start_with? expects at least one prefix",
        ),
        (
            "\"ab\".end_with?",
            "string.end_with? expects at least one suffix",
        ),
        (
            "\"ab\".include?",
            "string.include? expects exactly one substring",
        ),
        (
            "\"ab\".index",
            "string.index expects substring and optional offset",
        ),
        (
            "\"ab\".rindex(\"a\", 1, 2)",
            "string.rindex expects substring and optional offset",
        ),
        (
            "\"ab\".casecmp",
            "string.casecmp expects exactly one string",
        ),
        (
            "\"ab\".casecmp?(\"a\", \"b\")",
            "string.casecmp? expects exactly one string",
        ),
        (
            "\"ab\".partition",
            "string.partition expects exactly one separator",
        ),
        (
            "\"ab\".rpartition(\"a\", \"b\")",
            "string.rpartition expects exactly one separator",
        ),
        (
            "\"ab\".center",
            "string.center expects width and optional pad string",
        ),
        (
            "\"ab\".rjust(1, \"a\", \"b\")",
            "string.rjust expects width and optional pad string",
        ),
        (
            "\"ab\".split(\" \", 1, 2)",
            "string.split accepts at most a separator and a limit",
        ),
        (
            "\"ab\".count",
            "string.count expects at least one character set",
        ),
        (
            "\"ab\".delete!",
            "string.delete! expects at least one character set",
        ),
        (
            "\"ab\".tr(\"a\")",
            "string.tr expects source and replacement character sets",
        ),
        (
            "\"ab\".upcase(:ascii, :ascii)",
            "string.upcase accepts at most one case-mapping option",
        ),
        (
            "\"ab\".swapcase!(1, 2)",
            "string.swapcase! accepts at most one case-mapping option",
        ),
        (
            "\"ab\".template",
            "string.template expects exactly one context hash",
        ),
        ("\"ab\".clear(1)", "string.clear does not take arguments"),
        (
            "\"ab\".replace",
            "string.replace expects exactly one replacement",
        ),
        (
            "\"ab\".insert(1)",
            "string.insert expects an index and a string",
        ),
        ("\"ab\".each_char", "string.each_char requires a block"),
        (
            "\"ab\".each_codepoint",
            "string.each_codepoint requires a block",
        ),
        (
            "\"ab\".each_line(1) { |line| line }",
            "string.each_line does not take arguments",
        ),
        (
            "\"ab\".each_byte(1) { |byte| byte }",
            "string.each_byte does not take arguments",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}

#[test]
fn string_members_name_themselves_in_index_offset_and_width_errors() {
    let cases = [
        (
            "\"ab\".slice",
            "string.slice expects an index, range, or substring with optional length",
        ),
        (
            "\"ab\".slice(1, 2, 3)",
            "string.slice expects an index, range, or substring with optional length",
        ),
        (
            "\"ab\".slice(nil)",
            "string.slice index must be an integer, range, or substring",
        ),
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
            "\"ab\".slice(0, nil)",
            "string.slice length must be integer",
        ),
        (
            "\"ab\".byteslice",
            "string.byteslice expects an index, a range, or a start and length",
        ),
        (
            "\"ab\".byteslice(:a)",
            "string.byteslice index must be an integer or range",
        ),
        (
            "\"ab\".byteslice(\"a\", 1)",
            "string.byteslice start must be an integer",
        ),
        (
            "\"ab\".byteslice(0..1, 1)",
            "string.byteslice start must be an integer",
        ),
        (
            "\"ab\".byteslice(0, 1.0 / 0)",
            "string.byteslice length must be an integer",
        ),
        ("\"ab\".getbyte", "string.getbyte expects exactly one index"),
        (
            "\"ab\".getbyte(0, 1)",
            "string.getbyte expects exactly one index",
        ),
        (
            "\"ab\".getbyte(\"a\")",
            "string.getbyte index must be an integer",
        ),
        (
            "\"ab\".index(\"a\", \"b\")",
            "string.index offset must be integer",
        ),
        (
            "\"ab\".rindex(\"a\", nil)",
            "string.rindex offset must be integer",
        ),
        (
            "\"ab\".index(\"a\", 2**70)",
            "string.index offset must be integer",
        ),
        (
            "\"ab\".insert(\"a\", \"b\")",
            "string.insert index must be integer",
        ),
        (
            "\"ab\".insert(nil, 1)",
            "string.insert index must be integer",
        ),
        (
            "\"ab\".insert(10, \"x\")",
            "string.insert index 10 out of string",
        ),
        (
            "\"ab\".insert(-4, \"x\")",
            "string.insert index -4 out of string",
        ),
        (
            "\"ab\".insert(3.5, \"x\")",
            "string.insert index 3 out of string",
        ),
        (
            "\"ab\".center(\"a\")",
            "string.center width must be integer",
        ),
        (
            "\"ab\".ljust(nil, \"x\")",
            "string.ljust width must be integer",
        ),
        ("\"ab\".rjust(2**70)", "string.rjust width is out of range"),
        (
            "\"ab\".center(1.0 / 0)",
            "string.center width is out of range",
        ),
        (
            "\"ab\".ljust(0.0 / 0)",
            "string.ljust width is out of range",
        ),
        ("\"ab\".rjust(1e30)", "string.rjust width is out of range"),
        (
            "\"ab\".split(\",\", \"a\")",
            "string.split limit must be integer",
        ),
        (
            "\"ab\".split(\",\", 1.5)",
            "string.split limit must be integer",
        ),
        (
            "\"ab\".split(\",\", 2**70)",
            "string.split limit must fit in a 64-bit integer",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(message(body), expected, "{body}");
    }
}
