mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn inspection_preserves_raw_bytes_and_literal_interpolation_markers() {
    let script = Engine::new()
        .compile("def run(x: string) -> string\nx.inspect\nend")
        .unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(b"\0\r\n\t\"\\#{x}\xff")],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        output.value.as_bytes().unwrap(),
        b"\"\0\r\\n\\t\\\"\\\\\\#{x}\xff\""
    );
    assert_eq!(
        evaluate(
            r#"[nil.inspect, true.inspect, :ready.inspect, :"why?".inspect, :"é١".inspect, :"1a".inspect]"#
        ),
        serde_json::json!(["nil", "true", ":ready", ":\"why?\"", ":é١", ":\"1a\""])
    );
}

#[test]
fn nested_literals_round_trip_and_inspection_keeps_hash_order() {
    let source = r##"[{z:nil,"x y":["a",:ready,:"why?","literal\#{x}"]},1..2].inspect"##;
    let text = evaluate(source);
    let text = text.as_str().unwrap();
    assert_eq!(
        text,
        "[{z: nil, \"x y\": [\"a\", :ready, :\"why?\", \"literal\\#{x}\"]}, 1..2]"
    );
    assert_eq!(
        evaluate(&format!("({text}).inspect")),
        serde_json::json!(text)
    );
    assert_eq!(
        evaluate("h: hash<string, int> = {inspect:7,b:2,a:1};h.delete(\"b\");h[\"b\"]=3;h.inspect"),
        serde_json::json!("{inspect: 7, a: 1, b: 3}")
    );
    assert_eq!(
        evaluate("x: any =nil;128.times{x=[x]};x.as(array<any>).inspect.bytesize"),
        serde_json::json!(259)
    );
}

#[test]
fn projections_preserve_selector_order_and_collection_values() {
    assert_eq!(
        evaluate("a=[[1],[2]];out=a.values_at(0,1,0);out[0]&.push(9);[a,out]"),
        serde_json::json!([[[1], [2]], [[1, 9], [2], [1]]])
    );
    assert_eq!(
        evaluate(
            "h={b:[2],a:[1]};out=h.values_at(\"a\",\"missing\",\"b\",\"a\");out[0]&.push(9);[h,out]"
        ),
        serde_json::json!([{"b":[2],"a":[1]},[[1,9],null,[2],[1]]])
    );
    assert_eq!(
        evaluate("[10,20,30].values_at(-1,0..2,5,1...4,2..1)"),
        serde_json::json!([30, 10, 20, 30, null, 20, 30, null])
    );
    // A float selector is refused before anything runs.
    let source = "[10,20,30].values_at(1.9,-1.9)";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101", "V0101"]);
    assert_eq!(
        evaluate(
            "[[].values_at(..-1),[1,2,3].values_at(-2...),[1].values_at(9223372036854775807..9223372036854775807)]"
        ),
        serde_json::json!([[], [2, 3], [null]])
    );
}

