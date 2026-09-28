mod common;

use vibescript::{CallOptions, Capability, Engine, HostMethod, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

/// Compiles `source` with static types and a host `effect` that must not
/// run, and returns the codes of its errors and the text each points at.
fn refused(source: &str) -> Vec<(String, String)> {
    let mut engine = vibescript::Engine::new();
    engine.register("effect", |_, _| panic!("effect ran"));
    let error = engine
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| {
            (
                d.code.to_string(),
                source[d.span.start..d.span.end].to_owned(),
            )
        })
        .collect()
}

#[test]
fn blocks_on_non_iterating_collection_methods_fail_before_any_block_effect() {
    for expression in [
        "[1].to_s {effect()}",
        "[].to_s {effect()}",
        "[1,2,3].clear{effect()}",
        "[].clear{effect()}",
        "[1,nil,2].compact{effect()}",
        "[].compact{effect()}",
        "[1,2,3].reverse{effect()}",
        "[].reverse{effect()}",
        "{a: 1}.clear{effect()}",
        "{}.clear{effect()}",
        // A plain hash key named clear never overrides the builtin.
        "{clear: 1}.clear{effect()}",
        // In-place mutator sites on locals are refused too.
        "a=[1]; a.clear{effect()}",
        "h={a: 1}; h.clear{effect()}",
    ] {
        assert_eq!(
            refused(&format!("{expression};effect()")),
            [("V0305".to_owned(), "{".to_owned())],
            "{expression}"
        );
    }
}

#[test]
fn chunk_takes_a_size_or_a_block_but_not_both() {
    for expression in [
        "[1,2,3].chunk(2){effect()}",
        "[].chunk(2){effect()}",
        "[1].chunk(2,k:1){effect()}",
    ] {
        assert_eq!(
            refused(&format!("{expression};effect()")),
            [("V0301".to_owned(), "chunk".to_owned())],
            "{expression}"
        );
    }
}

#[test]
fn argument_and_keyword_errors_take_precedence_over_the_block() {
    // The keyword or positional argument error is reported before the block.
    for (expression, code, text) in [
        ("[1].clear(k:1){effect()}", "V0302", "k:"),
        ("[1].compact(k:1){effect()}", "V0302", "k:"),
        ("[1].reverse(k:1){effect()}", "V0302", "k:"),
        ("{a: 1}.clear(k:1){effect()}", "V0302", "k:"),
        ("[1].clear(1){effect()}", "V0301", "clear"),
        ("[1].compact(1){effect()}", "V0301", "compact"),
        ("[1].reverse(1){effect()}", "V0301", "reverse"),
        ("{a: 1}.clear(1){effect()}", "V0301", "clear"),
    ] {
        assert_eq!(
            refused(&format!("{expression};effect()")),
            [
                (code.to_owned(), text.to_owned()),
                ("V0305".to_owned(), "{".to_owned())
            ],
            "{expression}"
        );
    }
}

#[test]
fn blockless_forms_still_work() {
    for (source, expected) in [
        ("[1,2,3].reverse", serde_json::json!([3, 2, 1])),
        ("[1,nil,2].compact", serde_json::json!([1, 2])),
        ("[1,2].to_s", serde_json::json!("[1, 2]")),
        ("[].to_s", serde_json::json!("[]")),
        ("[1,2,3].chunk(2)", serde_json::json!([[1, 2], [3]])),
        ("a=[1,2]; a.clear; a", serde_json::json!([])),
        (
            "h: hash<string, int> = {a: 1}; h.clear; h",
            serde_json::json!({}),
        ),
    ] {
        let value = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(json(&value), expected, "{source}");
    }
}

fn host() -> Capability {
    let member = |name: &'static str| {
        HostMethod::new_with_block(name, |call, args, _| call.call_block(args)).value()
    };
    Capability::from_value(
        "host",
        Value::object(vec![
            (b"clear".to_vec(), member("host.clear")),
            (b"compact".to_vec(), member("host.compact")),
            (b"chunk".to_vec(), member("host.chunk")),
            (b"reverse".to_vec(), member("host.reverse")),
            (b"to_s".to_vec(), member("host.to_s")),
            (b"string".to_vec(), member("host.string")),
        ]),
    )
}

#[test]
fn callable_object_members_with_collection_names_still_accept_blocks() {
    let mut engine = Engine::new();
    engine.declare_capability(&host()).unwrap();
    for body in [
        "host.clear(3) { |n| n.as(int) + 1 }",
        "host.compact(3) { |n| n.as(int) + 1 }",
        "host.chunk(3) { |n| n.as(int) + 1 }",
        "host.reverse(3) { |n| n.as(int) + 1 }",
        "host.to_s(3) { |n| n.as(int) + 1 }",
        "host.string(3) { |n| n.as(int) + 1 }",
        "local = host; local.clear(3) { |n| n.as(int) + 1 }",
        "local = host; local.reverse(3) { |n| n.as(int) + 1 }",
    ] {
        let options = CallOptions {
            capabilities: vec![host()],
            ..CallOptions::default()
        };
        let outcome = engine
            .compile(&format!("def run -> any\n{body}\nend"))
            .unwrap_or_else(|error| panic!("{body}: {error}"))
            .call("run", &[], options)
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(outcome.value.as_int(), Some(4), "{body}");
    }
}
