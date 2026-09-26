mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, Error, ErrorKind, Limits, Value, stringify_json,
};

type Writes = Arc<Mutex<Vec<Vec<u8>>>>;

fn engine() -> (Engine, Writes, Writes) {
    let mut engine = Engine::new();
    let stdout = Writes::default();
    let stderr = Writes::default();
    let buffer = stdout.clone();
    engine.set_output_writer(move |_, bytes| {
        buffer.lock().unwrap().push(bytes.to_vec());
        Ok(())
    });
    let buffer = stderr.clone();
    engine.set_error_writer(move |_, bytes| {
        buffer.lock().unwrap().push(bytes.to_vec());
        Ok(())
    });
    (engine, stdout, stderr)
}

fn bytes(writes: &Writes) -> Vec<u8> {
    writes.lock().unwrap().concat()
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn streams_preserve_bytes_separators_empty_calls_and_return_values() {
    let (engine, stdout, stderr) = engine();
    let script = engine
        .compile(
            r#"
def run(input: string) -> array<array<any>?>
a=puts;b=print;c=warn;d=p
puts(nil,"a\nb",input)
print("x","",nil)
warn("careful",input)
e=p("x",[1,:two,nil])
[a,b,c,d,e]
end
"#,
        )
        .unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(vec![0, 0xff, 0xc3])],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([null, null, null, null, ["x", [1, "two", null]]])
    );
    assert_eq!(
        bytes(&stdout),
        b"\n\na\nb\n\0\xff\xc3\nx\"x\"\n[1, :two, nil]\n"
    );
    assert_eq!(bytes(&stderr), b"careful\n\0\xff\xc3\n");
    let stdout = stdout.lock().unwrap();
    assert_eq!(stdout.len(), 9);
    assert_eq!(&stdout[5..7], &[Vec::<u8>::new(), Vec::new()]);
}

#[test]
fn writers_are_required_even_for_empty_calls_and_validation_precedes_rendering() {
    for method in ["puts", "print", "warn", "p"] {
        let script = Engine::new().compile(method).unwrap();
        let error = script.run(CallOptions::default()).unwrap_err();
        assert!(
            error.message.contains("writer is not configured"),
            "{method}: {error}"
        );
        // Keywords and blocks are refused before any argument or rendering
        // runs; `p` has no signature for them at all.
        let (keyword, block) = if method == "p" {
            ("V0301", "V0301")
        } else {
            ("V0302", "V0305")
        };
        for (tail, codes) in [
            ("(a:1)", vec![keyword]),
            (" {raise \"block ran\"}", vec![block]),
            (
                "(a:1) {raise \"block ran\"}",
                if method == "p" {
                    vec![keyword]
                } else {
                    vec![keyword, block]
                },
            ),
            (
                "(C.new,a:argument()) {raise \"block ran\"}",
                if method == "p" {
                    vec![keyword]
                } else {
                    vec![keyword, block]
                },
            ),
        ] {
            let mut engine = vibescript::Engine::new();
            engine.register("argument", |_, _| panic!("argument ran"));
            let source = format!(
                "class C\ndef to_s -> string\nraise \"render ran\"\nend\nend\n{method}{tail}"
            );
            let error = engine.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), codes, "{source}");
            let call = source.rfind(method).unwrap();
            assert!(error.diagnostics()[0].span.start >= call, "{source}");
        }
    }
}

