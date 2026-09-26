mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Capability, Engine, ErrorKind, HostMethod, Value, stringify_json};

fn options(entries: &[(&str, Value)]) -> CallOptions {
    CallOptions {
        globals: entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
        ..CallOptions::default()
    }
}

/// An engine that declares each global's type, as static types need.
fn declaring(globals: &[(&str, &str)]) -> Engine {
    let mut engine = Engine::new();
    for (name, ty) in globals {
        engine.declare_global(*name, ty).unwrap();
    }
    engine
}

/// A host method that parses its string argument as an integer, as
/// `JSON.parse` would.
fn parse() -> Value {
    HostMethod::new("parse", |_, args, _| {
        let text = std::str::from_utf8(args[0].as_bytes().unwrap()).unwrap();
        Ok(Value::int(text.parse().unwrap()))
    })
    .value()
}

/// An engine that declares each of `names` as a host method like [`parse`],
/// which each call supplies as a global.
fn parsing(names: &[&str]) -> Engine {
    let mut engine = Engine::new();
    for name in names {
        engine
            .declare_capability(&Capability::from_value(*name, parse()))
            .unwrap();
    }
    engine
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

/// Runs `source` for its value, such as a namespace, a class or a type.
fn value(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

#[test]
fn globals_are_isolated_and_mutations_remain_visible_within_each_call() {
    let input = Value::hash(vec![(b"items".to_vec(), Value::array(vec![Value::int(1)]))]);
    let script = declaring(&[("settings", "{ items: array<int> }")])
        .compile("def update -> array<{ items: array<int> }>;before=settings;settings[\"items\"].push(2);[before,settings];end")
        .unwrap();
    let opts = options(&[("settings", input.clone())]);
    common::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    for _ in 0..3 {
                        let output = script.call("update", &[], opts.clone()).unwrap();
                        assert_eq!(
                            json(&output.value),
                            serde_json::json!([{ "items": [1] }, { "items": [1,2] }])
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    assert_eq!(json(&input), serde_json::json!({ "items": [1] }));
}

#[test]
fn host_globals_override_functions_declarations_hosts_and_builtins() {
    let names = ["helper", "Box", "Status", "Math", "host"];
    for composite in [false, true] {
        let (read, ty) = if composite {
            (
                "helper.fetch(0)+Box.fetch(0)+Status.fetch(0)+Math.fetch(0)+host.fetch(0)",
                "array<int>",
            )
        } else {
            ("helper+Box+Status+Math+host", "int")
        };
        let mut engine = declaring(&names.map(|name| (name, ty)));
        engine.register("host", |_, _| panic!("shadowed host ran"));
        let script = engine
            .compile(&format!(
                "def helper -> int;99;end;class Box;end;enum Status;Ready;end;def run -> int;{read};end"
            ))
            .unwrap();
        let entries: Vec<_> = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let value = Value::int(index as i64 + 1);
                (
                    name,
                    if composite {
                        Value::array(vec![value])
                    } else {
                        value
                    },
                )
            })
            .collect();
        assert_eq!(
            script
                .call("run", &[], options(&entries))
                .unwrap()
                .value
                .as_int(),
            Some(15)
        );
    }
}

#[test]
fn nil_overrides_and_parameter_shadowing_do_not_lose_bindings() {
    let script = declaring(&[("helper", "int?")])
        .compile("def helper -> int;99;end;def f(helper: int) -> int;helper+=1;helper;end;def run -> array<int?>;[f(6),helper];end")
        .unwrap();
    assert_eq!(
        json(
            &script
                .call("run", &[], options(&[("helper", Value::nil())]))
                .unwrap()
                .value
        ),
        serde_json::json!([7, null])
    );
    // An assignment binds the name over the global with its own type.
    for body in [
        "items: int? = nil;items",
        "items=1;items+=2;items",
        "items=1;[2].each{|items|items+=1};items",
        "items=1;[2].each{items+=2};items",
    ] {
        let expected = match body {
            "items: int? = nil;items" => serde_json::Value::Null,
            "items=1;[2].each{|items|items+=1};items" => serde_json::json!(1),
            _ => serde_json::json!(3),
        };
        let script = declaring(&[("items", "array<int>")]).compile(body).unwrap();
        assert_eq!(
            json(
                &script
                    .run(options(&[("items", Value::array(vec![Value::int(9)]))]))
                    .unwrap()
                    .value
            ),
            expected,
            "{body}"
        );
    }
}

#[test]
fn global_mutation_addresses_survive_parent_growth_and_rebindings() {
    for name in ["rows", "JSON", "Box", "helper"] {
        let engine = declaring(&[(name, "array<array<int>>")]);
        let source = format!(
            "class Box;end;def helper -> int;99;end;before={name};x={name}[-1].push((while true;{name}.push([9]);break 2;end));[x,{name},before]"
        );
        let input = Value::array(vec![Value::array(vec![Value::int(1)])]);
        assert_eq!(
            json(
                &engine
                    .compile(&source)
                    .unwrap()
                    .run(options(&[(name, input.clone())]))
                    .unwrap()
                    .value
            ),
            serde_json::json!([[1, 2], [[1, 2], [9]], [[1]]])
        );
        assert_eq!(json(&input), serde_json::json!([[1]]));
        let source = format!("class Box;end;def helper -> int;99;end;{name}=[7];{name}");
        // A namespace or class name cannot be rebound, even where a global
        // shadows it.
        if name.starts_with(char::is_uppercase) {
            let error = engine.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), ["V0102"], "{name}");
            assert_eq!(
                error.diagnostics()[0].span.start,
                source.find(&format!("{name}=")).unwrap()
            );
            continue;
        }
        assert_eq!(
            json(
                &engine
                    .compile(&source)
                    .unwrap()
                    .run(options(&[(name, input)]))
                    .unwrap()
                    .value
            ),
            serde_json::json!([7])
        );
    }
}

