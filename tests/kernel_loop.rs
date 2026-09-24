use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Stats, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn counters(stats: Stats) -> (u64, usize, usize) {
    (
        stats.steps,
        stats.peak_memory_bytes,
        stats.retained_memory_bytes,
    )
}

#[test]
fn loop_results_binding_and_nested_control_follow_the_language_contract() {
    for (source, expected) in [
        ("loop {break}", serde_json::json!(null)),
        ("loop {break false}", serde_json::json!(false)),
        ("loop do break :done end", serde_json::json!("done")),
        ("loop {|a,b|break [a,b]}", serde_json::json!([null, null])),
        (
            "loop {|(a,*rest)|break [a,rest]}",
            serde_json::json!([null, []]),
        ),
        ("loop {|v:int?|break v}", serde_json::json!(null)),
        (
            "loop {break [it,_1,_2]}",
            serde_json::json!([null, null, null]),
        ),
        ("(missing rescue loop)(){break 9}", serde_json::json!(9)),
        (
            "n=0;out=[];loop do\nn+=1\nbreak out if n>5\nnext [99] if n%2==0\nout.push(n)\nend",
            serde_json::json!([1, 3, 5]),
        ),
        (
            "n=0;out=[];loop do\nn+=1\nout.push(loop{break [n]})\nbreak out if n==3\nend",
            serde_json::json!([[1], [2], [3]]),
        ),
        (
            "out=[];value=loop do\nbegin\nbreak 7\nensure\nout.push(9)\nend\nend\n[value,out]",
            serde_json::json!([7, [9]]),
        ),
        (
            "n=0;loop do\nbegin\nn+=1\nraise \"retry\" if n<3\nbreak n\nrescue RuntimeError\nretry\nend\nend",
            serde_json::json!(3),
        ),
        (
            "def escape\nloop{return 7}\n99\nend\nescape()",
            serde_json::json!(7),
        ),
        (
            "def with_block\nloop{break yield(4)}\nend\nwith_block{|v|v+1}",
            serde_json::json!(5),
        ),
    ] {
        let output = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), expected, "{source}");
    }
}

#[test]
fn loop_validation_keeps_argument_order_and_stops_before_block_effects() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    for (source, expected, message) in [
        ("loop()", vec![], "loop requires a block"),
        (
            "loop(mark(1),flag:mark(2)){mark(3);break}",
            vec![1, 2],
            "loop does not take arguments",
        ),
        (
            "loop(flag:mark(1)){mark(2);break}",
            vec![1],
            "loop does not take keyword arguments",
        ),
        (
            "loop{|v:int|mark(1);break v}",
            vec![],
            "argument v expected int, got nil",
        ),
        (
            "loop.call(mark(1)){mark(2);break}",
            vec![],
            "loop is a method and cannot be used as a value",
        ),
    ] {
        events.lock().unwrap().clear();
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert!(error.message.starts_with(message), "{source}: {error}");
        assert_eq!(*events.lock().unwrap(), expected, "{source}");
    }
}