#[test]
fn registered_host_functions_override_output_helpers_without_requiring_writers() {
    for name in ["puts", "print", "warn", "p"] {
        let mut engine = Engine::new();
        engine.register(name, |_, args| Ok(args[0].clone()));
        let output = engine
            .compile(&format!("{name}(7)"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_int(), Some(7));
    }
}

#[test]
fn output_helpers_read_without_arguments_write_at_once_and_cannot_escape() {
    for name in ["puts", "print", "warn", "p"] {
        let empty: &[u8] = if name == "puts" { b"\n" } else { b"" };
        let (mut engine, stdout, stderr) = engine();
        engine.register("effect", |_, _| Ok(Value::nil()));
        // Reading the helper calls it, so what follows receives its nil result.
        let source = format!("{name}&.call(effect())");
        let output = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(output.value.type_name(), "nil", "{source}");
        assert_eq!(bytes(&stdout), empty, "{source}");
        assert!(bytes(&stderr).is_empty());
        // Every other way to call its result, or to reach it by name, is
        // refused before anything runs.
        for (source, codes, at) in [
            (format!("{name}.call(effect())"), &["V0203"][..], "call"),
            (format!("{name}.call(*[effect()])"), &["V0203"], "call"),
            (
                format!("f={name}.send(:itself,effect()) {{effect()}};f(7)"),
                &["V0405", "V0310"],
                "send",
            ),
            (
                format!("h={{f:{name}.public_send(:itself)}};h.f(7)"),
                &["V0405", "V0203"],
                "public_send",
            ),
            (
                format!("{name}&.send(:itself,effect())"),
                &["V0405"],
                "send",
            ),
        ] {
            let mut engine = vibescript::Engine::new();
            engine.register("effect", |_, _| panic!("effect ran"));
            let error = engine.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), codes, "{source}");
            assert_eq!(
                error.diagnostics()[0].span.start,
                source.find(at).unwrap(),
                "{source}"
            );
        }
    }
}

#[test]
fn bare_output_helpers_run_like_empty_calls() {
    let (engine, stdout, stderr) = engine();
    let script = engine
        .compile(
            "def run -> array<nil>
  a = puts
  b = print
  c = warn
  d = p
  puts
  [a, b, c, d]
end",
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([null, null, null, null])
    );
    assert_eq!(bytes(&stdout), b"\n\n");
    assert!(bytes(&stderr).is_empty());
    // A helper takes no block, so one is refused before anything runs.
    for (source, code) in [("puts { 1 }", "V0305"), ("p {\n}", "V0301")] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
    let error = Engine::new()
        .compile("puts")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(
        error.message.contains("writer is not configured"),
        "{error}"
    );
}

#[test]
fn class_rendering_resumes_after_nested_calls_and_keeps_partial_output() {
    let (engine, stdout, _) = engine();
    let script = engine
        .compile(
            r##"
class C
property count: int
def initialize
@count=0
end
private
def to_s(*, prefix: string = "C") -> string
@count+=1
print "nested:"
"#{prefix}#{@count}"
end
end
class Bad
def to_s
raise "render failed"
end
end
c=C.new
puts("start",c,c)
begin
puts("before",Bad.new,"after")
rescue RuntimeError=>e
puts(e)
ensure
print "ensure"
end
p(c)
c.count
"##,
        )
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(output.value.as_int(), Some(2));
    assert_eq!(
        bytes(&stdout),
        b"start\nnested:C1\nnested:C2\nbefore\nrender failed\nensure<C instance>\n"
    );
    assert_eq!(output.stats.retained_memory_bytes, 0);
}

#[test]
fn non_string_and_required_to_s_methods_use_default_rendering() {
    for definition in [
        "def to_s\n7\nend",
        "def to_s\n:symbol\nend",
        "def to_s(x: any)\nraise \"called\"\nend",
        "def to_s(*, x: any)\nraise \"called\"\nend",
    ] {
        let (engine, stdout, _) = engine();
        engine
            .compile(&format!(
                "class C\n{definition}\nend\nputs(C.new,[C.new]);p(C.new)"
            ))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(
            bytes(&stdout),
            b"<C instance>\n[<C instance>]\n<C instance>\n"
        );
    }
}