#[test]
fn known_and_computed_calls_capture_global_targets_before_arguments() {
    for expression in [
        "helper((while true;helper=1;break \"3\";end))",
        "helper(*(while true;helper=1;break [\"3\"];end))",
        "(helper)((while true;helper=1;break \"3\";end))",
        "helper (while true;helper=1;break \"3\";end)",
    ] {
        let script = parsing(&["helper"])
            .compile(&format!(
                "def helper(*args: array<any>) -> int;99;end;x={expression};[x,helper]"
            ))
            .unwrap();
        assert_eq!(
            json(&script.run(options(&[("helper", parse())])).unwrap().value),
            serde_json::json!([3, 1]),
            "{expression}"
        );
    }
}

#[test]
fn module_constants_take_precedence_over_host_bindings_in_call_targets() {
    // A constant cannot hold a function, and a class, builtin namespace or
    // host function cannot be rebound, so the constant is the receiver.
    let engine = Engine::new();
    for name in ["Parser"] {
        for expression in [
            format!("{name}.fetch(0)"),
            format!("({name}).fetch(0)"),
            format!("{name}.fetch(*[0])"),
            format!("{name}.fetch 0"),
        ] {
            let source = format!(
                "class Box;end;module M;{name}=[3];def self.run -> int;{expression};end;end;M.run"
            );
            let script = engine.compile(&source).unwrap();
            for (binding, opts) in [
                ("none", CallOptions::default()),
                ("global", options(&[(name, Value::nil())])),
                (
                    "capability",
                    CallOptions {
                        capabilities: vec![Capability::new(name, |_| Ok(Value::nil()))],
                        ..CallOptions::default()
                    },
                ),
            ] {
                assert_eq!(
                    script
                        .run(opts)
                        .unwrap_or_else(|error| panic!("{source}, binding={binding}: {error}"))
                        .value
                        .as_int(),
                    Some(3),
                    "{source}"
                );
            }
        }
    }
}

#[test]
fn module_initializers_call_enclosing_bindings() {
    // A local cannot hold a function, so the enclosing local is the receiver.
    for expression in [
        "helper.fetch(0)",
        "(helper).fetch(0)",
        "helper.fetch(*[0])",
        "helper.fetch 0",
    ] {
        let source = format!("helper=[3];module M;Result={expression};end;M.Result");
        let script = Engine::new().compile(&source).unwrap();
        for opts in [CallOptions::default(), options(&[("helper", Value::nil())])] {
            assert_eq!(
                script.run(opts).unwrap().value.as_int(),
                Some(3),
                "{source}"
            );
        }
    }
}

