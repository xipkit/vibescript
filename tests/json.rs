use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, parse_json, stringify_json};

const CAP: usize = 1 << 20;

fn options() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            memory_bytes: Some(64 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

#[test]
fn script_parse_limits_count_all_input_bytes_and_allow_the_exact_boundary() {
    for expression in ["JSON.parse(input)", "JSON.parse_as(input,int)"] {
        let script = Engine::new()
            .compile(&format!("def parse(input)\n{expression}\nend"))
            .unwrap();
        let mut raw = vec![b' '; CAP - 1];
        raw.push(b'7');
        let result = script
            .call("parse", &[Value::bytes(raw.as_slice())], options())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0);
        raw.push(b' ');
        assert_eq!(
            script
                .call("parse", &[Value::bytes(raw.as_slice())], options())
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(
            script
                .call("parse", &[Value::bytes(vec![b'?'; CAP + 1])], options())
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
    }
}

#[test]
fn script_stringify_limits_include_escapes_delimiters_and_hash_keys() {
    let script = Engine::new()
        .compile("def encode(input)\nJSON.stringify(input)\nend")
        .unwrap();
    let cases = [
        (
            Value::bytes(vec![b'a'; CAP - 2]),
            Value::bytes(vec![b'a'; CAP - 1]),
        ),
        (
            Value::bytes([vec![b'\n'; (CAP - 6) / 2], b"aaaa".to_vec()].concat()),
            Value::bytes(vec![b'\n'; (CAP - 2) / 2]),
        ),
        (
            Value::bytes("é".repeat((CAP - 2) / 2)),
            Value::bytes("é".repeat(CAP / 2)),
        ),
        (
            Value::array(vec![Value::bytes(vec![b'a'; CAP - 4])]),
            Value::array(vec![Value::bytes(vec![b'a'; CAP - 3])]),
        ),
        (
            Value::hash(vec![(b"x".to_vec(), Value::bytes(vec![b'a'; CAP - 8]))]),
            Value::hash(vec![(b"x".to_vec(), Value::bytes(vec![b'a'; CAP - 7]))]),
        ),
    ];
    for (exact, too_large) in cases {
        let result = script.call("encode", &[exact], options()).unwrap();
        assert_eq!(result.value.as_bytes().unwrap().len(), CAP);
        assert!(result.stats.retained_memory_bytes < CAP + 1024);
        assert_eq!(
            script
                .call("encode", &[too_large], options())
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
    }
    // The reference needs six bytes of headroom even for a two-byte escape.
    assert_eq!(
        script
            .call(
                "encode",
                &[Value::bytes(vec![b'\n'; (CAP - 4) / 2])],
                options()
            )
            .unwrap_err()
            .kind,
        ErrorKind::OutputLimit
    );
    let mut escaped = vec![0; (CAP - 2) / 6];
    escaped.extend_from_slice(b"aa");
    let result = script
        .call("encode", &[Value::bytes(escaped.as_slice())], options())
        .unwrap();
    assert_eq!(result.value.as_bytes().unwrap().len(), CAP);
    escaped.push(b'a');
    assert_eq!(
        script
            .call("encode", &[Value::bytes(escaped)], options())
            .unwrap_err()
            .kind,
        ErrorKind::OutputLimit
    );
}

#[test]
fn host_json_helpers_use_their_independent_execution_budgets() {
    let input = Value::bytes(vec![b'a'; CAP]);
    let encoded = stringify_json(&input, options()).unwrap();
    assert_eq!(encoded.value.as_bytes().unwrap().len(), CAP + 2);
    let parsed = parse_json(encoded.value.as_bytes().unwrap(), options()).unwrap();
    assert_eq!(parsed.value.as_bytes().unwrap(), input.as_bytes().unwrap());
    let small = CallOptions {
        limits: Limits {
            memory_bytes: Some(1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        stringify_json(&input, small.clone()).unwrap_err().kind,
        ErrorKind::Memory
    );
    assert_eq!(
        parse_json(encoded.value.as_bytes().unwrap(), small)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}
