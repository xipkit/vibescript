use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn strict_equality_checks_numeric_kinds_at_every_depth() {
    assert_eq!(
        result(
            "[1.eql?(1.0),1.equal?(1.0),[1].eql?([1.0]),[1].equal?([1.0]),{a:[1]}.eql?({a:[1.0]}),{a:[1]}.equal?({a:[1.0]}),[1,:x].eql?([1,\"x\"])]"
        ),
        serde_json::json!([false, false, false, true, false, true, false])
    );
    assert_eq!(
        result("a=0.0/0.0;[a.eql?(a),a.equal?(0.0/0.0),[a].equal?([a]),(-0.0).eql?(0.0)]"),
        serde_json::json!([false, true, false, true])
    );
}

#[test]
fn integer_float_equality_does_not_round_away_distinct_values() {
    for (integer, float, expected) in [
        (9_007_199_254_740_993, 9_007_199_254_740_992.0, false),
        (-9_007_199_254_740_993, -9_007_199_254_740_992.0, false),
        (i64::MAX, 9_223_372_036_854_775_808.0, false),
        (i64::MIN, -9_223_372_036_854_775_808.0, true),
        (9_007_199_254_740_992, 9_007_199_254_740_992.0, true),
        (0, -0.0, true),
        (0, f64::from_bits(1), false),
        (0, f64::NAN, false),
        (i64::MAX, f64::INFINITY, false),
        (i64::MIN, f64::NEG_INFINITY, false),
    ] {
        let output = Engine::new()
            .compile("def run(a,b)\n[a == b,b == a,a != b,[a].equal?([b]),{x:a}.equal?({x:b}),a.eql?(b),a.equal?(b)]\nend")
            .unwrap()
            .call("run", &[Value::int(integer),Value::float(float)], CallOptions::default())
            .unwrap();
        let values: Vec<_> = output
            .value
            .as_array()
            .unwrap()
            .iter()
            .map(Value::truthy)
            .collect();
        assert_eq!(
            values,
            vec![
                expected, expected, !expected, expected, expected, false, false
            ],
            "{integer} / {float}"
        );
    }
}

#[test]
fn identity_preserves_big_payloads_and_nominal_values() {
    assert_eq!(
        result(
            "a=9223372036854775808;[a.eql?(a+0),a.equal?(a),a.equal?(a.dup),a.equal?(a+0),a.equal?(+a),a.equal?(-(-a))]"
        ),
        serde_json::json!([true, true, true, false, true, false])
    );
    let source = "enum E\nA\nB\nend\nclass C\nend\na=C.new;[E.equal?(E),E::A.equal?(E::A),E::A.equal?(E::B),E::A.eql?(E::A),C.equal?(C),a.equal?(a),a.equal?(C.new)]";
    assert_eq!(
        result(source),
        serde_json::json!([true, true, false, true, true, true, false])
    );
    let integer = Value::parse_integer("9223372036854775808", 10).unwrap();
    let output = Engine::new()
        .compile("def run(a,b)\na.equal?(b)\nend")
        .unwrap()
        .call("run", &[integer.clone(), integer], CallOptions::default())
        .unwrap();
    assert_eq!(output.value.to_string(), "true");
}

#[test]
fn scoped_exports_and_match_offset_callables_support_equality() {
    assert_eq!(
        result(
            "[JSON::parse.eql?(nil),JSON::parse.equal?(JSON::parse),JSON::parse.equal?(JSON::stringify),(JSON::parse.eql?)(JSON::parse)]"
        ),
        serde_json::json!([false, true, false, true])
    );
    assert_eq!(
        result(
            "m=\"ab\".match(/(b)/);[m[:begin].eql?(nil),m[:begin].equal?(m[:begin]),m[:begin].equal?(m[:end]),(m[:begin].eql?)(m[:begin])]"
        ),
        serde_json::json!([false, true, false, true])
    );
}

#[test]
fn wrapped_calls_capture_the_receiver_before_argument_mutation() {
    assert_eq!(
        result("a=[1];r=(a.eql?)(a.push(2));[r,a]"),
        serde_json::json!([false, [1, 2]])
    );
    assert_eq!(
        result("a=[[1]];r=(a.equal?)(begin\na[0].push(2);a\nend);[r,a]"),
        serde_json::json!([false, [[1, 2]]])
    );
    assert_eq!(
        result("class C\nproperty link\nend\na=C.new;a.link=a;[(a.equal?)(a.link),a.eql?(a.link)]"),
        serde_json::json!([true, true])
    );
}

#[test]
fn overrides_and_temporal_block_contracts_keep_their_precedence() {
    assert_eq!(
        result(
            "class C\ndef eql?(x)\nx+3\nend\ndef equal?(x)\nx+4\nend\nend\na=C.new;[a.eql?(2),(a.equal?)(2)]"
        ),
        serde_json::json!([5, 6])
    );
    assert_eq!(
        result(
            "[1.seconds.eql?(1.seconds) {raise \"unused\"},Time.at(0).eql?(Time.at(0)) {raise \"unused\"}]"
        ),
        serde_json::json!([true, true])
    );
    assert_eq!(
        result("h={\"eql?\":3,\"equal?\":4};[h.eql?(h),h.equal?(h),h[\"eql?\"],h[\"equal?\"]]"),
        serde_json::json!([true, true, 3, 4])
    );
}

