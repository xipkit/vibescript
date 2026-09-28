mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

const IDENTITY: &str = "def identity(x: any) -> any\nx\nend\n";

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(&format!("{IDENTITY}{source}"))
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let output = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn reusable_literals_validate_json_and_preserve_collection_values() {
    assert_eq!(
        evaluate(
            r#"schema={id:int,tags?:array<string>,...}
               original=JSON.parse_as("{\"id\":7,\"extra\":[1]}",schema)
               changed=original.as({id:int,extra:array<int>});changed["extra"].push(2)
               [original,changed,schema==schema.dup,schema == nil,
                JSON.parse_as("[1,2]",array<int>),JSON.parse_as("null",int?),
                JSON.parse_as("{\"a\":3}",hash<string,int>)]"#
        ),
        serde_json::json!([
            {"id":7,"extra":[1]}, {"id":7,"extra":[1,2]}, true, false,
            [1,2], null, {"a":3}
        ])
    );
    for expression in [
        "JSON.parse_as(\"1.0\",int)",
        "JSON.parse_as(\"1\",float)",
        "JSON.parse_as(\"{}\",{id:int})",
        "JSON.parse_as(\"{\\\"id\\\":1,\\\"extra\\\":2}\",{id:int})",
        "JSON.parse_as(\"{\\\"id\\\":null}\",{id?:int})",
        "JSON.parse_as(\"{}\",hash<int,any>)",
    ] {
        let error = Engine::new()
            .compile(expression)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}");
    }
    // Empty braces are a hash, not a shape, so the checker refuses them as
    // a schema.
    let source = "JSON.parse_as(\"invalid\",{})";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        Engine::new()
            .compile("JSON.parse_as(\"invalid\",int)")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
    // The text must be a string, which is checked before running now.
    let error = vibescript::Engine::new()
        .compile("JSON.parse_as(:invalid,int)")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(error.diagnostics()[0].span.start, 14);
}

