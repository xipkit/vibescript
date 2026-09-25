mod common;

use vibescript::{CallOptions, ErrorKind, Limits, Value};

fn options() -> CallOptions {
    CallOptions {
        globals: [
            ("nan".into(), Value::float(f64::NAN)),
            ("negative_zero".into(), Value::float(-0.0)),
            ("min".into(), Value::int(i64::MIN)),
            ("min_float".into(), Value::float(i64::MIN as f64)),
            (
                "big".into(),
                Value::parse_integer("9223372036854775808", 10).unwrap(),
            ),
            (
                "object".into(),
                Value::object(vec![(b"a".to_vec(), Value::int(1))]),
            ),
            (
                "other".into(),
                Value::object(vec![(b"a".to_vec(), Value::float(1.0))]),
            ),
        ]
        .into(),
        ..CallOptions::default()
    }
}

fn forms(receiver: &str, name: &str, argument: &str) -> [String; 3] {
    [
        format!("({receiver}).{name}({argument})"),
        format!("({receiver}).send(:{name},{argument})"),
        format!("({receiver}).public_send(:send,:{name},{argument})"),
    ]
}

fn typed_source(setup: &str, params: &str, expression: &str, expected: bool) -> String {
    let (yes, no) = if expected {
        ("7", "'wrong'")
    } else {
        ("'wrong'", "7")
    };
    format!("{setup}; def run({params}) -> int; if {expression}; {yes}; else; {no}; end; end")
}

/// The checker must select the typed branch exactly and execution must agree.
fn branch(setup: &str, expression: &str, expected: bool, options: &CallOptions) {
    let source = typed_source(setup, "", expression, expected);
    let script = common::gradual_engine()
        .compile(&source)
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    let report = script.check_call("run", &[], options).unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
    let output = script
        .call("run", &[], options.clone())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    assert_eq!(output.value.as_int(), Some(7), "{source}");
}

/// The checker must keep both branches possible and execution must produce the
/// given result.
fn conservative(setup: &str, expression: &str, expected: bool, options: &CallOptions) {
    for order in [true, false] {
        let source = typed_source(setup, "", expression, order);
        let report = common::gradual_engine()
            .compile(&source)
            .unwrap_or_else(|e| panic!("{source}: {e}"))
            .check_call("run", &[], options)
            .unwrap();
        assert!(
            report.incomplete.is_empty() && !report.diagnostics.is_empty(),
            "{source}: {report:?}"
        );
    }
    let source = format!("{setup}; def run -> bool; {expression}; end");
    let output = common::gradual_engine()
        .compile(&source)
        .unwrap()
        .call("run", &[], options.clone())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    assert_eq!(output.value.truthy(), expected, "{source}");
}

fn typed_branch(params: &str, expression: &str, expected: bool, inputs: &[Vec<Value>]) {
    let source = typed_source("", params, expression, expected);
    let script = common::gradual_engine()
        .compile(&source)
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
    for args in inputs {
        let output = script
            .call("run", args, CallOptions::default())
            .unwrap_or_else(|e| panic!("{source} with {args:?}: {e}"));
        assert_eq!(output.value.as_int(), Some(7), "{source} with {args:?}");
    }
}

fn typed_conservative(params: &str, expression: &str, runtime: &[(Vec<Value>, bool)]) {
    for order in [true, false] {
        let source = typed_source("", params, expression, order);
        let report = common::gradual_engine()
            .compile(&source)
            .unwrap_or_else(|e| panic!("{source}: {e}"))
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(
            report.incomplete.is_empty() && !report.diagnostics.is_empty(),
            "{source}: {report:?}"
        );
    }
    let source = format!("def run({params}) -> bool; {expression}; end");
    let script = common::gradual_engine().compile(&source).unwrap();
    for (args, expected) in runtime {
        let output = script
            .call("run", args, CallOptions::default())
            .unwrap_or_else(|e| panic!("{source} with {args:?}: {e}"));
        assert_eq!(output.value.truthy(), *expected, "{source} with {args:?}");
    }
}

