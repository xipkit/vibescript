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
