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
