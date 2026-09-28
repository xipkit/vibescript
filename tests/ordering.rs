mod common;

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use vibescript::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Signature, Value,
    stringify_json,
};

/// A host function without parameters whose result has type `result`, so
/// scripts use it without a cast.
fn typed(
    name: &str,
    result: &str,
    function: impl Fn(&mut CallContext) -> vibescript::Result<Value> + Send + Sync + 'static,
) -> HostMethod {
    HostMethod::new(name, move |ctx, _, _| function(ctx))
        .with_signature(Signature {
            params: vec![],
            result: result.into(),
            accepts_block: false,
        })
        .unwrap()
}

/// The code and offset of each static diagnostic that refuses `source`.
fn refused(engine: &Engine, source: &str) -> Vec<(String, usize)> {
    let error = engine
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| (d.code.to_string(), d.span.start))
        .collect()
}

fn json(value: &Value) -> String {
    String::from_utf8(
        stringify_json(value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes()
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn key_sort_is_stable_across_merge_boundaries_and_preserves_inputs() {
    let script = Engine::new()
        .compile("def run(input: array<array<int>>) -> array<array<int>>\ninput.sort_by {|row|row.fetch(0)}\nend")
        .unwrap();
    for len in (0..=130).chain([255, 256, 257, 511, 1024]) {
        let mut rows: Vec<_> = (0..len).map(|i| ((i * 37 + 11) % 17, i)).collect();
        let input = Value::array(
            rows.iter()
                .map(|(key, i)| Value::array(vec![Value::int(*key), Value::int(*i)]))
                .collect(),
        );
        let original = json(&input);
        rows.sort_by_key(|(key, _)| *key);
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap_or_else(|e| panic!("length {len}: {e}"));
        let actual: Vec<_> = result
            .value
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let row = row.as_array().unwrap();
                (row[0].as_int().unwrap(), row[1].as_int().unwrap())
            })
            .collect();
        assert_eq!(actual, rows, "length {len}");
        assert_eq!(json(&input), original, "length {len}");
    }
}

#[test]
fn arrays_are_not_ordered() {
    // Only scalars are ordered, so sorting, comparing or picking extremes
    // by arrays is refused before running.
    let engine = vibescript::Engine::new();
    let arrays = "a: array<any> = [0];b: array<any> = [0];";
    for (source, code, text) in [
        (
            "def run(input: array<array<int>>) -> array<array<int>>\ninput.sort\nend".to_owned(),
            "V0115",
            "sort",
        ),
        (
            "def run(input: array<array<int>>) -> array<array<int>>\ninput.sort_by {|row|row}\nend"
                .to_owned(),
            "V0115",
            "sort_by",
        ),
        (
            "(0..2000).to_a.min_by {|n|a=[n%31];[a,a]}".to_owned(),
            "V0115",
            "min_by",
        ),
        (
            "(0..2000).to_a.max_by {|n|a=[n%31];[a,a]}".to_owned(),
            "V0115",
            "max_by",
        ),
        (format!("{arrays}a<=>b"), "V0108", "<=>"),
        (format!("{arrays}[a,b].sort.length-2"), "V0115", "sort"),
        (format!("{arrays}[a,b].minmax.length-2"), "V0115", "minmax"),
        (
            format!("{arrays}[a,b].sort_by {{|x|x}}.length-2"),
            "V0115",
            "sort_by",
        ),
    ] {
        assert_eq!(
            refused(&engine, &source),
            [(code.to_owned(), source.find(text).unwrap())],
            "{source}"
        );
    }
}

#[test]
fn nan_ordering_preserves_ieee_comparisons() {
    let script = Engine::new()
        .compile(
            "def order(a: float, b: float) -> int; a <=> b; end\n\
         def ieee(a: float, b: float) -> array<bool>; [a==b,a<b,a<=b,a>b,a>=b]; end",
        )
        .unwrap();
    let values = [
        f64::NAN,
        f64::from_bits(0xfff8_0000_0000_0001),
        f64::NEG_INFINITY,
        -1.0,
        -0.0,
        0.0,
        1.0,
        f64::INFINITY,
    ];
    for a in values {
        for b in values {
            let args = [Value::float(a), Value::float(b)];
            let expected = if a.is_nan() {
                if b.is_nan() { 0 } else { -1 }
            } else if b.is_nan() || a > b {
                1
            } else if a < b {
                -1
            } else {
                0
            };
            let value = script
                .call("order", &args, CallOptions::default())
                .unwrap()
                .value;
            assert_eq!(value.as_int(), Some(expected), "{a:?} <=> {b:?}");
            let value = script
                .call("ieee", &args, CallOptions::default())
                .unwrap()
                .value;
            assert_eq!(
                json(&value),
                serde_json::to_string(&[a == b, a < b, a <= b, a > b, a >= b]).unwrap()
            );
        }
    }
    for source in ["(0.0/0.0) <=> 1", "(0.0/0.0) <=> 9223372036854775808"] {
        assert_eq!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(-1)
        );
    }
    // Arrays are not ordered, and a comparator returns an int.
    let mut engine = vibescript::Engine::new();
    engine.register_method("nan", typed("nan", "float", |_| Ok(Value::float(f64::NAN))));
    let source = "n=nan();a=[n];[[n]<=>[n],a<=>a,[3,1,2].sort {n}]";
    let comparisons: Vec<(String, usize)> = source
        .match_indices("<=>")
        .map(|(offset, _)| ("V0108".to_owned(), offset))
        .chain([("V0101".to_owned(), source.find("n}").unwrap())])
        .collect();
    assert_eq!(refused(&engine, source), comparisons);
}