#[test]
fn strict_helper_selects_typed_branches_for_literals_and_nested_values() {
    let options = options();
    for (receiver, argument, expected) in [
        ("1", "1.0", false),
        ("1.0", "1", false),
        ("1", "1", true),
        ("nil", "nil", true),
        ("[1]", "[1.0]", false),
        ("[1.0]", "[1]", false),
        ("[1]", "[1]", true),
        ("[1,:x]", "[1,\"x\"]", false),
        ("{a:[1]}", "{a:[1.0]}", false),
        ("{a:1,b:[2]}", "{b:[2],\"a\":1}", true),
        ("{a:1}", "{b:1}", false),
        ("{a:1}", "{a:1,b:2}", false),
        ("[[1,[2]]]", "[[1,[2]]]", true),
        ("[[1,[2.0]]]", "[[1,[2]]]", false),
        ("nan", "nan", false),
        ("[nan]", "[nan]", false),
        ("negative_zero", "0.0", true),
        ("[negative_zero]", "[0.0]", true),
        ("[0]", "[negative_zero]", false),
        ("[9007199254740993]", "[9007199254740993]", true),
        ("[9007199254740993]", "[9007199254740992.0]", false),
        ("[min]", "[min_float]", false),
        ("min", "min_float", false),
        ("big", "1.0", false),
        ("object", "other", false),
        ("object", "{a:1}", false),
        ("{a:1}", "object", false),
        ("[object]", "[object]", true),
        ("\"a\"", ":a", false),
        ("1", "\"1\"", false),
        ("[nil]", "[false]", false),
        ("[1]", "{a:1}", false),
        ("nil", "[]", false),
    ] {
        for call in forms(receiver, "eql?", argument) {
            branch("", &call, expected, &options);
        }
    }
}

#[test]
fn identity_helper_selects_typed_branches_for_root_identity_and_nested_values() {
    let options = options();
    for (receiver, argument, expected) in [
        ("1", "1.0", false),
        ("1", "1", true),
        ("[1]", "[1.0]", true),
        ("[1.0]", "[1]", true),
        ("{a:[1]}", "{a:[1.0]}", true),
        ("[[1,[2.0]]]", "[[1,[2]]]", true),
        ("[1,:x]", "[1,\"x\"]", false),
        ("nan", "nan", true),
        ("nan", "1.0", false),
        ("[nan]", "[nan]", false),
        ("{a:nan}", "{a:nan}", false),
        ("negative_zero", "0.0", true),
        ("[0]", "[negative_zero]", true),
        ("[9007199254740993]", "[9007199254740992.0]", false),
        ("[9223372036854775807]", "[9223372036854775808.0]", false),
        ("[min]", "[min_float]", true),
        ("min", "min_float", false),
        ("big", "1.0", false),
        ("object", "other", true),
        ("object", "{a:1}", false),
        ("[object]", "[other]", true),
        ("object", "object", true),
        ("{a:1,b:[2]}", "{b:[2.0],\"a\":1.0}", true),
        ("{a:1}", "{a:1,b:2}", false),
        ("[1]", "{a:1}", false),
        ("nil", "[]", false),
    ] {
        for call in forms(receiver, "equal?", argument) {
            branch("", &call, expected, &options);
        }
    }
}

