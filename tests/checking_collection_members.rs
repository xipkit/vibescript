use std::{fs, path::Path};
use vibescript::{
    CallOptions, CheckReport, CheckedOutcome, Engine, ErrorClass, Limits, Script, Value,
};

fn compile(source: &str) -> Script {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn check(script: &Script, source: &str) -> CheckReport {
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.starts_with("Return value:"))
}

fn ints(values: &[i64]) -> Value {
    Value::array(values.iter().copied().map(Value::int).collect())
}

/// Each expression is clean as an `int` result and evaluates to `expected`.
fn exact_ints(cases: &[(&str, i64)]) {
    for &(expression, expected) in cases {
        let source = format!("def run -> int; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}

/// Each expression is known to be nil, so a string result is a contradiction
/// that the runtime's return check also reports.
fn exact_nils(expressions: &[&str]) {
    for expression in expressions {
        let source = format!("def run -> string; {expression}; end");
        let script = compile(&source);
        assert!(returns_bad_type(&check(&script, &source)), "{source}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert!(error.message.ends_with("got nil"), "{source}: {error}");
    }
}

/// Each expression is a known contradiction that also fails at runtime.
fn invalid(expressions: &[&str]) {
    for expression in expressions {
        let source = format!("def run; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(
            report.diagnostics.iter().any(|d| {
                d.message.contains("does not accept") || d.message.starts_with("Operator")
            }),
            "{source}: {report:?}"
        );
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

/// Each declaration checks cleanly, its exact call with `args` is clean and
/// execution returns `expected`.
fn witnesses(cases: Vec<(&str, Vec<Value>, Value)>) {
    for (source, args, expected) in cases {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let report = script
            .check_call("run", &args, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &args, CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), expected.to_string(), "{source}");
    }
}

/// `failing` declarations keep their rescue path reachable; `safe` ones do not.
fn rescues(failing: &[&str], safe: &[&str]) {
    for source in failing {
        let script = compile(source);
        assert!(returns_bad_type(&check(&script, source)), "{source}");
    }
    for source in safe {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    }
}

/// A finite invalid alternative stays a contradiction even when the supplied
/// alternative succeeds at runtime.
fn strict_arms(cases: Vec<(&str, Value)>) {
    for (source, arg) in cases {
        let script = compile(source);
        let report = check(&script, source);
        assert!(!report.diagnostics.is_empty(), "{source}");
        assert!(
            script.call("run", &[arg], CallOptions::default()).is_ok(),
            "{source}"
        );
    }
}

#[test]
fn array_set_operators_follow_membership_and_keep_order() {
    exact_ints(&[
        ("([1, 2, 3, 2] - [2]).length", 2),
        ("([1, 2, 3, 2] - [2]).last", 3),
        ("([1, 2.0, 2] - [2]).length", 2),
        ("([1, 2, 2, 3] & [3, 2]).first", 2),
        ("([1, 2, 2, 3] & [3, 2]).length", 2),
        ("([1, 2] & []).length", 0),
    ]);
    invalid(&["[1, 2] - 1", "[1] & nil", "[1] - 'x'"]);
    witnesses(vec![(
        "def run(xs: array<int>, ys: array<int>) -> array<int>; xs - ys; end",
        vec![ints(&[1, 2, 3]), ints(&[2])],
        ints(&[1, 3]),
    )]);
    strict_arms(vec![(
        "def run(ys: array<int> | int) -> array<int>; [1] - ys; end",
        ints(&[1]),
    )]);
}

#[test]
fn join_inspect_and_flatten_walk_nested_values() {
    exact_ints(&[
        ("[[1, [2]], 3].flatten(1).length", 3),
        ("[[1, [2]], 3].flatten.last", 3),
        ("[[1, [2]], 3].flatten(0)[1]", 3),
        ("[[1, [2]], 3].flatten(-1).length", 3),
        ("[1, [2, 3]].join(\",\").length", 5),
        ("[1, \"x\", nil].inspect.length", 13),
        ("{a: [1]}.inspect.length", 8),
    ]);
    invalid(&[
        "[1].join(1)",
        "[1].join(nil)",
        "[1].join(\",\", \"x\")",
        "[1].flatten(\"x\")",
        "[1].flatten(1, 2)",
        "[1].inspect(1)",
    ]);
    witnesses(vec![
        (
            "def run(xs: array<array<int>>) -> array<int>; xs.flatten; end",
            vec![Value::array(vec![ints(&[1]), ints(&[2, 3])])],
            ints(&[1, 2, 3]),
        ),
        (
            "def run(xs: array<string>, sep: string) -> string; xs.join(sep); end",
            vec![
                Value::array(vec![Value::bytes("a"), Value::bytes("b")]),
                Value::bytes("-"),
            ],
            Value::bytes("a-b"),
        ),
    ]);
    rescues(
        &[
            "def run(sep) -> int; begin; [1].join(sep); 0; rescue; 'bad'; end; end",
            "def run(n) -> int; begin; [[1]].flatten(n); 0; rescue; 'bad'; end; end",
        ],
        &["def run -> int; begin; ['a', 1, [nil]].join(','); 0; rescue; 'bad'; end; end"],
    );
    strict_arms(vec![(
        "def run(sep: string | int) -> string; [1].join(sep); end",
        Value::bytes(","),
    )]);
}

#[test]
fn zip_transpose_and_window_regroup_by_position() {
    exact_ints(&[
        ("[[1, 2], [3, 4]].transpose[1][0]", 2),
        ("[[1, 2], [3, 4]].transpose.length", 2),
        ("[].transpose.length", 0),
        ("[1, 2].zip([3])[0][1]", 3),
        ("[1, 2].zip([3], [5, 6])[1][2]", 6),
        ("[1, 2, 3, 4].window(3).length", 2),
        ("[1, 2, 3, 4].window(3)[1][2]", 4),
        ("[1, 2].window(5).length", 0),
    ]);
    exact_nils(&["[1, 2].zip([3])[1][1]"]);
    invalid(&[
        "[1].zip(1)",
        "[1].zip([1], nil)",
        "[[1], [2, 3]].transpose",
        "[1].transpose",
        "[[1]].transpose(1)",
        "[1].window(0)",
        "[1].window(1.5)",
    ]);
    witnesses(vec![
        (
            "def run(rows: array<array<int>>) -> array<array<int>>; rows.transpose; end",
            vec![Value::array(vec![ints(&[1, 2]), ints(&[3, 4])])],
            Value::array(vec![ints(&[1, 3]), ints(&[2, 4])]),
        ),
        (
            "def run(xs: array<int>, n: int) -> array<array<int>>; xs.window(n); end",
            vec![ints(&[1, 2, 3]), Value::int(2)],
            Value::array(vec![ints(&[1, 2]), ints(&[2, 3])]),
        ),
        (
            "def run(xs: array<int>) -> array<array<int | nil>>; xs.zip([7]); end",
            vec![ints(&[1, 2])],
            Value::array(vec![
                ints(&[1, 7]),
                Value::array(vec![Value::int(2), Value::nil()]),
            ]),
        ),
    ]);
    rescues(
        &[
            "def run(rows: array<array<int>>) -> int; begin; rows.transpose; 0; rescue; 'bad'; end; end",
            // Rows around gradual elements may exceed the value depth limit.
            "def run(xs: array<any>) -> int; begin; xs.zip([1]); 0; rescue LimitError; 'bad'; end; end",
        ],
        &[
            "def run -> int; begin; [[1, 2], [3, 4]].transpose; 0; rescue; 'bad'; end; end",
            "def run -> int; begin; [1, 2].zip([3]); 0; rescue; 'bad'; end; end",
            "def run(xs: array<int>) -> int; begin; xs.zip([1]); 0; rescue LimitError; 'bad'; end; end",
        ],
    );
    strict_arms(vec![(
        "def run(xs: array<int> | int) -> array<array<int>>; [1].zip(xs); end",
        ints(&[2]),
    )]);
}

#[test]
fn values_at_hash_projections_and_ranges_keep_selected_values() {
    exact_ints(&[
        ("[1, 2, 3].values_at(0, -1)[1]", 3),
        ("[1, 2, 3].values_at(1..2, 0).last", 1),
        ("[1, 2, 3].values_at(2..5).length", 4),
        ("{a: 1}.values_at(:a, 'a')[1]", 1),
        ("(1..4).to_a.last", 4),
        ("(3..1).to_a.first", 3),
        ("(1...1).to_a.length", 0),
        ("{a: 1, b: nil}.compact[:a]", 1),
        ("{a: 1, b: 2}.slice(:b, :z)[:b]", 2),
        ("{a: 1, b: 2}.except(:a)[:b]", 2),
    ]);
    exact_nils(&[
        "[1].values_at(4)[0]",
        "[1, 2].values_at(1..3)[1]",
        "{a: nil, b: 1}.compact[:a]",
        "{a: 1}.slice(:z)[:z]",
        "{a: 1}.except(:a)[:a]",
    ]);
    invalid(&[
        "[1].values_at(\"x\")",
        "[1].values_at(-5..0)",
        "{a: 1}.values_at(0)",
        "{a: 1}.values_at(0..1)",
        "{a: 1}.slice(1)",
        "{a: 1}.except(nil)",
        "{a: 1}.compact(1)",
        "(1..).to_a",
        "(1..2).to_a(1)",
    ]);
    witnesses(vec![
        (
            "def run(x: int | nil) -> hash<string, int>; {a: x, b: 1}.compact; end",
            vec![Value::nil()],
            Value::hash(vec![(b"b".to_vec(), Value::int(1))]),
        ),
        (
            "def run(record: hash) -> hash; record.except(:secret).slice(:id); end",
            vec![Value::hash(vec![
                (b"id".to_vec(), Value::int(1)),
                (b"secret".to_vec(), Value::bytes("x")),
            ])],
            Value::hash(vec![(b"id".to_vec(), Value::int(1))]),
        ),
        (
            "def run(n: int) -> array<int>; (1..n).to_a; end",
            vec![Value::int(3)],
            ints(&[1, 2, 3]),
        ),
    ]);
    rescues(
        &[
            "def run(key) -> int; begin; {a: 1}.slice(key); 0; rescue; 'bad'; end; end",
            "def run(r: range) -> int; begin; r.to_a; 0; rescue; 'bad'; end; end",
        ],
        &[
            "def run -> int; begin; {a: 1}.except(:a); 0; rescue; 'bad'; end; end",
            "def run -> int; begin; (1..3).to_a; 0; rescue; 'bad'; end; end",
        ],
    );
    strict_arms(vec![(
        "def run(key: symbol | int) -> hash; {a: 1}.slice(key); end",
        Value::symbol("a"),
    )]);
}

#[test]
fn keyed_lookups_remapping_and_set_members_follow_the_documented_examples() {
    exact_ints(&[
        ("{ a: [10, 20] }.dig(:a, 1)", 20),
        ("[[1, [2, 3]]].dig(0, 1, 1)", 3),
        ("[1, 2].union([2, 3], [3, 4]).length", 4),
        ("[1, 2].union([2, 3], [3, 4]).last", 4),
        ("[1, 2, 3, 2].difference([2], [3]).length", 1),
        (
            "{ first_name: 7 }.remap_keys({ first_name: :name })[:name]",
            7,
        ),
        ("\"h\u{e9}llo\".byteslice(1, 2).length", 1),
        ("[2, 3].to_s.length", 6),
    ]);
    exact_nils(&[
        "{ a: 1 }.dig(:b)",
        "[1].dig(-1)",
        "{ a: 1 }.dig(:a, :b)",
        "\"abc\".byteslice(5)",
    ]);
    invalid(&[
        "[1].dig",
        "[1].dig(\"x\")",
        "[1].dig(0.5)",
        "{ a: 1 }.dig(1)",
        "[1].union(1)",
        "[1].difference([1], nil)",
        "[1].to_s(1)",
        "{ a: 1 }.value?",
        "{ a: 1 }.remap_keys(1)",
        "{ a: 1 }.flatten(nil)",
        "{ a: 1 }.flatten(1, 2)",
        "\"abc\".byteslice(0..1, 1)",
        "\"abc\".byteslice(\"a\")",
    ]);
    witnesses(vec![
        (
            "def run(h: hash) -> bool; h.value?(1); end",
            vec![Value::hash(vec![(b"a".to_vec(), Value::int(1))])],
            Value::boolean(true),
        ),
        (
            "def run(xs: array<int>, ys: array<int>) -> array<int>; xs.union(ys); end",
            vec![ints(&[1, 2]), ints(&[2, 3])],
            ints(&[1, 2, 3]),
        ),
        (
            "def run(s: string, n: int) -> string | nil; s.byteslice(0, n); end",
            vec![Value::bytes("abc"), Value::int(2)],
            Value::bytes("ab"),
        ),
        (
            "def run(h: hash) -> hash; h.remap_keys({ a: :b }); end",
            vec![Value::hash(vec![(b"a".to_vec(), Value::int(1))])],
            Value::hash(vec![(b"b".to_vec(), Value::int(1))]),
        ),
        (
            "def run -> array<string | int | array<int>>; { a: [1, [2]] }.flatten(2); end",
            vec![],
            Value::array(vec![Value::bytes("a"), Value::int(1), ints(&[2])]),
        ),
        (
            "def run(h: hash<string, int>) -> array<string | int>; h.flatten(1); end",
            vec![Value::hash(vec![(b"a".to_vec(), Value::int(1))])],
            Value::array(vec![Value::bytes("a"), Value::int(1)]),
        ),
        (
            "def run(rows: array<array<int>>) -> int | nil; rows.dig(0, 1); end",
            vec![Value::array(vec![ints(&[1, 2])])],
            Value::int(2),
        ),
    ]);
    rescues(
        &[
            "def run(k) -> int; begin; [[1]].dig(0, k); 0; rescue; 'bad'; end; end",
            "def run(m) -> int; begin; {a: 1}.remap_keys(m); 0; rescue; 'bad'; end; end",
            "def run(x: array<int> | hash) -> int; begin; [x].dig(0, 0); 0; rescue; 'bad'; end; end",
        ],
        &[
            "def run -> int; begin; {a: [1]}.dig(:a, 0); 0; rescue; 'bad'; end; end",
            "def run -> int; begin; [1].union([2], []); 0; rescue; 'bad'; end; end",
        ],
    );
    strict_arms(vec![(
        "def run(ys: array<int> | int) -> array<int>; [1].union(ys); end",
        ints(&[2]),
    )]);
}

#[test]
fn count_and_byte_index_arguments_follow_the_runtime_conversion() {
    exact_ints(&[
        ("[1, 2, 3].first(2.0).length", 2),
        ("[1, 2, 3].drop(1.5).first", 2),
        ("\"abc\".getbyte(1.0) || 0", 98),
    ]);
    invalid(&[
        "\"abc\".getbyte([1])",
        "\"abc\".getbyte({})",
        "[1, 2].first([1])",
        "[1, 2].last([1])",
        "[1, 2].take([1])",
        "[1, 2].drop({})",
    ]);
    rescues(
        &[
            "def run(n: int) -> int; begin; [1].first(n); 0; rescue; 'bad'; end; end",
            "def run(xs: array<int>, n: float) -> int; begin; xs.take(n); 0; rescue; 'bad'; end; end",
            "def run(i: int) -> int; begin; 'a'.getbyte(i); 0; rescue; 'bad'; end; end",
        ],
        &[
            "def run(xs: array<int>) -> int; begin; xs.take(1); 0; rescue; 'bad'; end; end",
            "def run -> int; begin; 'a'.getbyte(0); 0; rescue; 'bad'; end; end",
        ],
    );
}

/// Site programs whose whole-file checks used to stop at one of these
/// operations. Each now finishes analysis and still runs.
#[test]
fn previously_unanalyzed_site_programs_finish_checking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/site");
    for path in [
        "rosettacode/popular/anagrams.vibe",
        "rosettacode/popular/best_shuffle.vibe",
        "rosettacode/popular/brazilian_numbers.vibe",
        "rosettacode/popular/matrix_transposition.vibe",
        "rosettacode/popular/range_extraction.vibe",
        "rosettacode/popular/set.vibe",
        "showcase/collections/hash_projection.vibe",
        "showcase/collections/matrix_report.vibe",
        "showcase/collections/reshape.vibe",
        "showcase/commerce/quote_approval.vibe",
        "showcase/finance/statement_grid.vibe",
        "showcase/math/chudnovsky_pi.vibe",
        "showcase/numbers/big_integers.vibe",
        "showcase/strings/text_toolkit.vibe",
        "showcase/workflows/release_readiness.vibe",
        "upstream/arrays/extras.vibe",
        "upstream/enums/operations.vibe",
        "upstream/hashes/transformations.vibe",
        "upstream/stdlib/core_utilities.vibe",
        "upstream/strings/operations.vibe",
    ] {
        let source = fs::read_to_string(root.join(path)).unwrap();
        let script = compile(&source);
        let options = CallOptions {
            limits: Limits {
                steps: None,
                memory_bytes: Some(256 << 20),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let report = script
            .check(&options)
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        assert!(report.incomplete.is_empty(), "{path}: {report:?}");
        script
            .call("run", &[], options)
            .unwrap_or_else(|error| panic!("{path}: {error}"));
    }
}

/// `fetch` and `fetch_values` raise on a missing key as documented. Site
/// programs that rescue or report that failure check cleanly, and checked calls
/// execute both the hit and the miss.
#[test]
fn fetch_misses_are_runtime_errors_in_each_public_checking_scope() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/site");
    let options = CallOptions::default();
    let mut scripts = Vec::new();
    for path in [
        "showcase/reliability/resilient_parse.vibe",
        "showcase/collections/hash_projection.vibe",
    ] {
        let script = compile(&fs::read_to_string(root.join(path)).unwrap());
        let report = script.check(&options).unwrap();
        assert!(report.is_clean(), "{path}: {report:?}");
        script
            .call("run", &[], options.clone())
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        scripts.push(script);
    }
    let prices = Value::hash(vec![
        (b"small".to_vec(), Value::int(5)),
        (b"large".to_vec(), Value::int(9)),
    ]);
    for (key, expected) in [("small", 5), ("huge", 0)] {
        let args = [prices.clone(), Value::symbol(key), Value::int(0)];
        assert!(
            scripts[0]
                .check_call("lookup_or", &args, &options)
                .unwrap()
                .is_clean()
        );
        let CheckedOutcome::Executed(outcome) = scripts[0]
            .checked_call("lookup_or", &args, options.clone())
            .unwrap()
        else {
            panic!("lookup_or({key}) was rejected");
        };
        assert_eq!(outcome.value.as_int(), Some(expected));
    }
    let complete = Value::hash(vec![
        (b"id".to_vec(), Value::int(7)),
        (b"email".to_vec(), Value::bytes("a@b")),
    ]);
    let partial = Value::hash(vec![(b"id".to_vec(), Value::int(7))]);
    for record in [&complete, &partial] {
        let args = [record.clone()];
        assert!(
            scripts[1]
                .check_call("required_fields", &args, &options)
                .unwrap()
                .is_clean()
        );
    }
    let CheckedOutcome::Executed(outcome) = scripts[1]
        .checked_call("required_fields", &[complete], options.clone())
        .unwrap()
    else {
        panic!("required_fields was rejected");
    };
    assert_eq!(outcome.value.to_string(), "[7, a@b]");
    let error = scripts[1]
        .checked_call("required_fields", &[partial], options.clone())
        .unwrap_err();
    assert_eq!(error.class(), Some(ErrorClass::Runtime), "{error}");
    for source in [
        "def run; {a: 1}.fetch(:b); end",
        "def run; [1].fetch(5); end",
        "def run; {a: 1}.fetch_values(:a, :b); end",
    ] {
        let script = compile(source);
        for report in [
            script.check_function("run", &options).unwrap(),
            script.check(&options).unwrap(),
            script.check_call("run", &[], &options).unwrap(),
        ] {
            assert!(report.is_clean(), "{source}: {report:?}");
        }
        let error = script
            .checked_call("run", &[], options.clone())
            .unwrap_err();
        assert_eq!(
            error.class(),
            Some(ErrorClass::Runtime),
            "{source}: {error}"
        );
    }
}