#[test]
fn sort_keys_are_retained_and_extrema_discard_unselected_keys() {
    for method in ["sort_by", "min_by", "max_by"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register_method(
            "allocate",
            typed("allocate", "string", move |ctx| {
                seen.fetch_add(1, Ordering::SeqCst);
                ctx.bytes(&[b'x'; 8192])
            }),
        );
        let result = engine
            .compile(&format!("(1..100).to_a.{method} {{allocate()}}"))
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(96_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
        if method == "sort_by" {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
            assert!(calls.load(Ordering::SeqCst) < 20);
        } else {
            let result = result.unwrap();
            assert_eq!(result.value.as_int(), Some(1));
            assert_eq!(calls.load(Ordering::SeqCst), 100);
            assert_eq!(result.stats.retained_memory_bytes, 0);
        }
    }
}

#[test]
fn long_comparisons_preserve_step_exhaustion_before_later_host_effects() {
    let checkpoint = Arc::new(AtomicU64::new(0));
    let observed = checkpoint.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("arm", move |ctx, _| {
        observed.store(ctx.stats().steps, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "a<=>b",
        "[a,b].sort",
        "[a,b].min",
        "[a,b].max",
        "[a,b].minmax",
        "[a,b].sort_by {|x|x}",
        "[a,b].min_by {|x|x}",
        "[a,b].max_by {|x|x}",
    ] {
        let script = engine
            .compile(&format!(
                "s=\"x\"*200000;a=s+\"a\";b=s+\"b\";arm();{expression};effect()"
            ))
            .unwrap();
        script.run(CallOptions::default()).unwrap();
        let limit = checkpoint.load(Ordering::SeqCst) + 200;
        effects.store(0, Ordering::SeqCst);
        let error = script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(limit),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn sorting_exits_release_keys_scratch_and_pending_receivers() {
    let mut engine = Engine::new();
    engine.register_method(
        "allocate",
        typed("allocate", "string", |ctx| ctx.bytes(&[b'x'; 8192])),
    );
    for body in [
        "[3,1,2].sort {allocate();return 7}",
        "[3,1,2].sort {allocate();break 7}",
        "a: array<array<int> | int> = [];a.push([3,1,2].sort {allocate();break 7});7",
        "[3,1,2].sort_by {|v|return 7 if v==2;allocate()}",
        "[3,1,2].sort_by {|v|break 7 if v==2;allocate()}",
        "[3,1,2].min_by {|v|return 7 if v==2;allocate()}",
        "[3,1,2].max_by {|v|break 7 if v==2;allocate()}",
        "[3,1,2].sort_by {next allocate()};7",
    ] {
        let result = engine
            .compile(&format!(
                "def work -> any\n{body}\nend\ndef run -> int\n200.times {{work}}\n7\nend"
            ))
            .unwrap()
            .call(
                "run",
                &[],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(96_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
    }
}

#[test]
fn cancellation_from_comparator_and_key_blocks_prevents_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "cancel",
        typed("cancel", "int", |ctx| {
            ctx.cancellation().cancel();
            Ok(Value::int(0))
        }),
    );
    engine.register_method(
        "effect",
        typed("effect", "int", move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Value::int(0))
        }),
    );
    for method in ["sort", "sort_by", "min_by", "max_by"] {
        for body in ["cancel();effect()", "cancel()"] {
            let error = engine
                .compile(&format!("[3,1,2].{method} {{{body}}};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled, "{method}: {body}");
            assert_eq!(effects.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn float_selection_uses_nan_first_and_keeps_equal_values_stable() {
    let source = "def sort(values: array<float>) -> array<float>; values.sort; end
        def sort_by(values: array<float>) -> array<float>; values.sort_by { |n| n }; end
        def compare(values: array<float>) -> array<float>; values.sort { |a,b| a <=> b }; end
        def select(values: array<float>) -> array<float?>
          [values.min,values.max,values.minmax[0],values.minmax[1],
           values.min_by { |n| n },values.max_by { |n| n }]
        end";
    let script = Engine::new().compile(source).unwrap();
    let nan1 = f64::from_bits(0x7ff8_0000_0000_0001);
    let nan2 = f64::from_bits(0xfff8_0000_0000_0002);
    let input = [
        1.0,
        nan1,
        0.0,
        -0.0,
        f64::INFINITY,
        nan2,
        f64::NEG_INFINITY,
        -2.0,
    ];
    let expected = [
        nan1,
        nan2,
        f64::NEG_INFINITY,
        -2.0,
        0.0,
        -0.0,
        1.0,
        f64::INFINITY,
    ];
    let args = [Value::array(input.map(Value::float).to_vec())];
    for name in ["sort", "sort_by", "compare"] {
        let result = script
            .call(name, &args, CallOptions::default())
            .unwrap()
            .value;
        let bits: Vec<_> = result
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_float().unwrap().to_bits())
            .collect();
        assert_eq!(bits, expected.map(f64::to_bits), "{name}");
    }
    let result = script
        .call("select", &args, CallOptions::default())
        .unwrap()
        .value;
    let bits: Vec<_> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_float().unwrap().to_bits())
        .collect();
    assert_eq!(
        bits,
        [
            nan1,
            f64::INFINITY,
            nan1,
            f64::INFINITY,
            nan1,
            f64::INFINITY
        ]
        .map(f64::to_bits)
    );
    let result = script
        .call("select", &[Value::array(vec![])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(json(&result), "[null,null,null,null,null,null]");
    for (source, expected) in [
        ("(0.0/0.0).clamp(0.0,1.0)", 0.0),
        ("0.5.clamp(0.0/0.0,1.0)", 0.5),
        ("(0.0/0.0).between?(0.0,1.0)", 0.0),
    ] {
        let value = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        if source.contains("between?") {
            assert_eq!(json(&value), "false");
        } else {
            assert_eq!(value.as_float(), Some(expected));
        }
    }
}
