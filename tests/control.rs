use std::time::Duration;
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn result_json(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn range_iteration_terminates_at_integer_boundaries() {
    let source = "max=9223372036854775807\nmin=-max-1\na=[]\nfor n in max..max\na.push(n)\nend\nfor n in min..min\na.push(n)\nend\nfor n in (max-1)..max\na.push(n)\nend\nfor n in (min+1)..min\na.push(n)\nend\na";
    assert_eq!(
        result_json(source),
        serde_json::json!([
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MAX,
            i64::MIN + 1,
            i64::MIN
        ])
    );
}

#[test]
fn hash_loop_expressions_preserve_break_values() {
    assert_eq!(
        result_json("a=for k,v in {a:1}\nbreak 7\nend\nb=for pair in {a:1}\nbreak\nend\n[a,b]"),
        serde_json::json!([7, null])
    );
}

#[test]
fn loops_hold_collection_snapshots() {
    assert_eq!(
        result_json(
            "a=[1,2]\nout=[]\nx=for n in a\nout.push(n)\na.push(3)\na[1]=9\nend\n[x,a,out]"
        ),
        serde_json::json!([[1, 2], [1, 9, 3, 3], [1, 2]])
    );
    assert_eq!(
        result_json(
            "h={a:1,b:2}\nout=[]\nx=for k,v in h\nh[:b]=9\nh[:c]=3\nout.push([k,v])\nend\n[x,h,out]"
        ),
        serde_json::json!([{"a":1,"b":2},{"a":1,"b":9,"c":3},[["a",1],["b",2]]])
    );
}

#[test]
fn empty_loops_and_pattern_comparisons_consume_steps() {
    for source in [
        "for n in 0..9223372036854775807\nend",
        "until false\nend",
        "case -1\nwhen *(1..10000).to_a then 1\nend",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions {
            limits: Limits {
                steps: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.run(options).unwrap_err().kind,
            ErrorKind::Steps,
            "{source}"
        );
    }
    let script = Engine::new()
        .compile("def run(input)\ncase -1\nwhen *input then 1\nend\nend")
        .unwrap();
    let input = Value::array((0..1000).map(Value::int).collect());
    let baseline = Engine::new()
        .compile("def run(input)\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(baseline.stats.steps + 100),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script.call("run", &[input], options).unwrap_err().kind,
        ErrorKind::Steps
    );
}

#[test]
fn running_for_loop_observes_cancellation() {
    let token = CancellationToken::new();
    let signal = token.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let mut engine = Engine::new();
    engine.register("started", move |_, _| {
        started.send(()).unwrap();
        Ok(Value::nil())
    });
    let script = engine
        .compile("started()\nfor n in 0..9223372036854775807\nend")
        .unwrap();
    let handle = std::thread::spawn(move || {
        script.run(CallOptions {
            cancellation: token,
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        })
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    signal.cancel();
    assert_eq!(
        handle.join().unwrap().unwrap_err().kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn destructuring_rest_storage_is_reserved_before_copying() {
    let input = Value::array((0..1000).map(Value::int).collect());
    let baseline = Engine::new()
        .compile("def run(input)\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(baseline.stats.peak_memory_bytes + 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("def run(input)\n*rest=input\nrest\nend")
        .unwrap();
    assert_eq!(
        script
            .call("run", std::slice::from_ref(&input), options.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let discard = Engine::new()
        .compile("def run(input)\nfirst,* = input\nfirst\nend")
        .unwrap();
    assert_eq!(
        discard
            .call("run", &[input], options)
            .unwrap()
            .value
            .as_int(),
        Some(0)
    );
}

#[test]
fn loop_unwinding_releases_sources_payloads_and_frames() {
    for body in [
        "for n in [input]\nend\n7",
        "for n in [input]\nbreak 7\nend",
        "for n in [input]\nreturn 7\nend",
        "for n in [input]\nnext [input,input]\nend\n7",
        "case input\nwhen (for n in [input]\nreturn 7\nend) then 9\nend",
    ] {
        let source = format!(
            "def f(input)\n{body}\nend\ndef run(input)\ni=0\nwhile i<100\nf(input)\ni+=1\nend\n7\nend"
        );
        let script = Engine::new().compile(&source).unwrap();
        let result = script
            .call(
                "run",
                &[Value::bytes(vec![b'x'; 16384])],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(50_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.value.as_int(), Some(7), "{body}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
    }
    let source = "i=0\nwhile i<100\ni+=1\nnext (\"x\"*8192)\nend\n7";
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(20_000),
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn malformed_control_syntax_is_rejected() {
    for source in [
        "a,*b,*c=[1,2]",
        "a,b+=1",
        "for a[0] in [1]\nend",
        "for 1 in [1]\nend",
        "unless false\n1\nelsif true\n2\nend",
        "x=if true\ny=1\nend",
        "case 1\nelse 2\nend",
        "1 if true while false",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().unwrap().kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
}