#[test]
fn templates_resolve_data_paths_and_serialize_scalars() {
    assert_eq!(
        evaluate(
            r#"enum Status
                       InProgress
                     end
                     "{{user.name}}/{{length}}/{{empty}}/{{flag}}/{{status}}/{{fee}}/{{wait}}/{{when}}".template({user:{name:"Ada"},length:7,empty:nil,flag:false,status:Status::InProgress,fee:money("2.50 USD"),wait:90.seconds,when:Time.utc(2024,1,2)},strict:true)"#
        ),
        serde_json::json!("Ada/7//false/in_progress/2.50 USD/90s/2024-01-02T00:00:00Z")
    );
    assert_eq!(
        evaluate(
            r#"["{{ bad {{name}}".template({name:"Ada"}),"{{{name}}".template({name:"Ada"}),"{{ missing }}".template({}),"{{x..y}}/{{é}}/{{1x}}".template({}),"{{\vname}}".template({name:"Ada"},strict:true)]"#
        ),
        serde_json::json!([
            "{{ bad Ada",
            "{Ada",
            "{{ missing }}",
            "{{x..y}}/{{é}}/{{1x}}",
            "{{\u{b}name}}"
        ])
    );
    // A namespace is not a context, and a method is not a value.
    for (source, code) in [
        ("\"{{length}}\".template(JSON)", "V0101"),
        ("\"{{x}}\".template({x:JSON.parse})", "V0301"),
        ("\"{{utc}}\".template(Time)", "V0101"),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
    for source in [
        "\"{{x}}\".template({},strict:true)",
        "\"{{x}}\".template({x:[]})",
    ] {
        assert!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn blocks_and_rejected_arguments_are_refused_before_host_effects() {
    let mut engine = vibescript::Engine::new();
    engine.register("effect", |_, _| panic!("effect ran"));
    for (source, expected) in [
        ("[1].values_at(0){effect()}", &["V0305"][..]),
        ("{x:1}.values_at(\"x\"){effect()}", &["V0305"]),
        ("\"{{x}}\".template({x:1}){effect()}", &["V0305"]),
        ("nil.inspect{effect()}", &["V0305"]),
        ("[].inspect{effect()}", &["V0305"]),
        ("{}.inspect{effect()}", &["V0305"]),
        ("[1].values_at(0,bad:1){effect()}", &["V0302", "V0305"]),
        (
            "\"x\".template({},strict:nil){effect()}",
            &["V0101", "V0305"],
        ),
        ("nil.inspect(effect())", &["V0301"]),
    ] {
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), expected, "{source}");
    }
}

#[test]
fn rendered_results_release_source_context_and_temporary_storage() {
    let mut bytes = Vec::with_capacity(1 << 20);
    bytes.extend_from_slice(b"Ada");
    let large = Value::bytes(bytes);
    let script = Engine::new()
        .compile("def inspect(x: string) -> string\nx.inspect\nend\ndef template(x: { name: string }) -> string\n\"{{name}}\".template(x)\nend")
        .unwrap();
    let output = script
        .call(
            "inspect",
            std::slice::from_ref(&large),
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"\"Ada\"");
    assert!(output.stats.retained_memory_bytes < 1024);
    let context = Value::hash(vec![(b"name".to_vec(), large)]);
    let output = script
        .call("template", &[context], CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"Ada");
    assert!(output.stats.retained_memory_bytes < 1024);

    let repeated = Engine::new().compile("i=0;while i<500;i+=1;text=\"{{x}}\".template({x:i});out={x:i}.values_at(\"x\").inspect;end;nil").unwrap();
    let output = repeated
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(8192),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert!(output.stats.peak_memory_bytes < 8192);
}

#[test]
fn large_scans_and_projection_growth_stop_before_later_host_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "input.inspect",
        "input.template({})",
        "(\"{{\"+input+\"}}\").template({})",
    ] {
        let script = engine
            .compile(&format!(
                "def run(input: string) -> any\n{expression};effect()\nend"
            ))
            .unwrap();
        let options = CallOptions {
            limits: Limits {
                steps: Some(1000),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script
                .call("run", &[Value::bytes(vec![b'a'; 1 << 20])], options)
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
    }
    for (expression, options, expected) in [
        (
            "[1].values_at(0..100000000)",
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(4096),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ),
        (
            "[1].values_at(0..100000000)",
            CallOptions {
                limits: Limits {
                    steps: Some(1000),
                    memory_bytes: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Steps,
        ),
        (
            "[1].values_at(0..9223372036854775807)",
            CallOptions::default(),
            ErrorKind::OutputLimit,
        ),
    ] {
        let script = engine.compile(&format!("{expression};effect()")).unwrap();
        assert_eq!(script.run(options).unwrap_err().kind, expected);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancelled_and_expired_calls_reject_even_empty_results() {
    for source in [
        "nil.inspect",
        "\"\".template({})",
        "[].values_at",
        "{}.values_at",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let token = vibescript::CancellationToken::new();
        token.cancel();
        assert_eq!(
            script
                .run(CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
        assert_eq!(
            script
                .run(CallOptions {
                    deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
                    ..CallOptions::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::Deadline
        );
    }
}
