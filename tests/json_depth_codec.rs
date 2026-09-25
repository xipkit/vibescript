use std::time::{Duration, Instant};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorClass, ErrorKind, Limits, Value, parse_json,
    stringify_json,
};

fn limits(steps: Option<u64>, memory: Option<usize>) -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: steps.or(Limits::default().steps),
            memory_bytes: memory.or(Limits::default().memory_bytes),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

fn options() -> CallOptions {
    limits(None, None)
}

fn nested(open: &str, close: &str, depth: usize, innermost: &str) -> String {
    format!("{}{innermost}{}", open.repeat(depth), close.repeat(depth))
}

/// Alternates arrays and single-key objects from the outside in.
fn mixed(depth: usize, innermost: &str) -> String {
    let mut text = String::new();
    for level in 0..depth {
        text.push_str(if level % 2 == 0 { "[" } else { "{\"k\":" });
    }
    text.push_str(innermost);
    for level in (0..depth).rev() {
        text.push(if level % 2 == 0 { ']' } else { '}' });
    }
    text
}

/// Texts containing exactly `depth` containers, each in canonical output form.
fn texts(depth: usize) -> Vec<String> {
    vec![
        nested("[", "]", depth, "0"),
        nested("[", "]", depth - 1, "[]"),
        nested("{\"a\":", "}", depth, "true"),
        nested("{\"a\":", "}", depth - 1, "{}"),
        mixed(depth, "\"x\""),
        mixed(depth - 1, "{}"),
    ]
}

/// Host values containing exactly `depth` containers around `innermost`.
fn hosts(depth: usize, innermost: Value) -> Vec<Value> {
    let mut arrays = innermost.clone();
    let mut hashes = innermost.clone();
    let mut alternating = innermost;
    for level in 0..depth {
        arrays = Value::array(vec![arrays]);
        hashes = Value::hash(vec![(b"a".to_vec(), hashes)]);
        alternating = if level % 2 == 0 {
            Value::array(vec![alternating])
        } else {
            Value::hash(vec![(b"k".to_vec(), alternating)])
        };
    }
    vec![arrays, hashes, alternating]
}

const DEPTH: usize = 10_000;

#[test]
fn parse_and_stringify_share_one_container_limit_including_empty_containers() {
    let limit = DEPTH;
    for text in texts(limit) {
        let parsed = parse_json(text.as_bytes(), options()).unwrap();
        let encoded = stringify_json(&parsed.value, options()).unwrap();
        assert_eq!(encoded.value.as_bytes(), Some(text.as_bytes()));
    }
    for text in texts(limit + 1) {
        let error = parse_json(text.as_bytes(), options()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion, "{text}");
        assert_eq!(error.class(), Some(ErrorClass::Limit));
    }
    for innermost in [Value::int(0), Value::array(vec![]), Value::hash(vec![])] {
        let extra = usize::from(innermost.as_array().is_some() || innermost.as_hash().is_some());
        for host in hosts(limit - extra, innermost.clone()) {
            let encoded = stringify_json(&host, options()).unwrap();
            let parsed = parse_json(encoded.value.as_bytes().unwrap(), options()).unwrap();
            let again = stringify_json(&parsed.value, options()).unwrap();
            assert_eq!(again.value.as_bytes(), encoded.value.as_bytes());
        }
        for host in hosts(limit + 1 - extra, innermost.clone()) {
            let error = stringify_json(&host, options()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
        }
    }
}

#[test]
fn script_builtins_apply_the_same_container_limit() {
    let limit = DEPTH;
    let parse = Engine::new()
        .compile("def run(s: string) -> any\nJSON.parse(s)\nend")
        .unwrap();
    let encode = Engine::new()
        .compile("def run(v: any) -> string\nJSON.stringify(v)\nend")
        .unwrap();
    for text in texts(limit) {
        let parsed = parse
            .call("run", &[Value::bytes(text.clone())], options())
            .unwrap();
        let encoded = encode.call("run", &[parsed.value], options()).unwrap();
        assert_eq!(encoded.value.as_bytes(), Some(text.as_bytes()));
    }
    for text in texts(limit + 1) {
        let error = parse
            .call("run", &[Value::bytes(text.clone())], options())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion, "{text}");
    }
    for host in hosts(limit, Value::int(0)) {
        let expected = stringify_json(&host, options()).unwrap();
        let encoded = encode.call("run", &[host], options()).unwrap();
        assert_eq!(encoded.value.as_bytes(), expected.value.as_bytes());
    }
    for host in hosts(limit + 1, Value::int(0)) {
        let error = encode.call("run", &[host], options()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
    }
}

#[test]
fn deepest_json_values_survive_instance_and_module_fields() {
    for source in [
        "class Box; property value: any; @flag: int; def initialize(@value: any); @flag=0; end; def touch -> int; @flag+=1; end; end; def run(input: string) -> bool; box=Box.new(JSON.parse(input)); box.touch; box.value=box.value; JSON.stringify(box.value)==input; end",
        "module Box; @@value: any = nil; @@flag: int = 0; def self.put(v: any) -> int; @@value=v; @@flag=1; end; def self.get -> any; @@value; end; end; def run(input: string) -> bool; Box.put(JSON.parse(input)); JSON.stringify(Box.get)==input; end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        for text in texts(DEPTH) {
            let result = script
                .call("run", &[Value::bytes(text)], CallOptions::default())
                .unwrap();
            assert_eq!(result.value.to_string(), "true");
            assert_eq!(result.stats.retained_memory_bytes, 0);
        }
    }
}