#[test]
fn block_assignments_update_existing_host_bindings() {
    for body in [
        "count+=1;count",
        "count=count+1;count",
        "[1].each{count+=1};count",
        "[1].each{count=count+1};count",
        "count=9;[1].each{count+=1};count",
    ] {
        let script = declaring(&[("count", "int")])
            .compile(&format!("def run -> int;{body};end"))
            .unwrap();
        let opts = options(&[("count", Value::int(9))]);
        let granted = CallOptions {
            capabilities: vec![Capability::new("count", |_| Ok(Value::int(9)))],
            ..CallOptions::default()
        };
        for binding in [&opts, &granted] {
            for _ in 0..2 {
                assert_eq!(
                    script
                        .call("run", &[], binding.clone())
                        .unwrap()
                        .value
                        .as_int(),
                    Some(10),
                    "{body}"
                );
            }
        }
        assert_eq!(opts.globals["count"].as_int(), Some(9));
    }
}

#[test]
fn unused_composites_and_overwrites_avoid_importing_large_values() {
    let huge = Value::array(vec![Value::bytes(vec![b'x'; 1024]); 512]);
    for strict in [false, true] {
        let mut engine = declaring(&[("big", "array<string>")]);
        engine.set_strict_effects(strict);
        for body in [
            "1",
            "big=1;big",
            "def f(big: int) -> int;big;end;f(1)",
            "enum big;Large;end;enum State;Ready;end;def f(x:State) -> int;1;end;f(:ready)",
        ] {
            let mut opts = options(&[("big", huge.clone())]);
            opts.limits.memory_bytes = Some(48 << 10);
            assert_eq!(
                engine
                    .compile(body)
                    .unwrap()
                    .run(opts)
                    .unwrap()
                    .value
                    .as_int(),
                Some(1),
                "{body}, strict={strict}"
            );
        }
        let mut opts = options(&[("big", huge.clone())]);
        opts.limits.memory_bytes = Some(48 << 10);
        assert_eq!(
            engine
                .compile("big.length")
                .unwrap()
                .run(opts)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
    }
}

#[test]
fn strict_globals_are_validated_before_initializers_defaults_and_callbacks() {
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    let script = engine
        .compile("class C;effect();end;def run(x: any = effect()) -> any;effect();end")
        .unwrap();
    for (source, hidden) in [
        ("JSON", value("JSON")),
        ("a host method", parse()),
        ("{x:int}", value("{x:int}")),
        ("class C;end;C", value("class C;end;C")),
        ("class C;end;C.new", value("class C;end;C.new")),
        (
            "class C;property link: C?;end;c=C.new;c.link=c;c",
            value("class C;property link: C?;end;c=C.new;c.link=c;c"),
        ),
    ] {
        let poison = Value::array(vec![Value::hash(vec![(b"hidden".to_vec(), hidden)])]);
        let err = script
            .call("run", &[], options(&[("unused", poison)]))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Runtime, "{source}: {err}");
        assert!(
            err.message
                .starts_with("strict effects: global unused must be data-only"),
            "{err}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn strict_validation_is_metered_even_for_unused_globals() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("1").unwrap();
    let input = Value::array(vec![Value::int(7); 4096]);
    let opts = options(&[("unused", input)]);
    let baseline = script.run(opts.clone()).unwrap();
    assert!(baseline.stats.steps > 4096);
    for kind in [
        ErrorKind::Steps,
        ErrorKind::Memory,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let mut opts = opts.clone();
        opts.cancellation = vibescript::CancellationToken::new();
        match kind {
            ErrorKind::Steps => opts.limits.steps = Some(baseline.stats.steps - 1),
            ErrorKind::Memory => {
                opts.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1)
            }
            ErrorKind::Cancelled => opts.cancellation.cancel(),
            ErrorKind::Deadline => opts.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        assert_eq!(script.run(opts).unwrap_err().kind, kind);
    }
}

#[test]
fn globals_and_arguments_preserve_independent_collection_values() {
    let input = Value::array(vec![Value::int(1)]);
    let script = declaring(&[("shared", "array<int>"), ("other", "array<int>")])
        .compile("def run(arg: array<int>) -> array<array<int>>;arg.push(2);other.push(3);[arg,shared,other];end")
        .unwrap();
    let opts = options(&[("shared", input.clone()), ("other", input.clone())]);
    let baseline = script
        .call("run", std::slice::from_ref(&input), opts.clone())
        .unwrap();
    assert_eq!(
        json(&baseline.value),
        serde_json::json!([[1, 2], [1], [1, 3]])
    );
    for shortage in [0, 1] {
        for memory in [false, true] {
            let mut exact = opts.clone();
            if memory {
                exact.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - shortage);
            } else {
                exact.limits.steps = Some(baseline.stats.steps - shortage as u64);
            }
            let result = script.call("run", std::slice::from_ref(&input), exact);
            if shortage == 0 {
                assert_eq!(json(&result.unwrap().value), json(&baseline.value));
            } else {
                assert_eq!(
                    result.unwrap_err().kind,
                    if memory {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Steps
                    }
                );
            }
        }
    }
    assert_eq!(json(&input), serde_json::json!([1]));
}

#[test]
fn incoming_enums_rebind_when_lazily_materialized_or_used_in_types() {
    // A global is `any` and never names a type, so the producer casts the
    // incoming member to its enum.
    let producer = declaring(&[("state", "")]).compile("enum Status;Ready;end;def pair -> array<any>;[Status,Status::Ready];end;def typed(x: Status) -> Status;x;end;def run -> array<bool>;[state==Status::Ready,typed(state.as(Status))==state];end").unwrap();
    let pair = producer
        .call("pair", &[], options(&[("state", Value::nil())]))
        .unwrap()
        .value;
    let pair = pair.as_array().unwrap();
    let opts = options(&[("state", pair[1].clone())]);
    assert_eq!(
        json(&producer.call("run", &[], opts.clone()).unwrap().value),
        serde_json::json!([true, true])
    );
    // The consumer cannot name the incoming enum, so it compares the member
    // materialized from the array with the one it receives directly.
    let consumer = declaring(&[("state", "array<any>"), ("ready", "")])
        .compile("def run -> bool;state.fetch(0)==ready;end")
        .unwrap();
    let opts = options(&[
        ("ready", pair[1].clone()),
        ("state", Value::array(vec![pair[1].clone()])),
    ]);
    assert_eq!(
        json(&consumer.call("run", &[], opts).unwrap().value),
        serde_json::json!(true)
    );
}

#[test]
fn imported_global_objects_preserve_cycles_aliases_and_call_isolation() {
    // Another program's instance is `any` here, and its class cannot be
    // named, so the instance comes from the consumer's own class.
    let consumer = declaring(&[("node", "")]).compile("class Node;property link: Node?;property count: int;def initialize;@count=0;@link=self;end;end;def make -> Node;Node.new;end;def run(arg: Node) -> array<int | bool>;arg.count+=1;node=node.as(Node);[node.count,node==arg,node.link==node];end;def inspect(arg: Node) -> int;arg.count;end").unwrap();
    let unbound = options(&[("node", Value::nil())]);
    let original = consumer.call("make", &[], unbound.clone()).unwrap().value;
    for _ in 0..3 {
        let output = consumer
            .call(
                "run",
                std::slice::from_ref(&original),
                options(&[("node", original.clone())]),
            )
            .unwrap();
        assert_eq!(json(&output.value), serde_json::json!([1, true, true]));
        assert_eq!(
            consumer
                .call("inspect", std::slice::from_ref(&original), unbound.clone())
                .unwrap()
                .value
                .as_int(),
            Some(0)
        );
    }
}

#[test]
fn unused_foreign_namespaces_do_not_initialize_and_used_ones_keep_host_ownership() {
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut source = Engine::new();
    source.register("original", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let namespace = source
        .compile("class Counter;@@n: int = original().as(int);def self.bump -> int;@@n+=1;@@n;end;end;Counter")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    effects.store(0, Ordering::SeqCst);
    let mut engine = declaring(&[("Counter", "")]);
    engine.register("original", |_, _| panic!("wrong callback owner"));
    let opts = options(&[("Counter", namespace)]);
    assert_eq!(
        engine
            .compile("1")
            .unwrap()
            .run(opts.clone())
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    // The namespace is `any`, which the script compares but never calls;
    // reading it imports and initializes it with its own callback.
    let script = engine.compile("Counter != nil").unwrap();
    for count in 1..=2 {
        assert_eq!(
            json(&script.run(opts.clone()).unwrap().value),
            serde_json::json!(true)
        );
        assert_eq!(effects.load(Ordering::SeqCst), count);
    }
}

#[test]
fn captured_overrides_cover_host_declaration_block_and_error_paths() {
    for name in ["helper", "host", "Box", "JSON"] {
        let mut engine = parsing(&[name]);
        engine.register("host", |_, _| panic!("shadowed host ran"));
        for form in [format!("{name}(\"3\")"), format!("{name}(*[\"3\"])")] {
            let script = engine
                .compile(&format!(
                    "def helper(*args: array<any>) -> int;99;end;class Box;end;{form}"
                ))
                .unwrap();
            assert_eq!(
                script
                    .run(options(&[(name, parse())]))
                    .unwrap()
                    .value
                    .as_int(),
                Some(3),
                "{form}"
            );
        }
        // The host method takes no block, which the checker refuses.
        let source = format!(
            "def helper(*args: array<any>) -> int;99;end;class Box;end;begin;{name}(\"3\"){{42}};rescue;7;end"
        );
        let error = engine.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0305"], "{name}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find("{42}").unwrap(),
            "{name}"
        );
    }
    let engine = parsing(&["helper"]);
    for argument in [
        "(begin;raise \"argument\";end)",
        "(begin;raise \"argument\";rescue;\"3\";end)",
    ] {
        let script = engine
            .compile(&format!(
                "def helper(*args: array<any>) -> int;99;end;a=begin;helper({argument});rescue;7;end;[a,helper(\"4\")]"
            ))
            .unwrap();
        let expected = if argument.contains("rescue") { 3 } else { 7 };
        assert_eq!(
            json(&script.run(options(&[("helper", parse())])).unwrap().value),
            serde_json::json!([expected, 4])
        );
    }
}

#[test]
fn global_type_overrides_and_depth_guards_cannot_be_bypassed() {
    let script = declaring(&[("Status", "int")])
        .compile("enum Status;Ready;end;def typed(x:Status) -> Status;x;end;def run -> Status;typed(:ready);end")
        .unwrap();
    assert_eq!(
        script
            .call("run", &[], options(&[("Status", Value::int(1))]))
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    let mut deep = Value::int(1);
    for _ in 0..10_001 {
        deep = Value::array(vec![deep]);
    }
    let mut engine = declaring(&[("unused", "")]);
    let opts = options(&[("unused", deep)]);
    assert_eq!(
        engine
            .compile("1")
            .unwrap()
            .run(opts.clone())
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
    assert_eq!(
        engine
            .compile("unused")
            .unwrap()
            .run(opts.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    engine.set_strict_effects(true);
    assert_eq!(
        engine.compile("1").unwrap().run(opts).unwrap_err().kind,
        ErrorKind::Recursion
    );
}

#[test]
fn strict_data_globals_accept_shared_subgraphs_without_exponential_scans() {
    let mut data = Value::int(1);
    for _ in 0..64 {
        data = Value::array(vec![data.clone(), data]);
    }
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let mut opts = options(&[("unused", data)]);
    opts.limits.steps = Some(10_000);
    opts.limits.memory_bytes = Some(48 << 10);
    assert_eq!(
        engine
            .compile("1")
            .unwrap()
            .run(opts)
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
    let mut shared = Value::int(1);
    for _ in 0..80 {
        shared = Value::array(vec![shared]);
    }
    let mut nested = shared.clone();
    for _ in 0..9_921 {
        nested = Value::array(vec![nested]);
    }
    let opts = options(&[("a", shared), ("b", nested)]);
    assert_eq!(
        engine.compile("1").unwrap().run(opts).unwrap_err().kind,
        ErrorKind::Recursion
    );
}