#[test]
fn helpers_reject_known_type_mismatches_for_general_typed_inputs() {
    let big = Value::parse_integer("9223372036854775808", 10).unwrap();
    let ints: &[Vec<Value>] = &[vec![Value::int(1)], vec![big]];
    let floats: &[Vec<Value>] = &[vec![Value::float(1.0)], vec![Value::float(f64::NAN)]];
    let strings: &[Vec<Value>] = &[vec![Value::bytes(b"a")]];
    let bools: &[Vec<Value>] = &[vec![Value::boolean(true)]];
    for (params, expression, inputs) in [
        ("x:int", "x.eql?(1.0)", ints),
        ("x:int", "x.equal?(1.0)", ints),
        ("x:int", "[x].eql?([1.0])", ints),
        ("x:int", "x.eql?(\"1\")", ints),
        ("x:int", "x.equal?([])", ints),
        ("x:int", "[x].equal?([nil])", ints),
        ("x:int", "({a:x}).eql?({b:x})", ints),
        ("x:float", "x.equal?(1)", floats),
        ("x:float", "[x].eql?([1])", floats),
        ("x:string", "x.eql?(:a)", strings),
        ("x:string", "x.equal?(1)", strings),
        ("x:bool", "x.eql?(nil)", bools),
    ] {
        typed_branch(params, expression, false, inputs);
    }
}

#[test]
fn helpers_keep_general_inputs_conservative_where_runtime_identity_varies() {
    let options = options();
    // A general integer may be a big payload: identity depends on the runtime
    // allocation and strict equality on the hidden value.
    conservative("", "big.equal?(big)", true, &options);
    conservative("", "big.equal?(big + 0)", false, &options);
    conservative("", "big.eql?(big + 0)", true, &options);
    conservative("", "big.eql?(1)", false, &options);
    let big = Value::parse_integer("9223372036854775808", 10).unwrap();
    typed_conservative(
        "x:int",
        "x.equal?(x + 0)",
        &[(vec![Value::int(1)], true), (vec![big], false)],
    );
    typed_conservative(
        "x:int",
        "[x].equal?([1.0])",
        &[(vec![Value::int(1)], true), (vec![Value::int(2)], false)],
    );
    typed_conservative(
        "x:float",
        "x.eql?(x)",
        &[
            (vec![Value::float(1.0)], true),
            (vec![Value::float(f64::NAN)], false),
        ],
    );
    typed_conservative(
        "x:float",
        "[x].equal?([x])",
        &[
            (vec![Value::float(1.0)], true),
            (vec![Value::float(f64::NAN)], false),
        ],
    );
}

#[test]
fn helpers_preserve_instance_enum_and_source_override_dispatch() {
    let setup = "enum E; A; B; end; class Plain; property link; end; class C; def eql?(x); x+3; end; def equal?(x); x+4; end; end";
    let options = CallOptions::default();
    for (expression, expected) in [
        ("(begin; a=Plain.new; a.equal?(a); end)", true),
        ("(begin; a=Plain.new; a.eql?(Plain.new); end)", false),
        (
            "(begin; a=Plain.new; a.link=a; a.equal?(a.link); end)",
            true,
        ),
        ("(begin; a=Plain.new; [a].eql?([a]); end)", true),
        (
            "(begin; a=Plain.new; b=Plain.new; [a].equal?([b]); end)",
            false,
        ),
        ("Plain.new.eql?([])", false),
        ("Plain.new.equal?(nil)", false),
        ("Plain.new.eql?(1)", false),
        ("E.equal?(E)", true),
        ("E::A.equal?(E::A)", true),
        ("E::A.equal?(E::B)", false),
        ("E::A.eql?(E::A)", true),
        ("E::A.eql?(1)", false),
        ("E::A.eql?(:a)", false),
        ("E::A.equal?(E)", false),
        ("[E::A].eql?([E::A])", true),
        ("[E::A].equal?([E::B])", false),
        ("(begin; h={\"eql?\":3,\"equal?\":4}; h.eql?(h); end)", true),
        (
            "(begin; h={\"eql?\":3,\"equal?\":4}; h.equal?(h); end)",
            true,
        ),
    ] {
        branch(setup, expression, expected, &options);
    }
    // Source methods named after the helpers keep precedence over the native ones.
    let source = format!("{setup}; def run -> int; C.new.eql?(2) + C.new.equal?(3); end");
    let script = common::gradual_engine().compile(&source).unwrap();
    let report = script.check_call("run", &[], &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        script.call("run", &[], options).unwrap().value.as_int(),
        Some(12)
    );
}

