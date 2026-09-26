mod common;

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
        ("loop { break(:done) }", serde_json::json!("done")),
        (
            "loop {break [it,_1,_2]}",
            serde_json::json!([null, null, null]),
        ),
        (
            "n=0;out: array<int> =[];loop {\nn+=1\nbreak out if n>5\nnext [99] if n%2==0\nout.push(n)\n}",
            serde_json::json!([1, 3, 5]),
        ),
        (
            "n=0;out: array<any> =[];loop {\nn+=1\nout.push(loop{break [n]})\nbreak out if n==3\n}",
            serde_json::json!([[1], [2], [3]]),
        ),
        (
            "out: array<int> =[];value=loop {\nbegin\nbreak 7\nensure\nout.push(9)\nend\n}\n[value,out]",
            serde_json::json!([7, [9]]),
        ),
        (
            "n=0;loop {\nbegin\nn+=1\nraise \"retry\" if n<3\nbreak n\nrescue RuntimeError\nretry\nend\n}",
            serde_json::json!(3),
        ),
        (
            "def escape -> int\nloop{return 7}\n99\nend\nescape",
            serde_json::json!(7),
        ),
        (
            "def with_block(&block: int -> int) -> int\nloop{break yield(4)}.as(int)\nend\nwith_block{|v|v+1}",
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
    // A loop passes its block no values, so declared block parameters are
    // refused, and so is a rescue fallback that names a missing callee.
    for (source, codes, at) in [
        ("loop {|a,b|break [a,b]}", &["V0306", "V0306"][..], "a"),
        ("loop {|(a,*rest)|break [a,rest]}", &["V0306"], "a"),
        ("loop {|v:int?|break v}", &["V0306"], "v"),
        (
            "(missing rescue loop)(){break 9}",
            &["V0201", "V0304"],
            "missing",
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{source}");
        assert_eq!(error.diagnostics()[0].span.start, source.find(at).unwrap());
    }
}

#[test]
fn invalid_loop_calls_are_refused_before_their_arguments_run() {
    let mut engine = vibescript::Engine::new();
    engine.register("mark", |_, _| panic!("mark ran"));
    for (source, expected) in [
        ("loop()", &[("V0304", 0)][..]),
        (
            "loop(mark(1),flag:mark(2)){mark(3);break}",
            &[("V0301", 0), ("V0302", 13)],
        ),
        ("loop(flag:mark(1)){mark(2);break}", &[("V0302", 5)]),
        ("loop{|v:int|mark(1);break v}", &[("V0306", 6)]),
        (
            "loop.call(mark(1)){mark(2);break}",
            &[("V0304", 0), ("V0106", 5)],
        ),
    ] {
        let error = engine.compile(source).err().unwrap();
        let found: Vec<(String, usize)> = error
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code.to_string(), diagnostic.span.start))
            .collect();
        let expected: Vec<(String, usize)> = expected
            .iter()
            .map(|(code, at)| (code.to_string(), *at))
            .collect();
        assert_eq!(found, expected, "{source}");
    }
}

#[test]
fn loop_discards_normal_and_next_results_without_retaining_storage() {
    let script = Engine::new()
        .compile(
            r#"
def discard(n: int,skip: bool)
 i=0
 loop {
  i+=1
  break nil if i>n
  next ["x"*4096] if skip
  ["x"*4096]
 }
end
def hold(n: int) -> any
 i=0
 out: array<array<string>> =[]
 loop {
  i+=1
  out.push(["x"*512])
  break out if i==n
 }
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
def run -> any
 n=0
 loop {
  n+=1
  result=[1,2].map{|v|[n,v]}
  break result if n==3
 }
end
def deep(n: int) -> any
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
            "out: array<string> =[];loop{out.push(\"value\"*512)}",
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
    // Arguments to loop are refused before anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("cancel", |_, _| panic!("cancel ran"));
    checked.register("mark", |_, _| panic!("mark ran"));
    let error = checked
        .compile("loop(cancel(),mark(1)){mark(2);break}")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0301"]);
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
    // A method body is not inside the caller's loop, so a loop transfer
    // there is refused before anything runs.
    for control in ["break 7", "next 7"] {
        let source =
            format!("def invalid -> any\n{control}\nend\ndef run -> any\nloop{{invalid}}\nend");
        let error = Engine::new().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0001"], "{control}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(control).unwrap(),
            "{control}"
        );
    }
}
