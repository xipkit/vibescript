use super::*;
use crate::{CallOptions, Limits, budget::MAX_VALUE_DEPTH};

#[test]
fn repeated_short_strings_share_values_without_retaining_the_document() {
    let input = format!("[{}]", vec![r#""api""#; 256].join(","));
    let mut ctx = CallContext::new(CallOptions::default());
    let parsed = parse(&mut ctx, input.as_bytes()).unwrap();
    let values = parsed.as_array().unwrap();
    let crate::value::Kind::Bytes(first) = &values[0].0 else {
        panic!("string")
    };
    for value in values {
        let crate::value::Kind::Bytes(bytes) = &value.0 else {
            panic!("string")
        };
        assert!(std::sync::Arc::ptr_eq(first, bytes));
        assert_eq!(bytes.data.as_slice(), b"api");
        assert_eq!(bytes.data.capacity(), 3);
    }
    drop(parsed);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn colliding_record_keys_share_storage_without_changing_values() {
    let record = r#"{"name":"Ada","city":"Paris","name":"Grace"}"#;
    let input = format!("[{}]", vec![record; 32].join(","));
    let mut ctx = CallContext::new(CallOptions::default());
    let parsed = parse(&mut ctx, input.as_bytes()).unwrap();
    let records = parsed.as_array().unwrap();
    let first = records[0].as_hash().unwrap();
    for record in records {
        let fields = record.as_hash().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].1.as_bytes(), Some(b"Grace".as_slice()));
        assert_eq!(fields[1].1.as_bytes(), Some(b"Paris".as_slice()));
        for (actual, shared) in fields.iter().zip(first) {
            assert!(std::ptr::eq(
                actual.0.as_bytes().unwrap(),
                shared.0.as_bytes().unwrap()
            ));
        }
    }
    drop(parsed);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn compare(input: &[u8], limit: Option<u64>) {
    let run = |portable| {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: limit,
                memory_bytes: Some(64 << 20),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let mut parser = parser::Parser::new(&mut ctx, input);
        if portable {
            parser.portable();
        }
        let result = document(&mut parser);
        let failure = parser.failure;
        drop(parser);
        let stats = ctx.stats();
        let result = result
            .map(|value| {
                let mut encoder = CallContext::new(CallOptions::default());
                stringify(&mut encoder, &value)
                    .unwrap()
                    .as_bytes()
                    .unwrap()
                    .to_vec()
            })
            .map_err(|error| (error.kind, error.message));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        (
            result,
            failure,
            stats.steps,
            stats.peak_memory_bytes,
            stats.retained_memory_bytes,
        )
    };
    assert_eq!(
        run(false),
        run(true),
        "input {:?}",
        &input[..input.len().min(100)]
    );
}

#[test]
fn random_and_adversarial_documents_match_both_scanners() {
    let mut state = 0x243f6a8885a308d3u64;
    let mut next = |bound| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize % bound
    };
    let pieces = [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "9223372036854775808",
        "-9223372036854775809",
        "1.25e+30",
        "1e9999",
        "1e-9999",
        "01",
        "-",
        "1.",
        "1e+",
        "\"é界🙂\"",
        "\"\\ud800\"",
        "\"\\ud83d\\ude42\"",
        "[",
        "]",
        "{",
        "}",
        ":",
        ",",
        "\"",
        "\\",
        " ",
    ];
    for _ in 0..2500 {
        let mut input = Vec::new();
        for _ in 0..1 + next(60) {
            input.extend_from_slice(pieces[next(pieces.len())].as_bytes());
        }
        compare(&input, None);
        compare(&input, Some(next(150) as u64));
    }
    for padding in [
        0, 1, 7, 8, 15, 16, 31, 32, 63, 64, 65, 4093, 4095, 4096, 4097, 65536,
    ] {
        for tail in [
            "\"",
            "\\\"\"",
            "\\\\\"",
            "\\u0061\"",
            "\\ud800\\udc00\"",
            "\\ud800\\u0000\"",
            "é界🙂\"",
            "\\",
            "\n",
        ] {
            let input = format!("[\"{}{tail}]", "a".repeat(padding));
            compare(input.as_bytes(), None);
            let indexed = format!("{}{input}", " ".repeat(512));
            compare(indexed.as_bytes(), None);
        }
    }
    // Invalid bytes occur in values, keys, delimiters, whitespace, numbers,
    // literals and every UTF-8 lane; Vibescript replaces them inside strings.
    let source = br#" {"key":"unicode____", "a":[123.45e-6, true, null]} "#;
    for pos in 0..source.len() {
        for byte in 128..=255 {
            let mut input = source.to_vec();
            input[pos] = byte;
            compare(&input, None);
            let mut indexed = vec![b' '; 513];
            indexed.push(b'[');
            indexed.extend_from_slice(&input);
            indexed.push(b']');
            compare(&indexed, None);
        }
    }
    for depth in [MAX_VALUE_DEPTH - 1, MAX_VALUE_DEPTH, MAX_VALUE_DEPTH + 1] {
        compare(
            format!("{}0{}", "[".repeat(depth), "]".repeat(depth)).as_bytes(),
            None,
        );
    }
    for len in [19, 64, 1101, 4097, 20000] {
        for suffix in ["", ".0", "e-10000", "e99999"] {
            compare(format!("{}{}", "9".repeat(len), suffix).as_bytes(), None);
        }
    }
    for padding in 500..580 {
        for bytes in [
            b"\xc0\x80".as_slice(),
            b"\xed\xa0\x80",
            b"\xf4\x90\x80\x80",
            b"\xf0\x9f\x99",
        ] {
            let mut input = format!("[\"{}", "a".repeat(padding)).into_bytes();
            input.extend_from_slice(bytes);
            input.extend_from_slice(b"\"]");
            compare(&input, None);
        }
    }
}

#[test]
fn serde_agrees_where_json_semantics_coincide() {
    fn equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
        use serde_json::Value;
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len() && a.iter().all(|(k, a)| b.get(k).is_some_and(|b| equal(a, b)))
            }
            _ => a == b,
        }
    }
    for size in 0..250 {
        let value = serde_json::json!({"records": (0..size % 30).map(|i| serde_json::json!({
            "id": i, "score": i as f64 * 1.25, "active": i % 2 == 0,
            "name": format!("{}é界🙂\n\t\"\\", "a".repeat(size)), "tags": [null, true, "ready"]
        })).collect::<Vec<_>>()});
        let bytes = serde_json::to_vec(&value).unwrap();
        compare(&bytes, None);
        let mut ctx = CallContext::new(CallOptions::default());
        let parsed = parse(&mut ctx, &bytes).unwrap();
        let output = stringify(&mut ctx, &parsed).unwrap();
        let decoded: serde_json::Value =
            serde_json::from_slice(output.as_bytes().unwrap()).unwrap();
        // Re-encoding integral floats changes their serde number kind.
        assert!(equal(&value, &decoded));
        let again = parse(&mut ctx, &serde_json::to_vec(&decoded).unwrap()).unwrap();
        assert_eq!(
            stringify(&mut ctx, &again).unwrap().as_bytes(),
            output.as_bytes()
        );
        for at in [0, bytes.len() / 2, bytes.len() - 1] {
            let mut invalid = bytes.clone();
            invalid[at] = 0;
            assert!(serde_json::from_slice::<serde_json::Value>(&invalid).is_err());
            assert!(parse(&mut ctx, &invalid).is_err());
        }
    }
    for digits in [19, 20, 64, 1000, 4097] {
        for negative in [false, true] {
            let text = format!("{}{}", if negative { "-" } else { "" }, "9".repeat(digits));
            let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
            let mut ctx = CallContext::new(CallOptions::default());
            let parsed = parse(&mut ctx, text.as_bytes()).unwrap();
            let encoded = stringify(&mut ctx, &parsed).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(encoded.as_bytes().unwrap()).unwrap(),
                expected
            );
        }
    }
}