#[test]
fn detached_predicates_and_invalid_calls_reject_before_block_effects() {
    for expression in [
        "1.eql?",
        "[1].equal?",
        "1.seconds.eql?",
        "Time.at(0).equal?",
    ] {
        let error = Engine::new()
            .compile(expression)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error}");
        assert!(
            error.message.contains("cannot be used as a value"),
            "{error}"
        );
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for (source, message) in [
        ("1.eql?()", "int.eql? expects 1 argument, got 0"),
        ("1.eql?(1,2)", "int.eql? expects 1 argument, got 2"),
        (
            "1.eql?(1,x:2) {entered()}",
            "int.eql? does not accept keyword arguments",
        ),
        (
            "(1.equal?)(1) {entered()}",
            "int.equal? does not accept a block",
        ),
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, message);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn comparisons_meter_raw_bytes_and_release_captured_values() {
    let script = Engine::new()
        .compile("def run(a,b)\n[(a.eql?)(b),a.equal?(b)]\nend")
        .unwrap();
    let mut data = vec![0xff; 64 << 10];
    data[0] = 0;
    let args = [Value::bytes(data.clone()), Value::bytes(data)];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.to_string(), "[true, true]");
    assert!(output.stats.steps >= 2 * (64 << 10) / 64);
    assert!(output.stats.retained_memory_bytes < 1024);
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.call("run", &args, options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(
        script.call("run", &args, options.clone()).unwrap_err().kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("run", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn cancellation_during_arguments_prevents_comparison_and_later_effects() {
    let token = CancellationToken::new();
    let cancellation = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancellation.cancel();
        Ok(Value::bytes(vec![1; 8192]))
    });
    engine.register("after", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let error = engine.compile("begin\n((\"x\"*8192).eql?)(stop());after()\nrescue LimitError | RuntimeError\nafter()\nensure\nafter()\nend").unwrap().run(CallOptions { cancellation: token, ..CallOptions::default() }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn enum_imports_preserve_aliases_and_rebind_owned_arguments() {
    let script = Engine::new().compile("enum E\nA\nB\nend\ndef make\n[E,E::A,E::A,E::B]\nend\ndef check(x)\n[x[0].equal?(E),x[1].equal?(E::A),x[1].equal?(x[2]),x[3].enum.equal?(x[0]),x[1].equal?(x[3])]\nend").unwrap();
    let output = script.call("make", &[], CallOptions::default()).unwrap();
    let args = [output.value.clone()];
    let checked = script.call("check", &args, CallOptions::default()).unwrap();
    assert_eq!(checked.value.to_string(), "[true, true, true, true, false]");

    let foreign = Engine::new()
        .compile("def check(x)\n[x[1].equal?(x[2]),x[3].enum.equal?(x[0]),x[1].equal?(x[3])]\nend")
        .unwrap();
    assert_eq!(
        foreign
            .call("check", &args, CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "[true, true, false]"
    );
    let mut options = CallOptions::default();
    options.limits.steps = Some(checked.stats.steps);
    options.limits.memory_bytes = Some(checked.stats.peak_memory_bytes);
    script.call("check", &args, options.clone()).unwrap();
    options.limits.memory_bytes = Some(checked.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("check", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn enum_rebinding_reaches_instance_fields_and_keyword_arguments() {
    let script = Engine::new().compile("enum E\nA\nend\nclass Box\nproperty items\nend\ndef make\nb=Box.new;b.items=[E::A,E::A];b\nend\ndef check(box:,item:)\n[box.items[0].equal?(E::A),box.items[0].equal?(box.items[1]),item.equal?(E::A)]\nend\ndef member\nE::A\nend").unwrap();
    let value = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let item = script
        .call("member", &[], CallOptions::default())
        .unwrap()
        .value;
    let output = script
        .call_with_keywords(
            "check",
            &[],
            &[("box".into(), value), ("item".into(), item)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(output.value.to_string(), "[true, true, true]");
    assert!(output.stats.retained_memory_bytes < 1024);
}

#[test]
fn callback_enum_results_keep_separate_identity_from_the_invocation() {
    let captured = Arc::new(std::sync::Mutex::new(Value::nil()));
    let returned = captured.clone();
    let mut engine = Engine::new();
    engine.register("saved", move |_, _| Ok(returned.lock().unwrap().clone()));
    let script = engine.compile("enum E\nA\nend\ndef make\nE::A\nend\ndef check(input)\na=saved();[input.equal?(E::A),a.eql?(E::A),a.equal?(E::A)]\nend").unwrap();
    let member = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    *captured.lock().unwrap() = member.clone();
    let output = script
        .call("check", &[member], CallOptions::default())
        .unwrap();
    assert_eq!(output.value.to_string(), "[true, true, false]");
}
