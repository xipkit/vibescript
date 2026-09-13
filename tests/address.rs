use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

#[test]
fn path_storage_and_work_are_accounted_before_writing() {
    let mut input = Value::int(1);
    for _ in 0..48 {
        input = Value::array(vec![input]);
    }
    let engine = Engine::new();
    let baseline = engine
        .compile("def run(input)\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let target = format!("input{}", "[0]".repeat(48));
    let source = format!("def run(input)\n{target}=2\n{target}\nend");
    let script = engine.compile(&source).unwrap();
    for (limits, expected) in [
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes + 512),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            Limits {
                steps: Some(baseline.stats.steps + 30),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
    ] {
        assert_eq!(
            script
                .call(
                    "run",
                    std::slice::from_ref(&input),
                    CallOptions {
                        limits,
                        ..CallOptions::default()
                    }
                )
                .unwrap_err()
                .kind,
            expected
        );
    }
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(2));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let mut leaf = &input;
    for _ in 0..48 {
        leaf = &leaf.as_array().unwrap()[0];
    }
    assert_eq!(leaf.as_int(), Some(1));
}

#[test]
fn pending_writes_are_reclaimed_on_return_and_call_completion() {
    for body in [
        "input.push((while true\nreturn 7\nend))",
        "input[0].push((while true\nreturn 7\nend))",
        "input[0][0]+=(while true\nreturn 7\nend)",
        "input[0].push(input[0].push(2))\n7",
    ] {
        let source = format!(
            "def f(input)\n{body}\nend\ndef run(input)\ni=0\nwhile i<100\nf(input)\ni+=1\nend\n7\nend"
        );
        let input = Value::array(vec![Value::array(vec![
            Value::int(1),
            Value::bytes(vec![b'a'; 16384]),
        ])]);
        let script = Engine::new().compile(&source).unwrap();
        let result = script
            .call(
                "run",
                &[input],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(40_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.value.as_int(), Some(7), "{body}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
    }
}

#[test]
fn temporary_receivers_run_once_and_do_not_write_through_function_results() {
    let source =
        "def get(a)\na\nend\ndef run(input)\na=[[1]]\nget(a)[0].push(2)\nget(a)[0][0]=9\na\nend";
    let script = Engine::new().compile(source).unwrap();
    let result = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(b"[[1]]".as_slice()));
}

#[test]
fn nested_loop_expressions_preserve_the_enclosing_pending_write() {
    let source = "a=[[1]]\nx=a[0].push((for n in [1,2]\nnext if n==1\nbreak 7\nend))\n[a,x]";
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    assert_eq!(
        encoded.value.as_bytes(),
        Some(b"[[[1,7]],[1,7]]".as_slice())
    );
}