#[test]
fn loop_discards_normal_and_next_results_without_retaining_storage() {
    let script = Engine::new()
        .compile(
            r#"
def discard(n,skip)
 i=0
 loop do
  i+=1
  break nil if i>n
  next ["x"*4096] if skip
  ["x"*4096]
 end
end
def hold(n)
 i=0
 out=[]
 loop do
  i+=1
  out.push(["x"*512])
  break out if i==n
 end
end
"#,
        )
        .unwrap();
    let retention_options = CallOptions {
        limits: Limits {
            steps: Some(3_000_000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    for skip in [false, true] {
        let small = script
            .call(
                "discard",
                &[Value::int(8), Value::boolean(skip)],
                retention_options.clone(),
            )
            .unwrap();
        let large = script
            .call(
                "discard",
                &[Value::int(256), Value::boolean(skip)],
                retention_options.clone(),
            )
            .unwrap();
        assert_eq!(small.stats.peak_memory_bytes, large.stats.peak_memory_bytes);
        assert_eq!(large.stats.retained_memory_bytes, 0);
        assert!(json(&large.value).is_null());
    }
    let small = script
        .call("hold", &[Value::int(16)], CallOptions::default())
        .unwrap();
    let large = script
        .call("hold", &[Value::int(64)], CallOptions::default())
        .unwrap();
    assert_eq!(large.value.as_array().unwrap().len(), 64);
    assert!(large.stats.retained_memory_bytes > 3 * small.stats.retained_memory_bytes);
    let limited = CallOptions {
        limits: Limits {
            memory_bytes: Some(small.stats.peak_memory_bytes),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("hold", &[Value::int(64)], limited)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let fresh = script
        .call("hold", &[Value::int(16)], CallOptions::default())
        .unwrap();
    assert_eq!(counters(small.stats), counters(fresh.stats));
}

#[test]
fn nested_loop_frames_obey_work_memory_and_recursion_limits() {
    let script = Engine::new()
        .compile(
            r#"
def run
 n=0
 loop do
  n+=1
  result=[1,2].map{|v|[n,v]}
  break result if n==3
 end
end
def deep(n)
 return n if n==0
 loop{break deep(n-1)}
end
"#,
        )
        .unwrap();
    let baseline = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&baseline.value), serde_json::json!([[3, 1], [3, 2]]));
    for steps in 0..baseline.stats.steps {
        let options = CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Steps,
            "steps={steps}"
        );
    }
    let peak = baseline.stats.peak_memory_bytes;
    for memory in (0..peak).step_by((peak / 41).max(1)).chain([peak - 1]) {
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Memory,
            "memory={memory}"
        );
    }
    let options = CallOptions {
        limits: Limits {
            recursion: 6,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("deep", &[Value::int(50)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    let fresh = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(counters(baseline.stats), counters(fresh.stats));
    assert_eq!(json(&fresh.value), json(&baseline.value));
}

#[test]
fn loop_cancellation_and_exhaustion_cannot_run_rescue_or_ensure_effects() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for (body, limits, expected) in [
        (
            "loop{next [\"value\"*512]}",
            Limits {
                steps: Some(300),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
        (
            "out=[];loop{out.push(\"value\"*512)}",
            Limits {
                memory_bytes: Some(64 << 10),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            "loop{cancel();mark(1)}",
            Limits::default(),
            ErrorKind::Cancelled,
        ),
        (
            "loop(cancel(),mark(1)){mark(2);break}",
            Limits::default(),
            ErrorKind::Cancelled,
        ),
    ] {
        events.lock().unwrap().clear();
        let source = format!(
            "begin\n{body}\nrescue RuntimeError | LimitError\nmark(3)\nensure\nmark(4)\nend"
        );
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                limits,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, expected, "{source}");
        assert!(events.lock().unwrap().is_empty(), "{source}");
    }
    let fresh = engine
        .compile("loop{break 7}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&fresh.value), serde_json::json!(7));
}

#[test]
fn loop_calls_preserve_host_overrides_and_method_control_boundaries() {
    let mut engine = Engine::new();
    engine.register("loop", |ctx, args| {
        ctx.charge(1)?;
        Ok(Value::int(args[0].as_int().unwrap() + 10))
    });
    let output = engine
        .compile("[loop(4),(loop)(5)]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([14, 15]));
    for control in ["break 7", "next 7"] {
        let script = Engine::new()
            .compile(&format!(
                "def invalid\n{control}\nend\ndef run\nloop{{invalid()}}\nend\ndef good\nloop{{break 9}}\nend"
            ))
            .unwrap();
        let baseline = script.call("good", &[], CallOptions::default()).unwrap();
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
        let fresh = script.call("good", &[], CallOptions::default()).unwrap();
        assert_eq!(json(&fresh.value), serde_json::json!(9));
        assert_eq!(counters(baseline.stats), counters(fresh.stats));
    }
}
