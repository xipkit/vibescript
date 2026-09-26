mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value};

#[test]
fn captured_writes_preserve_host_inputs_and_independent_calls() {
    let script = Engine::new()
        .compile(
            "def zero(&block: () -> array<int>) -> array<int>\nyield\nend\n\
             def run(input: array<int>) -> array<array<int>>\na=input;old=a\n\
             zero {zero {a[0]=a.fetch(0)+1;a.push(7)}}\n[a,old]\nend",
        )
        .unwrap();
    let input = Value::array(vec![Value::int(2)]);
    for _ in 0..3 {
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        let updated = values[0].as_array().unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(updated[0].as_int(), Some(3));
        assert_eq!(updated[1].as_int(), Some(7));
        assert_eq!(values[1].as_array().unwrap()[0].as_int(), Some(2));
    }
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input.as_array().unwrap()[0].as_int(), Some(2));
}

#[test]
fn block_control_flow_releases_pending_arguments_and_receivers() {
    let input = Value::array((0..512).map(Value::int).collect());
    let definitions = "def zero(&block: () -> any) -> any\nyield\nend\n\
        def sink(a: any,b: any) -> any\nb\nend\n\
        def named(*, payload: any,done: any) -> any\ndone\nend\n\
        def defaulted(*, payload: any,done: any = zero {return 7}) -> any\ndone\nend\n";
    for body in [
        "a=[input];a[0]&.push(zero {return 7}.as(int))",
        "sink(input,zero {return 7})",
        "named(payload:input,done:zero {return 7})",
        "defaulted(payload:input)",
        "zero {zero {return 7}}",
        "zero {break 7}",
        "zero {next 7}",
    ] {
        let source = format!(
            "{definitions}\ndef work(input: array<int>) -> any\n{body}\nend\n\
             def run(input: array<int>) -> int\nfor i in 1..200\nwork(input)\nend\n7\nend"
        );
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(96_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
        assert!(result.stats.peak_memory_bytes < 96_000, "{body}");
    }
}

#[test]
fn block_destructuring_charges_its_copy_before_entering_the_body() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let input = Value::array((0..512).map(Value::int).collect());
    let baseline = engine
        .compile("def run(input: array<int>) -> int\ninput.length\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let script = engine
        .compile("def one(x: array<int>, &block: array<int> -> int) -> int\nyield x\nend\ndef run(input: array<int>) -> int\none(input){|(*rest)|entered().as(int)}\nend")
        .unwrap();
    for (limits, kind) in [
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes + 4096),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            Limits {
                steps: Some(baseline.stats.steps + 128),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
    ] {
        let error = script
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits,
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let result = script
        .call("run", &[input], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn block_errors_and_cancellation_prevent_later_host_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for (source, kind) in [
        (
            "def zero(&block: () -> any) -> any\nyield\nend\nzero {cancel; tick}",
            ErrorKind::Cancelled,
        ),
        (
            "def zero(&block: () -> any) -> any\nyield\nend\nzero {return 7}",
            ErrorKind::Argument,
        ),
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            kind,
            "{source}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    // Arguments to `block_given?`, a yield without a declared block and a
    // call of a missing function are refused before anything runs.
    for (source, code) in [
        ("block_given?(tick)", "V0301"),
        ("yield tick", "V0308"),
        ("missing {tick}", "V0201"),
    ] {
        let mut engine = vibescript::Engine::new();
        engine.register("tick", |_, _| panic!("tick ran"));
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, 0, "{source}");
    }
}

#[test]
fn block_recursion_and_syntax_depth_are_bounded() {
    let source =
        "def zero(&block: () -> any) -> any\nyield\nend\ndef recurse() -> any\nzero {recurse}\nend";
    let script = Engine::new().compile(source).unwrap();
    let error = script
        .call(
            "recurse",
            &[],
            CallOptions {
                limits: Limits {
                    recursion: 16,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        script
            .call(
                "recurse",
                &[],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    let source = format!("{}1{}", "zero {".repeat(1100), "}".repeat(1100));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert!(error.message.contains("nesting too deep"));
}

#[test]
fn nested_frame_storage_is_reserved_before_the_block_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let count = 512;
    let mut source =
        "def zero(&block: () -> int) -> int\nyield\nend\ndef run() -> int\n".to_owned();
    for index in 0..count {
        source.push_str(&format!("outer{index}=1\n"));
    }
    source.push_str("zero {\n");
    for index in 0..count {
        source.push_str(&format!("inner{index}=2\n"));
    }
    source.push_str("entered().as(int)\n}\nend\n");
    let script = engine.compile(&source).unwrap();
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(2 * count * size_of::<Value>()),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn breaking_a_receiving_loop_restores_local_call_lookup() {
    // A local named like a function hides it, so calling the name is
    // refused before anything runs.
    let source = "def id(x: int) -> int\nx\nend\n\
                  def receive(&block: int -> int) -> int\nid=9\nwhile true\nid=yield id(3)\nend\nid(4)\nend\n\
                  def run(input: any) -> int\nreceive {break 7}\nend";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0310", "V0310"]);
    let calls: Vec<usize> = error.diagnostics().iter().map(|d| d.span.start).collect();
    assert_eq!(
        calls,
        [source.find("id(3)").unwrap(), source.find("id(4)").unwrap()]
    );
}

/// Runs `source`'s top-level statements with static types and returns the
/// result of the last one.
fn value_of(source: &str) -> Value {
    vibescript::Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .value
}

#[test]
fn a_brace_after_a_call_starts_a_block_whose_statements_are_never_hash_entries() {
    for (source, expected) in [
        ("loop { break :done }", "done"),
        ("loop {\n  break :done\n}", "done"),
        (
            "def f -> symbol\n  [1].each { return :found }\n  :none\nend\nf",
            "found",
        ),
        (
            "def f -> symbol\n  [1].each {\n    return :found\n  }\n  :none\nend\nf",
            "found",
        ),
        ("[1].map { :a }.fetch(0)", "a"),
        ("[1].map { |n| :a }.fetch(0)", "a"),
    ] {
        let value = value_of(source);
        assert_eq!(value.type_name(), "symbol", "{source}");
        assert_eq!(value.as_bytes(), Some(expected.as_bytes()), "{source}");
    }
    // Anywhere else a brace starts a hash, whose `name :value` still labels.
    let value = value_of("h = { name: 1 }\nh.fetch(\"name\")");
    assert_eq!(value.as_int(), Some(1));
}

#[test]
fn a_brace_after_a_parenless_call_s_last_argument_is_that_call_s_block() {
    let definitions = "def twice(n: int, &block: int -> int) -> int\n  yield(n) + yield(n)\nend\n";
    for (body, expected) in [
        ("twice 2 { |n| n * 10 }", 40),
        ("x = 3\ntwice x { |n| n }", 6),
        // The nearest call takes the block.
        ("[[1, 2], [3]].map { |pair| pair.length }.sum", 3),
    ] {
        let value = value_of(&format!("{definitions}{body}"));
        assert_eq!(value.as_int(), Some(expected), "{body}");
    }
    let value = value_of(
        "groups: array<array<int>> = []\n[1, 2, 3].each_slice 2 { |s| groups << s }\ngroups.length",
    );
    assert_eq!(value.as_int(), Some(2));
}

#[test]
fn a_block_starts_on_its_call_s_line() {
    // A brace on the next line starts a hash statement of its own.
    let value = value_of("x = [1].length\n{ a: 1 }");
    assert_eq!(value.type_name(), "hash");
    // After a value, a brace cannot start a block.
    for source in ["x = (loop\n  { break 1 })", "y = 1\nz = (y { })"] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}");
    }
}

#[test]
fn a_hash_argument_to_a_call_without_parentheses_is_refused_with_a_fix() {
    for (source, fixed) in [
        ("puts { a: 1 }\n", Some("puts({ a: 1 })\n")),
        ("puts { \"a\": 1 }\n", Some("puts({ \"a\": 1 })\n")),
        (
            "x = [1].first { a: 1 }\n",
            Some("x = [1].first({ a: 1 })\n"),
        ),
        (
            "[1].each { |x| p { a: x } }\n",
            Some("[1].each { |x| p({ a: x }) }\n"),
        ),
        ("p 1, { a: 1 }\n", Some("p(1, { a: 1 })\n")),
        ("p 1, { a: 1 }, 2\n", Some("p(1, { a: 1 }, 2)\n")),
        // The block after an argument has no single repair.
        ("y = 1\np y { a: 1 }\n", None),
    ] {
        let engine = Engine::new();
        let error = engine.compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}");
        assert!(
            error.message.contains("needs parentheses"),
            "{source}: {error}"
        );
        let [diagnostic] = error.diagnostics() else {
            panic!("{source}: {:?}", error.diagnostics());
        };
        assert_eq!(diagnostic.code.to_string(), "V0002", "{source}");
        let applied = diagnostic
            .applicable_fix()
            .and_then(|fix| fix.apply(source));
        assert_eq!(applied.as_deref(), fixed, "{source}");
        if let Some(fixed) = fixed {
            let parsed = engine.compile(fixed).err();
            assert!(
                parsed.is_none_or(|error| error.kind != ErrorKind::Syntax),
                "{fixed}"
            );
        }
    }
    // A typed local in a block is a statement, not a hash entry.
    let value = value_of("[1].map { total: int = 2\n  total }.fetch(0)");
    assert_eq!(value.as_int(), Some(2));
}