#[test]
fn helpers_keep_protected_values_and_temporal_block_contracts() {
    let options = CallOptions::default();
    let matched =
        |body: &str| format!("(begin; m=\"ab\".match(/(b)/); if m; {body}; else; false; end; end)");
    for (expression, expected) in [
        (matched("m.eql?(nil)"), false),
        (matched("m.equal?([])"), false),
        (matched("m[:begin].eql?(nil)"), false),
        (matched("m[:begin].equal?(1)"), false),
        (
            "(begin; begin; raise \"bad\"; rescue => e; e.equal?(nil); end; end)".to_owned(),
            false,
        ),
        ("1.seconds.eql?(nil)".to_owned(), false),
        ("Time.at(0).equal?(1.seconds)".to_owned(), false),
        ("1.seconds.equal?(1)".to_owned(), false),
    ] {
        branch("", &expression, expected, &options);
    }
    for (expression, expected) in [
        (matched("m.eql?(m)"), true),
        (matched("m[:begin].equal?(m[:begin])"), true),
        ("1.seconds.eql?(1.seconds)".to_owned(), true),
        ("Time.at(0).equal?(Time.at(0))".to_owned(), true),
    ] {
        conservative("", &expression, expected, &options);
    }
    // Temporal eql? tolerates an ignored block; the block never runs.
    for expression in [
        "1.seconds.eql?(1.seconds) {raise \"unused\"}",
        "Time.at(0).eql?(Time.at(0)) {raise \"unused\"}",
    ] {
        let source = format!("def run -> bool; {expression}; end");
        let script = common::gradual_engine().compile(&source).unwrap();
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .truthy(),
            "{source}"
        );
    }
}

#[test]
fn helpers_reject_bad_call_shapes_without_running_blocks() {
    for (expression, kind) in [
        ("[1].eql?()", ErrorKind::Argument),
        ("1.seconds.eql?()", ErrorKind::Argument),
        ("Time.at(0).equal?()", ErrorKind::Argument),
        ("[1].eql?([1],[1])", ErrorKind::Argument),
        ("[1].eql?([1],x:1)", ErrorKind::Argument),
        ("[1].equal?([1]) {raise \"entered\"}", ErrorKind::Argument),
        (
            "({a:1}).eql?({a:1}) {raise \"entered\"}",
            ErrorKind::Argument,
        ),
        ("[1].equal?", ErrorKind::Type),
    ] {
        let source = format!("def run -> bool; {expression}; end");
        let script = common::gradual_engine().compile(&source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(
            report.incomplete.is_empty() && !report.diagnostics.is_empty(),
            "{source}: {report:?}"
        );
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, kind, "{source}: {error}");
    }
}

#[test]
fn helper_checks_obey_exact_and_sampled_work_and_memory_limits() {
    let options = options();
    let script = common::gradual_engine()
        .compile(
            "def run -> int; if [1,[2,{a:[3]}]].eql?([1.0,[2.0,{a:[3.0]}]]); return 'wrong'; end; if [nan].equal?([nan]); return 'wrong'; end; if ({a:big}).eql?({a:1.0}); return 'wrong'; end; 7; end",
        )
        .unwrap();
    let baseline = script.check_call("run", &[], &options).unwrap();
    assert!(baseline.is_clean(), "{baseline:?}");
    let stats = baseline.stats;
    drop(baseline);
    assert_eq!(
        script
            .call("run", &[], options.clone())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    for (memory, steps, expected) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..options.clone()
        };
        assert_eq!(
            script
                .check_call("run", &[], &options)
                .err()
                .map(|e| e.kind),
            expected
        );
    }
    for sample in 0..16 {
        for memory in [false, true] {
            let mut options = options.clone();
            let kind = if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 16);
                ErrorKind::Memory
            } else {
                options.limits.steps = Some(stats.steps * sample as u64 / 16);
                ErrorKind::Steps
            };
            assert_eq!(
                script.check_call("run", &[], &options).unwrap_err().kind,
                kind
            );
        }
    }
}
