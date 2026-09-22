use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn set_operations_preserve_first_occurrences_and_difference_duplicates() {
    assert_eq!(
        evaluate(
            r#"a=[3,1,3,2];
            [a & [2,3,3], a - [3], a.union([2,4],[4,5]),
             a.difference([3],[2]), a.difference, a.union,
             [[1,2,3],[2,3],[3]].reduce(:&), [[1,2,3],[2],[3]].reduce(:-)]"#,
        ),
        serde_json::json!([
            [3, 2],
            [1, 2],
            [3, 1, 2, 4, 5],
            [1],
            [3, 1, 3, 2],
            [3, 1, 2],
            [3],
            [1]
        ])
    );
    assert_eq!(
        evaluate(
            "[[1,1,2]-[2], [1,1,2].difference([2]), [[1],[1.0]].union, [{a:1},{a:1.0}].union]"
        ),
        serde_json::json!([[1, 1], [1, 1], [[1]], [{"a":1}]])
    );
}

#[test]
fn scalar_keys_keep_kinds_distinct_and_collapse_nans_including_block_keys() {
    let result = Engine::new()
        .compile("n=0.0/0.0;a=[1,1.0,n,n,0.0,-0.0];[a.uniq,a.uniq{|x|x},a.union]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    for output in result.value.as_array().unwrap() {
        let values = output.as_array().unwrap();
        assert_eq!(values.len(), 4);
        assert_eq!(values[0].type_name(), "int");
        assert_eq!(values[1].type_name(), "float");
        assert!(values[2].as_float().unwrap().is_nan());
        assert_eq!(values[3].as_float(), Some(0.0));
    }
    assert_eq!(
        evaluate(
            "n=0.0/0.0;[([n,n]&[n]).length,([n,n]-[n]).length,[[n],[n]].uniq.length,[[n],[n]].uniq{|x|x}.length,([1,1.0]-[1]).length,([1,1.0]&[1]).length]"
        ),
        serde_json::json!([1, 0, 2, 2, 1, 1])
    );
}

#[test]
fn operators_and_methods_preserve_evaluated_values_and_nested_aliases() {
    for (source, expected) in [
        (
            "a=[1,2,1];old=a;r=a-a.shift(1);[a,old,r]",
            serde_json::json!([[2, 1], [1, 2, 1], [2]]),
        ),
        (
            "a=[1,2];old=a;r=a & a.push(3);r.push(4);[a,old,r]",
            serde_json::json!([[1, 2, 3], [1, 2], [1, 2, 4]]),
        ),
        (
            "a=[[1],[2]];r=a.union([[1.0],[3]]);r[0].push(9);[a,r]",
            serde_json::json!([[[1], [2]], [[1, 9], [2], [3]]]),
        ),
        (
            "a=[1,2];old=a;a-=[2];[a,old]",
            serde_json::json!([[1], [1, 2]]),
        ),
        (
            "a=[1];old=a;r=a.union(a.push(2));[a,old,r]",
            serde_json::json!([[1, 2], [1], [1, 2]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn intersection_obeys_precedence_line_continuation_and_command_spacing() {
    assert_eq!(
        evaluate(
            "a=[1];b=[1,2];[a &b, a & b + [3], [1,2]&[2] == [2], [1,2]&\n[2], ([1,2]&[2] << 3)]"
        ),
        serde_json::json!([[1], [1], true, [2], [2]])
    );
    assert_eq!(
        evaluate("def values\n[1,2]\nend\n[values&[2],values & [2],values &\n[2]]"),
        serde_json::json!([[2], [2], [2]])
    );
    for source in [
        "values &[2]",
        "values(&[2])",
        "values 1, &[2]",
        "a=[];a&=[]",
        "[1] & ; [1]",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().unwrap().kind,
            ErrorKind::Syntax
        );
    }
}

#[test]
fn ignored_blocks_and_argument_failures_preserve_host_effect_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    for method in ["union", "difference"] {
        engine
            .compile(&format!("[1].{method}([2]){{mark()}}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    for method in ["union", "difference"] {
        for args in ["[],bad:mark()", "[],mark()"] {
            calls.store(0, Ordering::Relaxed);
            let error = engine
                .compile(&format!("[].{method}({args});mark()"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert!(matches!(error.kind, ErrorKind::Argument | ErrorKind::Type));
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }
    }
}

#[test]
fn repeated_results_release_storage_and_exhausted_work_stops_before_host_effects() {
    for operation in ["a.union([128])", "a.difference([0])", "a & a", "a - [-1]"] {
        let script = Engine::new()
            .compile(&format!(
                "a=(0..127).to_a;i=0;while i<64;r={operation};i+=1;end;r.length"
            ))
            .unwrap();
        let result = script
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(32768),
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        assert!(result.stats.peak_memory_bytes < 32768);
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    // Building the input costs about 4,100 steps and each operation several
    // steps per element, so the quota runs out inside the operation.
    for operation in ["a.union", "a.uniq{|x|x}", "a & a", "a - a"] {
        let script = engine
            .compile(&format!("a=(0..2047).to_a;{operation};mark()"))
            .unwrap();
        assert_eq!(
            script
                .run(CallOptions {
                    limits: Limits {
                        steps: Some(8192),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::Steps,
            "{operation}"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}

fn steps(source: &str, limits: Limits) -> Result<u64, ErrorKind> {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions {
            limits,
            ..CallOptions::default()
        })
        .map(|outcome| outcome.stats.steps)
        .map_err(|error| error.kind)
}

#[test]
fn set_operations_scale_linearly_and_fit_default_quotas() {
    let operations = [
        "a.uniq.size",
        "a.uniq { |x| x }.size",
        "(a - a.reverse).size",
        "(a & a).size",
        "a.union(a).size",
        "a.difference(a.reverse).size",
        "a.map { |x| x.to_s }.uniq.size",
        "a.map { |x| [x % 97, x.to_s] }.uniq.size",
    ];
    for operation in operations {
        let unlimited = Limits {
            steps: None,
            ..Limits::default()
        };
        let cost = |n: u64| {
            let base = steps(&format!("a = (1..{n}).to_a\na.size"), unlimited.clone()).unwrap();
            steps(
                &format!("a = (1..{n}).to_a\n{operation}"),
                unlimited.clone(),
            )
            .unwrap()
                - base
        };
        let (small, large) = (cost(4000), cost(8000));
        assert!(large < small * 9 / 4, "{operation}: {small} then {large}");
        // The reference runs these 20,000-element cases within its default quota.
        let source = format!("a = (1..20000).to_a\n{operation}");
        assert!(
            steps(&source, Limits::default()).is_ok(),
            "{operation} exceeds the default quota"
        );
    }
    assert!(steps("(1..1500).to_a.uniq.size", Limits::default()).is_ok());
}

#[test]
fn set_operation_quotas_are_exact() {
    for operation in [
        "a.uniq",
        "a & b",
        "a - b",
        "a.union(b)",
        "a.uniq { |x| x % 7 }",
    ] {
        let source = format!("a = (1..300).to_a\nb = (150..450).to_a\n({operation}).size");
        let exact = steps(
            &source,
            Limits {
                steps: None,
                ..Limits::default()
            },
        )
        .unwrap();
        let limited = |steps| Limits {
            steps: Some(steps),
            ..Limits::default()
        };
        assert_eq!(steps(&source, limited(exact)), Ok(exact), "{operation}");
        assert_eq!(
            steps(&source, limited(exact - 1)),
            Err(ErrorKind::Steps),
            "{operation}"
        );
    }
}