#[test]
fn canonical_types_are_structural_but_preserve_union_order_and_literal_keys() {
    assert_eq!(
        evaluate(
            r##"a={z:INT,"flag?":bool,items:array<string>,name?:string,...}
               b={items:array<string>,name?:string,"flag?":bool,z:int,...}
               ["#{a}",a==b,{x:int | string}=={x:string | int},
                {x:object}=={x:hash},{"x?":bool}=={x?:bool},
                "#{identity(Time)}","#{identity(Duration)}"]"##
        ),
        serde_json::json!([
            "<Shape { \"flag?\": bool, items: array<string>, name?: string, z: int, ... }>",
            true,
            false,
            false,
            false,
            "<object>",
            "<object>"
        ])
    );
    assert_eq!(
        evaluate(r##""#{{"\x85?":int,"\u0085?":int}}""##),
        serde_json::json!("<Shape { \"\\x85?\": int, \"\\u0085?\": int }>")
    );
}

#[test]
fn literal_fallback_uses_bound_names_and_current_lexical_scopes() {
    assert_eq!(
        evaluate("int=7;[identity(int),{x:int},identity(array<int>) == nil]"),
        serde_json::json!([7,{"x":7},false])
    );
    assert_eq!(evaluate("int=7;{x:int,}"), serde_json::json!({"x":7}));
    assert_eq!(
        Engine::new()
            .compile("{x:int,}")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    // A local assigned on one path only cannot be read, so it no longer
    // falls back to the type.
    let source = format!("{IDENTITY}if false;int=7;end;\"#{{identity(int)}}\"");
    let error = vibescript::Engine::new().compile(&source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0202"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("int)").unwrap()
    );
    assert_eq!(
        evaluate("\"#{identity(int)}\""),
        serde_json::json!("<Shape int>")
    );
    assert_eq!(
        evaluate("[7].map{|int|[1].map{[identity(int),{x:int}]}}"),
        serde_json::json!([[[7,{"x":7}]]])
    );
    assert_eq!(
        evaluate("string?=9;[identity(string?),\"#{{x:string?}}\"]"),
        serde_json::json!([9, "<Shape { x: string? }>"])
    );
    let mut engine = Engine::new();
    engine.register("int", |_, _| Ok(Value::int(7)));
    let script = engine.compile(&format!("{IDENTITY}identity(int)")).unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap().value.as_int(),
        Some(7)
    );
    let result = engine
        .compile("JSON.parse_as(\"[7]\",array<int>)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_array().unwrap()[0].as_int(), Some(7));
}

#[test]
fn hosts_can_retain_import_and_reuse_types_after_the_script_is_dropped() {
    assert_eq!(size_of::<Value>(), 16);
    let script = Engine::new().compile("{name:string}").unwrap();
    let schema = script.run(CallOptions::default()).unwrap();
    assert_eq!(schema.value.type_name(), "shape");
    assert_eq!(
        schema.value.as_type_literal(),
        Some(b"{ name: string }".as_slice())
    );
    assert!(schema.stats.retained_memory_bytes > 0);
    assert_eq!(Value::nil().as_type_literal(), None);
    drop(script);
    let retained = schema.value;
    let mut engine = Engine::new();
    engine.register("schema", move |ctx, _| ctx.import(&retained));
    let output = engine
        .compile("JSON.parse_as(\"{\\\"name\\\":\\\"Ada\\\"}\",schema().as(type<{ name: string }>))[\"name\"]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_bytes(), Some(b"Ada".as_slice()));
    assert!(output.stats.retained_memory_bytes < 256);

    let repeated = engine
        .compile("i=0;while i<1000;type=schema();text=\"#{type}\";i+=1;end;nil")
        .unwrap();
    for _ in 0..3 {
        let result = repeated
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(8192),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        assert_eq!(result.stats.retained_memory_bytes, 0);
        assert!(result.stats.peak_memory_bytes < 8192);
    }
}

#[test]
fn identity_method_blocks_are_rejected_before_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    // `dup` takes no block, which is refused before anything runs; the
    // removed `nil?`, `itself`, `tap` and `yield_self` are covered by the
    // surface tests.
    for receiver in [
        "nil", "true", "1", "1.0", "\"x\"", ":x", "[]", "{}", "1..3", "{x:int}",
    ] {
        let source = format!("({receiver}).dup{{mark()}};mark()");
        let error = engine.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0305"], "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find("{mark").unwrap(),
            "{source}"
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn long_schema_work_and_storage_obey_call_limits_before_host_effects() {
    let name = "x".repeat(32768);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for operation in ["\"#{schema}\"", "schema==schema.dup"] {
        let script = engine
            .compile(&format!("schema={{{name}:int}};{operation};mark()"))
            .unwrap();
        for kind in [
            ErrorKind::Memory,
            ErrorKind::Steps,
            ErrorKind::Cancelled,
            ErrorKind::Deadline,
        ] {
            let mut options = CallOptions::default();
            match kind {
                ErrorKind::Memory => options.limits.memory_bytes = Some(16384),
                ErrorKind::Steps => options.limits.steps = Some(32),
                ErrorKind::Cancelled => options.cancellation.cancel(),
                ErrorKind::Deadline => options.deadline = Some(std::time::Instant::now()),
                _ => unreachable!(),
            }
            assert_eq!(script.run(options).unwrap_err().kind, kind, "{operation}");
        }
    }
    assert_eq!(
        engine
            .compile("cancel();schema={x:int};mark()")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn nested_type_and_value_parsing_reaches_its_guards() {
    for (prefix, suffix, depth) in [("array<", ">", 300), ("{x:", "}", 1100)] {
        let source = format!(
            "{IDENTITY}identity({}int{})",
            prefix.repeat(depth),
            suffix.repeat(depth)
        );
        assert_eq!(
            Engine::new().compile(&source).err().unwrap().kind,
            ErrorKind::Syntax
        );
    }
    for leaf in ["int", "1"] {
        let source = format!("{}{}{}", "{x:".repeat(1100), leaf, "}".repeat(1100));
        assert_eq!(
            Engine::new().compile(&source).err().unwrap().kind,
            ErrorKind::Syntax
        );
    }
    for depth in [31, 63] {
        let source = format!(
            "{IDENTITY}identity({}int{})",
            "array<".repeat(depth),
            ">".repeat(depth)
        );
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert!(result.value.as_type_literal().is_some());
    }
}
