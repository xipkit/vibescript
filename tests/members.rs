use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

#[test]
fn delete_returns_the_last_matching_stored_value() {
    for (source, kind) in [
        ("[1,1.0].delete(1)", "float"),
        ("[1.0,1].delete(1.0)", "int"),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.type_name(), kind);
    }
}

#[test]
fn shrinking_collections_release_removed_storage_and_excess_capacity() {
    for (source, expected, retained) in [
        ("a=(1..4096).to_a\na.pop(4095)\na", "[1]", 1024),
        ("a=(1..4096).to_a\na.shift(4095)\na", "[4096]", 1024),
        (
            "a=(1..4096).to_a\nwhile a.length>1\na.pop\nend\na",
            "[1]",
            1024,
        ),
        (
            "a: array<string> = []\nfor i in 1..128\na.push(\"a\"*4096)\nend\na.clear\na",
            "[]",
            1024,
        ),
        (
            "h: hash<string, int | string> = {}\nfor i in 0...128\nh[i.to_s]=\"a\"*4096\nend\nfor i in 0...127\nh.delete(i.to_s)\nend\nh[\"127\"]=7\nh",
            "{\"127\":7}",
            1024,
        ),
        (
            "h: hash<string, string> = {}\nfor i in 0...128\nh[i.to_s]=\"a\"*4096\nend\nfor i in 0...128\nh.delete(i.to_s)\nend\nh",
            "{}",
            1024,
        ),
        (
            "h: hash<string, string> = {payload:\"a\"*524288}\nh.clear\nh",
            "{}",
            1024,
        ),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: Some(5_000_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(
            encoded.value.as_bytes(),
            Some(expected.as_bytes()),
            "{source}"
        );
        assert!(
            result.stats.retained_memory_bytes < retained,
            "{source}: {:?}",
            result.stats
        );
    }
}

#[test]
fn mutators_check_expansion_and_scan_limits() {
    for (source, expected) in [
        // Inserting past the end raises instead of padding (ADR-008).
        ("[0].insert(9223372036854775807,1)", ErrorKind::Argument),
        ("[0].fill(1,9223372036854775807,1)", ErrorKind::Arithmetic),
        ("[0].fill(1,0..9223372036854775807)", ErrorKind::Arithmetic),
        ("s=\"a\"*8192\ns.prepend(s,s)", ErrorKind::Memory),
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(16_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, expected, "{source}");
    }
    // Deleting compares the shared graph once per distinct pair, not per path.
    let source = "a: array<any> = [1]\nfor i in 1..20\na=[a,a]\nend\nb=[a]\nb.delete(a)\nb.length";
    let outcome = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                steps: Some(10_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(0));
}

#[test]
fn pending_member_calls_release_their_receivers_on_control_transfers() {
    for body in [
        "input[\"items\"].push((while true\nreturn 7\nend))",
        "input[\"items\"].fetch(0).prepend((while true\nreturn 7\nend))",
        "input[\"items\"].pop.as(array<string>).push((while true\nreturn 7\nend))",
        "input[\"items\"].fetch(0).pop\ninput[\"items\"].clear\n7",
    ] {
        let source = format!(
            "def f(input: {{ items: array<array<string>> }}) -> any\n{body}\nend\ndef run(input: {{ items: array<array<string>> }}) -> int\nfor i in 1..100\nf(input)\nend\n7\nend"
        );
        let input = Value::hash(vec![(
            b"items".to_vec(),
            Value::array(vec![Value::array(vec![Value::bytes(vec![b'a'; 16384])])]),
        )]);
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(40_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0);
        let items = input.as_hash().unwrap()[0].1.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_array().unwrap().len(), 1);
    }
}

#[test]
fn string_insertion_preserves_invalid_bytes() {
    let script = Engine::new()
        .compile("def run(input: string) -> array<string>\n[input.insert(2,\"X\"),input]\nend")
        .unwrap();
    let input = Value::bytes(vec![b'a', 0xff, 0xc3, 0xa9]);
    let output = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let values = output.value.as_array().unwrap();
    assert_eq!(
        values[0].as_bytes(),
        Some([b'a', 0xff, b'X', 0xc3, 0xa9].as_slice())
    );
    assert_eq!(values[1].as_bytes(), input.as_bytes());
}