#[test]
fn malformed_input_after_a_complete_deep_sibling_is_recoverable() {
    let limit = DEPTH;
    let deep = nested("[", "]", limit - 1, "1");
    let script = Engine::new()
        .compile(
            "def run(s: string) -> any\nbegin\nJSON.parse(s)\nrescue => e\ne.message\nend\nend",
        )
        .unwrap();
    for (text, expected, script_message) in [
        (
            format!("[{deep},?]"),
            format!("expected JSON value at byte {}", deep.len() + 2),
            "JSON.parse invalid JSON: invalid character '?' looking for beginning of value",
        ),
        (
            format!("[{deep}]x"),
            format!("trailing JSON data at byte {}", deep.len() + 2),
            "JSON.parse invalid JSON: trailing data",
        ),
        (
            format!("{{\"a\":{deep},1}}"),
            format!("expected JSON object key at byte {}", deep.len() + 6),
            "JSON.parse invalid JSON: invalid character '1' looking for beginning of object key string",
        ),
    ] {
        let error = parse_json(text.as_bytes(), options()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Json, "{text}");
        assert_ne!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(error.message, expected);
        // Scripts see the reference parser's wording, without byte offsets.
        let result = script
            .call("run", &[Value::bytes(text.clone())], options())
            .unwrap();
        let message = std::str::from_utf8(result.value.as_bytes().unwrap()).unwrap();
        assert_eq!(message, script_message);
        // Only the rescued message survives; the deep sibling was released.
        assert!(
            result.stats.retained_memory_bytes < 512,
            "{:?}",
            result.stats
        );
    }
}

#[test]
fn deep_codec_quota_boundaries_are_exact_through_the_public_helpers() {
    let limit = DEPTH;
    let text = mixed(limit, "\"x\"");
    let baseline = parse_json(text.as_bytes(), options()).unwrap();
    let steps = baseline.stats.steps;
    let peak = baseline.stats.peak_memory_bytes;
    let exact = parse_json(text.as_bytes(), limits(Some(steps), Some(peak))).unwrap();
    assert_eq!(exact.stats.steps, steps);
    assert_eq!(exact.stats.peak_memory_bytes, peak);
    assert_eq!(
        parse_json(text.as_bytes(), limits(Some(steps - 1), None))
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    assert_eq!(
        parse_json(text.as_bytes(), limits(None, Some(peak - 1)))
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );

    let host = hosts(limit, Value::bytes(b"payload".to_vec())).remove(2);
    let baseline = stringify_json(&host, options()).unwrap();
    let steps = baseline.stats.steps;
    let peak = baseline.stats.peak_memory_bytes;
    let exact = stringify_json(&host, limits(Some(steps), Some(peak))).unwrap();
    assert_eq!(exact.value.as_bytes(), baseline.value.as_bytes());
    assert_eq!(exact.stats.steps, steps);
    assert_eq!(exact.stats.peak_memory_bytes, peak);
    assert_eq!(
        stringify_json(&host, limits(Some(steps - 1), None))
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    assert_eq!(
        stringify_json(&host, limits(None, Some(peak - 1)))
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn cancellation_and_deadlines_stop_deep_codec_work() {
    let limit = DEPTH;
    let text = nested("[", "]", limit, "0");
    let host = hosts(limit, Value::int(0)).remove(0);
    let token = CancellationToken::new();
    token.cancel();
    let cancelled = CallOptions {
        cancellation: token,
        ..options()
    };
    let expired = CallOptions {
        deadline: Some(Instant::now() - Duration::from_secs(1)),
        ..options()
    };
    for (options, kind) in [
        (cancelled, ErrorKind::Cancelled),
        (expired, ErrorKind::Deadline),
    ] {
        let error = parse_json(text.as_bytes(), options.clone()).unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.class(), None);
        let error = stringify_json(&host, options).unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.class(), None);
    }
}
