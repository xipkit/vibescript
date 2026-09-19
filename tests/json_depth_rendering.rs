//! Rendering sinks over deeply nested values: `to_s`, interpolation, `inspect`,
//! `format` projections and the output builtins.
//!
//! The walkers keep their frames in charged buffers rather than on the native
//! stack, including at the documented 10,000-container boundary.

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Script, Value};

const DEPTH: usize = 10_000;

type Writes = Arc<Mutex<Vec<Vec<u8>>>>;

fn engine() -> (Engine, Writes) {
    let mut engine = Engine::new();
    let stdout = Writes::default();
    let buffer = stdout.clone();
    engine.set_output_writer(move |_, bytes| {
        buffer.lock().unwrap().push(bytes.to_vec());
        Ok(())
    });
    (engine, stdout)
}

fn written(writes: &Writes) -> Vec<u8> {
    writes.lock().unwrap().concat()
}

fn nested_arrays(depth: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..depth {
        value = Value::array(vec![value]);
    }
    value
}

fn nested_hashes(depth: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..depth {
        value = Value::hash(vec![(b"k".to_vec(), value)]);
    }
    value
}

fn sinks(engine: &Engine) -> Script {
    engine
        .compile(
            "def render(x)\nx.to_s\nend\n\
             def interpolate(x)\n\"<#{x}>\"\nend\n\
             def inspect_value(x)\nx.inspect\nend\n\
             def project(x)\nformat(\"%.5s|%s\", x, x)\nend\n\
             def emit(x)\nputs(x)\np(x)\nprint(x)\nnil\nend",
        )
        .unwrap()
}

fn call(script: &Script, name: &str, value: &Value) -> Vec<u8> {
    let output = script
        .call(name, std::slice::from_ref(value), CallOptions::default())
        .unwrap();
    output.value.as_bytes().unwrap().to_vec()
}

#[test]
fn nested_arrays_render_through_every_sink_at_the_construction_limit() {
    let (engine, stdout) = engine();
    let script = sinks(&engine);
    let value = nested_arrays(DEPTH, Value::int(7));
    let text = format!("{}7{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
    assert_eq!(call(&script, "render", &value), text.as_bytes());
    assert_eq!(
        call(&script, "interpolate", &value),
        format!("<{text}>").as_bytes()
    );
    assert_eq!(call(&script, "inspect_value", &value), text.as_bytes());
    assert_eq!(
        call(&script, "project", &value),
        format!("{}|{text}", &text[..5]).as_bytes()
    );
    let output = script
        .call("emit", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert_eq!(
        written(&stdout),
        format!("{text}\n{text}\n{text}").as_bytes()
    );
}

#[test]
fn nested_hashes_render_through_every_sink_at_the_construction_limit() {
    let (engine, stdout) = engine();
    let script = sinks(&engine);
    let value = nested_hashes(DEPTH, Value::bytes(b"s"));
    let text = format!("{}s{}", "{k: ".repeat(DEPTH), "}".repeat(DEPTH));
    let inspected = format!("{}\"s\"{}", "{k: ".repeat(DEPTH), "}".repeat(DEPTH));
    // Plain hashes reserve `to_s` for a field lookup; array conversion still
    // renders nested hashes through the same display walker.
    let wrapped = Value::array(vec![nested_hashes(DEPTH - 1, Value::bytes(b"s"))]);
    let wrapped_text = format!("[{}s{}]", "{k: ".repeat(DEPTH - 1), "}".repeat(DEPTH - 1));
    assert_eq!(call(&script, "render", &wrapped), wrapped_text.as_bytes());
    assert_eq!(
        call(&script, "interpolate", &value),
        format!("<{text}>").as_bytes()
    );
    assert_eq!(call(&script, "inspect_value", &value), inspected.as_bytes());
    assert_eq!(
        call(&script, "project", &value),
        format!("{}|{text}", &text[..5]).as_bytes()
    );
    let output = script
        .call("emit", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert_eq!(
        written(&stdout),
        format!("{text}\n{inspected}\n{text}").as_bytes()
    );
}

#[test]
fn values_beyond_the_construction_limit_are_refused_before_any_write() {
    let (engine, stdout) = engine();
    let script = sinks(&engine);
    let value = nested_arrays(DEPTH + 1, Value::int(7));
    for name in ["render", "interpolate", "inspect_value", "project", "emit"] {
        let error = script
            .call(name, std::slice::from_ref(&value), CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion, "{name}");
    }
    assert!(written(&stdout).is_empty());
}

#[test]
fn special_hashes_keep_their_spelling_inside_nested_containers() {
    let (engine, stdout) = engine();
    let script = engine
        .compile(
            "def run(o)\n\
             m = \"abc\".match(/b/)\n\
             x = [[m, o], {k: [o]}]\n\
             puts(x)\n\
             [x.to_s, \"#{x}\", format(\"%.9s\", x), x.inspect]\n\
             end",
        )
        .unwrap();
    let object = Value::object(vec![
        (b"z".to_vec(), Value::int(1)),
        (b"a".to_vec(), Value::nil()),
    ]);
    let output = script
        .call("run", &[object], CallOptions::default())
        .unwrap();
    let result = output.value.as_array().unwrap();
    let plain = b"[[b, <object>], {k: [<object>]}]";
    assert_eq!(result[0].as_bytes().unwrap(), plain);
    assert_eq!(result[1].as_bytes().unwrap(), plain);
    assert_eq!(result[2].as_bytes().unwrap(), &plain[..9]);
    // Match data inspects as an object hash with sorted fields; host objects sort too.
    let inspected = result[3].as_bytes().unwrap();
    assert!(inspected.starts_with(b"[[{begin: <builtin>, captures: [], end: <builtin>, "));
    assert!(inspected.ends_with(b", to_s: \"b\"}, {a: nil, z: 1}], {k: [{a: nil, z: 1}]}]"));
    assert_eq!(written(&stdout), b"[[b, <object>], {k: [<object>]}]\n");
}

#[test]
fn interrupted_deep_renders_write_nothing_and_release_scratch() {
    let (engine, stdout) = engine();
    let script = sinks(&engine);
    let value = nested_arrays(DEPTH, Value::int(7));
    let token = CancellationToken::new();
    token.cancel();
    let cases = [
        (
            CallOptions {
                cancellation: token,
                ..CallOptions::default()
            },
            ErrorKind::Cancelled,
        ),
        (
            CallOptions {
                deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
                ..CallOptions::default()
            },
            ErrorKind::Deadline,
        ),
        (
            CallOptions {
                limits: Limits {
                    steps: Some(DEPTH as u64 * 3),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Steps,
        ),
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(DEPTH * 24),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ),
    ];
    for (options, kind) in cases {
        let error = script
            .call("emit", std::slice::from_ref(&value), options)
            .unwrap_err();
        assert_eq!(error.kind, kind);
    }
    assert!(written(&stdout).is_empty());
}
