use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn replaced_bytes(mut bytes: &[u8]) -> String {
    let mut output = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(std::str::from_utf8(&bytes[..valid]).unwrap());
                output.push('\u{fffd}');
                bytes = &bytes[valid + 1..];
            }
        }
    }
    output
}

#[test]
fn unicode_and_invalid_bytes_cross_scan_and_copy_boundaries() {
    let script = Engine::new()
        .compile("def run(s: string) -> array<int | string>\n [s.length, JSON.parse(JSON.stringify(s)).as(string)]\nend")
        .unwrap();
    for padding in [0, 1, 15, 16, 17, 4093, 4094, 4095, 4096, 4097] {
        for sample in [
            "é界🙂".as_bytes(),
            "\u{2028}\u{2029}<>&\"\\".as_bytes(),
            b"\xff\xfe",
            b"\xc0\x80",
            b"\xed\xa0\x80",
            b"\xf4\x90\x80\x80",
            b"\xe2\x82",
            b"\x80\x80\x80\x80\x80",
        ] {
            let mut bytes = vec![b'a'; padding];
            bytes.extend_from_slice(sample);
            let expected = replaced_bytes(&bytes);
            let input = Value::bytes(bytes);
            let result = script
                .call("run", std::slice::from_ref(&input), CallOptions::default())
                .unwrap();
            let values = result.value.as_array().unwrap();
            assert_eq!(values[0].as_int(), Some(expected.chars().count() as i64));
            assert_eq!(values[1].as_bytes(), Some(expected.as_bytes()));
            let encoded = stringify_json(&input, CallOptions::default()).unwrap();
            let decoded: String =
                serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
            assert_eq!(decoded, expected, "padding {padding}, sample {sample:02x?}");
        }
    }
}

#[test]
fn unicode_bulk_operations_observe_step_limits() {
    let engine = Engine::new();
    for body in ["input.length", "JSON.stringify(input)", "JSON.parse(input)"] {
        let script = engine
            .compile(&format!("def run(input: string) -> any\n {body}\nend"))
            .unwrap();
        let input = if body == "JSON.parse(input)" {
            format!("\"{}\"", "é界🙂".repeat(10000))
        } else {
            "é界🙂".repeat(10000)
        };
        let options = CallOptions {
            limits: Limits {
                steps: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script
                .call("run", &[Value::bytes(input)], options)
                .unwrap_err()
                .kind,
            ErrorKind::Steps,
            "{body}"
        );
    }
}
