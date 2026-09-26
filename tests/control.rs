mod common;

use std::time::Duration;
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn result_json(source: &str) -> serde_json::Value {
    json_of(&Engine::new(), source)
}

fn json_of(engine: &Engine, source: &str) -> serde_json::Value {
    let result = engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn range_iteration_terminates_at_integer_boundaries() {
    let source = "max=9223372036854775807\nmin=-max-1\na: array<int> =[]\nfor n in max..max\na.push(n)\nend\nfor n in min..min\na.push(n)\nend\nfor n in (max-1)..max\na.push(n)\nend\nfor n in (min+1)..min\na.push(n)\nend\na";
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
            "a=[1,2]\nout: array<int> =[]\nx=for n in a\nout.push(n)\na.push(3)\na[1]=9\nend\n[x,a,out]"
        ),
        serde_json::json!([[1, 2], [1, 9, 3, 3], [1, 2]])
    );
    assert_eq!(
        result_json(
            "h: hash<string, int> ={a:1,b:2}\nout: array<[string, int]> =[]\nx=for k,v in h\nh[\"b\"]=9\nh[\"c\"]=3\nout.push([k,v])\nend\n[x,h,out]"
        ),
        serde_json::json!([{"a":1,"b":2},{"a":1,"b":9,"c":3},[["a",1],["b",2]]])
    );
}

#[test]
fn empty_loops_and_pattern_comparisons_consume_steps() {
    for source in [
        "for n in 0..9223372036854775807\nend",
        "while !false\nend",
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
        .compile("def run(input: array<int>) -> int?\ncase -1\nwhen *input then 1\nend\nend")
        .unwrap();
    let input = Value::array((0..1000).map(Value::int).collect());
    let baseline = Engine::new()
        .compile("def run(input: array<int>) -> array<int>\ninput\nend")
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
#[cfg_attr(target_os = "wasi", ignore = "WASI has no threads")]
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
        .compile("def run(input: array<int>) -> array<int>\ninput\nend")
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
        .compile("def run(input: array<int>) -> array<int>\n*rest=input\nrest\nend")
        .unwrap();
    assert_eq!(
        script
            .call("run", std::slice::from_ref(&input), options.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let discard = Engine::new()
        .compile("def run(input: array<int>) -> int?\nfirst,* = input\nfirst\nend")
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
            "def f(input: string) -> any\n{body}\nend\ndef run(input: string) -> int\ni=0\nwhile i<100\nf(input)\ni+=1\nend\n7\nend"
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

#[test]
fn range_matchers_compare_range_targets_by_equality() {
    for (source, expected) in [
        ("(1..3) === (1..3)", true),
        ("(1...3) === (1...3)", true),
        ("(7...7) === (7...7)", true),
        ("(1..3) === (1...3)", false),
        ("(1..3) === \"a\"", false),
        ("2 === (1..3)", false),
        (
            "case (1..5)\nwhen 2 then false\nwhen 1..5 then true\nelse false\nend",
            true,
        ),
        ("case (1...5)\nwhen 1..5 then false\nelse true\nend", true),
        ("[1..3, 4].any?(1..3) && [(1..3)].count(1..3) == 1", true),
    ] {
        assert_eq!(result_json(source), serde_json::json!(expected), "{source}");
    }
}

#[test]
fn float_range_membership_is_exact_beyond_double_precision() {
    for (source, expected) in [
        (
            "(9007199254740993..9007199254740993).include?(9007199254740992.0)",
            false,
        ),
        (
            "(9007199254740992..9007199254740992).include?(9007199254740993.0)",
            true,
        ),
        ("(9007199254740993..).include?(9007199254740992.0)", false),
        ("(..9007199254740991).include?(9007199254740992.0)", false),
        ("(...9007199254740993) === 9007199254740992.0", true),
        (
            "(9007199254740995...9007199254740992).include?(9007199254740992.0)",
            false,
        ),
        (
            "case 9007199254740992.0\nwhen 9007199254740993..9007199254740993 then true\nelse false\nend",
            false,
        ),
        (
            "case 9223372036854775807.0\nwhen 9223372036854775807.. then true\nelse false\nend",
            true,
        ),
        ("(1..).include?(1.0/0) && !(1..3).include?(1.0/0)", true),
        ("(3...1).include?(1.5) && !(3...1).include?(1.0)", true),
    ] {
        assert_eq!(result_json(source), serde_json::json!(expected), "{source}");
    }
}

#[test]
fn locals_read_where_control_skips_their_assignment_are_refused() {
    // A local must be assigned on every path that reaches a read, so reads
    // that found nil, or failed as undefined, at run time are refused.
    for (source, codes, at) in [
        (
            "def branch -> int?\n  if false\n    x = 1\n  else\n    x\n  end\nend",
            &["V0202"][..],
            "x\n  end",
        ),
        (
            "def negated_else -> int?\n  if !true\n    u = 1\n  else\n    u\n  end\nend",
            &["V0202"],
            "u\n  end",
        ),
        (
            "def elsif_condition -> string?\n  if false\n    e: int? = 1\n    \"no\"\n  \
             elsif e == nil\n    \"ok\"\n  end\nend",
            &["V0202"],
            "e ==",
        ),
        ("y = 1 if y == nil", &["V0201"], "y =="),
        ("z = 1 while z == nil", &["V0201"], "z =="),
        (
            "seen = [1, 2].map { |v| s = v if s == nil; s }",
            &["V0201", "V0202"],
            "s ==",
        ),
        (
            "before = later\nif false\n  later = 1\nend",
            &["V0201"],
            "later",
        ),
        (
            "if true\n  later\nelse\n  later = 1\nend",
            &["V0201"],
            "later",
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{source}"
        );
    }
    // A statement modifier loop still assigns a local it reads.
    assert_eq!(
        result_json("i = 0\ni = i + 1 while i < 3\ni"),
        serde_json::json!(3)
    );
}