#[test]
fn inspect_keeps_match_data_protected_and_returns_logical_collection_values() {
    let (engine, stdout, _) = engine();
    let output = engine
        .compile(
            r#"
a=[1];b=p(a);b.push(2)
m="b".match(/(a)?(b)/)
n=p(m)
copy=n.dup
puts(copy);p([copy])
begin
copy.as(match_data).captures.push("x")
rescue RuntimeError=>e
puts(e)
end
[a,b,n.to_s]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([[1], [1, 2], "b"]));
    let inspected = "{begin: <builtin>, captures: [nil, \"b\"], end: <builtin>, named_captures: {}, post_match: \"\", pre_match: \"\", to_s: \"b\"}";
    assert_eq!(
        bytes(&stdout),
        format!("[1]\n{inspected}\nb\n[{inspected}]\nassignment cannot modify match data\n")
            .as_bytes()
    );
}

#[test]
fn each_rendered_argument_has_its_own_recoverable_limit() {
    const LIMIT: usize = 1 << 20;
    for method in ["puts", "print", "warn", "p"] {
        let (engine, stdout, stderr) = engine();
        let script = engine
            .compile(&format!(
                "def run(input: string) -> int\n{method}(input,input)\n7\nend"
            ))
            .unwrap();
        let payload = LIMIT - if method == "p" { 2 } else { 0 };
        let output = script
            .call(
                "run",
                &[Value::bytes(vec![b'x'; payload])],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(output.value.as_int(), Some(7));
        assert_eq!(output.stats.retained_memory_bytes, 0);
        let writer = if method == "warn" { &stderr } else { &stdout };
        let writes = writer.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert!(
            writes
                .iter()
                .all(|bytes| bytes.len() == LIMIT + usize::from(method != "print"))
        );
        drop(writes);
        writer.lock().unwrap().clear();
        let source = format!(
            "def run(input: string) -> any\nbegin\n{method}(\"before\",input,\"after\")\nrescue LimitError=>e\n[e.class,e.message]\nend\nend"
        );
        let output = engine
            .compile(&source)
            .unwrap()
            .call(
                "run",
                &[Value::bytes(vec![b'x'; payload + 1])],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([
                "LimitError",
                format!("{method} output exceeds limit {LIMIT} bytes")
            ])
        );
        assert_eq!(writer.lock().unwrap().len(), 1);
    }
}

#[test]
fn escaped_inspect_output_is_capped_after_expansion() {
    let (engine, stdout, _) = engine();
    let script = engine
        .compile("def run(input: string)\np(input)\nend")
        .unwrap();
    let error = script
        .call(
            "run",
            &[Value::bytes(vec![b'\n'; 1 << 19])],
            CallOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    assert!(bytes(&stdout).is_empty());
}

#[test]
fn writer_errors_can_be_rescued_without_replaying_prior_arguments() {
    let mut engine = Engine::new();
    let writes = Writes::default();
    let seen = writes.clone();
    engine.set_output_writer(move |_, bytes| {
        seen.lock().unwrap().push(bytes.to_vec());
        if bytes == b"fail\n" {
            Err(Error::new(ErrorKind::Host, "writer failed"))
        } else {
            Ok(())
        }
    });
    let script = engine.compile("begin\nputs(\"first\",\"fail\",\"last\")\nrescue RuntimeError=>e\nputs(e)\nend\nputs(\"next\")").unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(bytes(&writes), b"first\nfail\nwriter failed\nnext\n");
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn writer_cannot_hide_exhaustion_or_cancellation() {
    for kind in [ErrorKind::Steps, ErrorKind::Memory, ErrorKind::Cancelled] {
        for host_error in [false, true] {
            let mut engine = Engine::new();
            let writes = Arc::new(AtomicUsize::new(0));
            let seen = writes.clone();
            engine.set_output_writer(move |ctx, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                match kind {
                    ErrorKind::Steps => {
                        let _ = ctx.charge(u64::MAX);
                    }
                    ErrorKind::Memory => {
                        let _ = ctx.bytes(&[b'x'; 16384]);
                    }
                    ErrorKind::Cancelled => ctx.cancellation().cancel(),
                    _ => unreachable!(),
                }
                if host_error {
                    Err(Error::new(ErrorKind::Host, "hidden"))
                } else {
                    Ok(())
                }
            });
            let script = engine
                .compile("begin\nputs(\"first\",\"second\")\nrescue RuntimeError\n99\nend")
                .unwrap();
            let options = CallOptions {
                limits: Limits {
                    memory_bytes: Some(16384),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            assert_eq!(script.run(options).unwrap_err().kind, kind);
            assert_eq!(writes.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn class_rendering_obeys_recursion_and_cancellation_before_writing() {
    let (mut engine, stdout, _) = engine();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let script = engine
        .compile("class C\ndef to_s\ncancel();\"ignored\"\nend\nend\nputs(C.new,\"after\")")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert!(bytes(&stdout).is_empty());
    let script = engine
        .compile("class C\ndef to_s\nputs(self);\"unreachable\"\nend\nend\nputs(C.new)")
        .unwrap();
    let options = CallOptions {
        limits: Limits {
            recursion: 16,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Recursion);
    assert!(bytes(&stdout).is_empty());
}

#[test]
fn output_scratch_is_released_between_iterations_and_calls() {
    let mut engine = Engine::new();
    engine.set_output_writer(|_, _| Ok(()));
    let script = engine
        .compile(
            "def run(n: int)\ni=0\nwhile i<n\nputs([\"x\"*4096,nil]);p({a:i})\ni+=1\nend\nnil\nend",
        )
        .unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let short = script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    for _ in 0..3 {
        let long = script
            .call("run", &[Value::int(128)], options.clone())
            .unwrap();
        assert_eq!(long.stats.retained_memory_bytes, 0);
        assert_eq!(long.stats.peak_memory_bytes, short.stats.peak_memory_bytes);
    }
}

#[test]
fn writers_are_snapshotted_at_compile_time_and_do_not_escape_with_results() {
    let mut engine = Engine::new();
    let writer = Arc::new(AtomicUsize::new(0));
    let weak = Arc::downgrade(&writer);
    engine.set_output_writer(move |_, _| {
        writer.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let old = engine.compile("p([1])").unwrap();
    engine.set_output_writer(|_, _| Err(Error::new(ErrorKind::Host, "new writer")));
    let new = engine.compile("p([1])").unwrap();
    let result = old.run(CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1]));
    assert_eq!(weak.upgrade().unwrap().load(Ordering::SeqCst), 1);
    drop(old);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        new.run(CallOptions::default()).unwrap_err().message,
        "new writer"
    );
}

#[test]
fn writer_can_reenter_another_script_without_disturbing_pending_rendering() {
    let (inner_engine, inner_stdout, _) = engine();
    let inner = inner_engine.compile("puts(\"inner\");[7]").unwrap();
    let (mut engine, stdout, _) = engine();
    let captured = stdout.clone();
    engine.set_output_writer(move |ctx, bytes| {
        captured.lock().unwrap().push(bytes.to_vec());
        let result = inner.run(CallOptions {
            cancellation: ctx.cancellation().child_token(),
            ..CallOptions::default()
        })?;
        assert_eq!(result.value.as_array().unwrap()[0].as_int(), Some(7));
        Ok(())
    });
    let script = engine
        .compile("class C\ndef to_s -> string\n\"outer\"\nend\nend\nputs(C.new,\"after\")")
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(result.stats.retained_memory_bytes, 0);
    assert_eq!(bytes(&stdout), b"outer\nafter\n");
    assert_eq!(bytes(&inner_stdout), b"inner\ninner\n");
}

#[test]
fn every_output_quota_boundary_preserves_write_order_and_fresh_call_state() {
    let (engine, stdout, _) = engine();
    let script = engine
        .compile(
            r##"
class C
def to_s
print("<")
"#{7}"
end
end
def run
begin
puts("first",C.new,"last")
p(["q"])
rescue RuntimeError
print("rescued")
ensure
print("ensure")
end
nil
end
"##,
        )
        .unwrap();
    let baseline = script.call("run", &[], CallOptions::default()).unwrap();
    let expected = stdout.lock().unwrap().clone();
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    for limit in 0..=baseline.stats.steps {
        stdout.lock().unwrap().clear();
        let options = CallOptions {
            limits: Limits {
                steps: Some(limit),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let result = script.call("run", &[], options);
        if limit < baseline.stats.steps {
            assert_eq!(
                result.unwrap_err().kind,
                ErrorKind::Steps,
                "step budget {limit}"
            );
        } else {
            assert_eq!(result.unwrap().stats.retained_memory_bytes, 0);
        }
        assert!(
            expected.starts_with(&stdout.lock().unwrap()),
            "step budget {limit}"
        );
    }
    let peak = baseline.stats.peak_memory_bytes;
    for limit in (0..peak).step_by(64).chain([peak - 1, peak]) {
        stdout.lock().unwrap().clear();
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(limit),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let result = script.call("run", &[], options);
        if limit < peak {
            assert_eq!(
                result.unwrap_err().kind,
                ErrorKind::Memory,
                "memory budget {limit}"
            );
        } else {
            assert_eq!(result.unwrap().stats.retained_memory_bytes, 0);
        }
        assert!(
            expected.starts_with(&stdout.lock().unwrap()),
            "memory budget {limit}"
        );
    }
    stdout.lock().unwrap().clear();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(result.stats.steps, baseline.stats.steps);
    assert_eq!(
        result.stats.peak_memory_bytes,
        baseline.stats.peak_memory_bytes
    );
    assert_eq!(
        result.stats.retained_memory_bytes,
        baseline.stats.retained_memory_bytes
    );
    assert_eq!(*stdout.lock().unwrap(), expected);
}

#[test]
fn protected_value_output_preserves_the_ports_rendering_policy() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../docs/output-differences.json")).unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let (engine, stdout, stderr) = engine();
        let result = engine
            .compile(case["source"].as_str().unwrap())
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap();
        assert_eq!(json(&result.value), case["expected"], "{}", case["name"]);
        for (field, writer) in [("stdout_hex", stdout), ("stderr_hex", stderr)] {
            let hex = case["rust"][field].as_str().unwrap();
            assert_eq!(hex.len() % 2, 0, "{} {field}", case["name"]);
            let expected = hex
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(bytes(&writer), expected, "{} {field}", case["name"]);
        }
    }
}

#[test]
fn concurrent_calls_isolate_cancellation_and_pending_rendering() {
    let (mut engine, stdout, _) = engine();
    engine.register("check", |ctx, args| {
        if args[0].as_int() == Some(0) {
            ctx.cancellation().cancel();
        }
        Ok(Value::nil())
    });
    let script=engine.compile("class C\nproperty n: int\ndef initialize(n: int)\n@n=n\nend\ndef to_s -> string\ncheck(@n);@n.to_s\nend\nend\ndef run(n: int) -> int\nputs(C.new(n));n\nend").unwrap();
    common::scope(|scope| {
        let jobs = (0..8)
            .map(|n| {
                let script = &script;
                scope.spawn(move || script.call("run", &[Value::int(n)], CallOptions::default()))
            })
            .collect::<Vec<_>>();
        for (n, job) in jobs.into_iter().enumerate() {
            let result = job.join().unwrap();
            if n == 0 {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
            } else {
                assert_eq!(result.unwrap().value.as_int(), Some(n as i64));
            }
        }
    });
    let mut writes = stdout.lock().unwrap().clone();
    writes.sort();
    assert_eq!(
        writes,
        (1..8)
            .map(|n| format!("{n}\n").into_bytes())
            .collect::<Vec<_>>()
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        script
            .call(
                "run",
                &[Value::int(9)],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(stdout.lock().unwrap().len(), 7);
}
