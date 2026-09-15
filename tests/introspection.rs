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
fn core_predicates_use_strict_types_and_exact_class_identity() {
    assert_eq!(
        result(
            "class C\nend\nmodule M\nend\nc=C.new;[c.is_a?(C),c.kind_of?(C),c.instance_of?(C),c.is_a?(M),C.is_a?(C),1.is_a?(C),1.is_type?(:int),1.is_type?(:float),\"1\".is_type?(:int),9223372036854775808.is_type?(:int),JSON.is_type?(:hash),nil.is_type?(\"int?\")]"
        ),
        serde_json::json!([
            true, true, true, false, false, false, true, false, false, true, true, true
        ])
    );
}

#[test]
fn responding_checks_privacy_without_invoking_methods() {
    assert_eq!(
        result(
            "class C\nproperty p\nprivate def hidden\nraise \"invoked\"\nend\nprotected def guarded\nraise \"invoked\"\nend\ndef check\n[respond_to?(:hidden),self.respond_to?(:hidden),self.respond_to?(:guarded),respond_to?(:guarded,false)]\nend\nend\nc=C.new;c.data=JSON::parse;c.tap=3;[c.respond_to?(:p),c.respond_to?(\"p=\"),c.respond_to?(:data),c.respond_to?(:hidden),c.respond_to?(:hidden,true),c.respond_to?(:guarded,true),c.respond_to?(:tap),c.check]"
        ),
        serde_json::json!([
            true,
            true,
            false,
            false,
            true,
            true,
            false,
            [true, false, false, true]
        ])
    );
}

#[test]
fn hash_data_and_callable_exports_follow_member_lookup() {
    assert_eq!(
        result(
            "h={keys:1,run:JSON::parse,tap:3};JSON[:keys]=1;JSON[:is_type?]=JSON::parse;[h.respond_to?(:keys),h.respond_to?(:run),h.respond_to?(:tap),JSON.respond_to?(:keys),JSON.respond_to?(:parse),JSON.is_type?(\"[3]\"),{\"is_type?\":7}.is_type?(:hash),JSON::parse.respond_to?(:equal?),JSON::parse.respond_to?(:call)]"
        ),
        serde_json::json!([true, true, false, false, true, [3], true, true, false])
    );
    assert_eq!(
        result(
            "class C\nend\nc=C.new;c.tap=JSON::parse;C.tap=JSON::parse;[c.respond_to?(:tap),C.respond_to?(:tap)]"
        ),
        serde_json::json!([true, true])
    );
}

#[test]
fn named_atoms_resolve_in_the_active_lexical_scope() {
    assert_eq!(
        result(
            "class C\nend\nclass D\nend\nenum E\nA\nend\nJSON[:E]=E;v=C.new;C=D;[v.is_type?(:C),nil.is_type?(\"C?\"),[v,:C].reduce(:is_type?),[nil,\"C?\"].reduce(:is_type?),[1].map {nil.is_type?(\"C?\")},E::A.is_type?(\"JSON.E\"),nil.is_type?(\"JSON.E?\"),E.is_type?(:E),:a.is_type?(:E)]"
        ),
        serde_json::json!([true, false, true, false, [false], true, true, false, false])
    );
    assert_eq!(
        result(
            "class User\nend\n[User.new.is_type?(:USER),nil.is_type?(\"USER?\"),nil.is_type?(\"User?\"),nil.is_type?(\"Missing?\")]"
        ),
        serde_json::json!([false, false, true, false])
    );
}

#[test]
fn parenthesized_and_rescue_selected_predicates_keep_their_call_rules() {
    assert_eq!(
        result(
            "class C\nend\n[(C.new.is_a?)(C),(C.new.is_a? rescue JSON::parse)(C),(1.is_type?)(:nil),(1.is_type? rescue JSON::parse)(:nil),(1.respond_to?)(:odd?),(1.respond_to? rescue JSON::parse)(:odd?)]"
        ),
        serde_json::json!([true, false, false, true, true, false])
    );
}

#[test]
fn namespace_writes_in_methods_rebind_the_visible_receiver() {
    for (declaration, method, receiver) in [
        ("class C", "check", "C.new"),
        ("class C", "self.check", "C"),
        ("module C", "self.check", "C"),
    ] {
        let source = format!(
            "enum E\nA\nend\n{declaration}\ndef {method}\nJSON[:E]=E;[JSON.key?(:E),nil.is_type?(\"JSON.E?\"),[nil,\"JSON.E?\"].reduce(:is_type?)]\nend\nend\n{receiver}.check"
        );
        assert_eq!(
            result(&source),
            serde_json::json!([true, true, true]),
            "{source}"
        );
        let source = format!(
            "{declaration}\nJSON={{items:[1]}}\ndef {method}\nJSON.items.push(2);JSON[:items]\nend\nend\n[{receiver}.check,JSON.key?(:items)]"
        );
        assert_eq!(
            result(&source),
            serde_json::json!([[1, 2], false]),
            "{source}"
        );
    }
}

#[test]
fn symbolic_reductions_use_script_methods_and_predicate_overrides() {
    assert_eq!(
        result(
            "class C\nproperty value\ndef initialize\n@value=1\nend\ndef add(x)\n@value+=x;self\nend\ndef is_type?(x)\nx+10\nend\nend\n[[C.new,2,3].reduce(:add).value,[C.new,7].reduce(:is_type?),[C.new,C].reduce(:is_a?),[1,:odd?].reduce(:respond_to?),[[1],2,3].reduce(:push)]"
        ),
        serde_json::json!([6, 17, true, true, [1, 2, 3]])
    );
    assert_eq!(
        result("class C\ndef accept(x:int) -> int\nx\nend\nend\n[C.new,3].reduce(:accept)"),
        serde_json::json!(3)
    );
    let error = Engine::new()
        .compile("class C\nprivate def hidden(x)\nx\nend\nend\n[C.new,1].reduce(:hidden)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
}

#[test]
fn invalid_predicates_validate_arguments_before_blocks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for name in [
        "respond_to?",
        "is_a?",
        "kind_of?",
        "instance_of?",
        "is_type?",
    ] {
        for (suffix, message) in [
            (
                "(x:1) {entered()}".to_owned(),
                format!("{name} does not take keyword arguments"),
            ),
            (
                "(1) {entered()}".to_owned(),
                format!("{name} does not take a block"),
            ),
            (
                String::new(),
                format!(
                    "{name} expects {}",
                    if name == "respond_to?" {
                        "1 or 2 arguments"
                    } else {
                        "exactly one argument"
                    }
                ),
            ),
        ] {
            for receiver in ["1", "C.new", "C", "JSON::parse", "{x:int}"] {
                let source = format!("class C\nend\n{receiver}.{name}{suffix}");
                let error = engine
                    .compile(&source)
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err();
                assert_eq!(error.message, message, "{source}");
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn raw_type_atoms_have_bounded_errors_and_preserve_byte_quoting() {
    let script = Engine::new()
        .compile("def run(atom)\nnil.is_type?(atom)\nend")
        .unwrap();
    for (bytes, message) in [
        (
            vec![0xff],
            "is_type? supports type atoms only, got \"\\xff\"",
        ),
        (
            b"int\0".to_vec(),
            "is_type? supports type atoms only, got \"int\\x00\"",
        ),
        (
            b"int\n".to_vec(),
            "is_type? supports type atoms only, got \"int\\n\"",
        ),
        (
            vec![b'a'; 257],
            "is_type? supports type atoms only, got 257 bytes",
        ),
        (
            vec![b'.'; 1 << 20],
            "is_type? supports type atoms only, got 1048576 bytes",
        ),
    ] {
        let error = script
            .call("run", &[Value::bytes(bytes)], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, message);
    }
    assert_eq!(
        script
            .call(
                "run",
                &[Value::bytes("A".repeat(256))],
                CallOptions::default()
            )
            .unwrap()
            .value
            .to_string(),
        "false"
    );
    let error = script
        .call(
            "run",
            &[Value::bytes("JSON.Missing?")],
            CallOptions::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.message,
        "unknown type atom \"JSON.Missing?\" in is_type?"
    );
}

#[test]
fn callable_hash_entries_keep_raw_and_chunked_method_names() {
    let script = Engine::new().compile("def run(key)\nh={};h[key]=JSON::parse;JSON[key]=JSON::parse;[h.respond_to?(key),[h,\"[3]\"].reduce(key),JSON.respond_to?(key),[JSON,\"[4]\"].reduce(key),1.respond_to?(key)]\nend").unwrap();
    for key in [
        vec![0xff],
        vec![0xc3],
        b"\0".to_vec(),
        [vec![b'x'; 4095], "é".as_bytes().to_vec(), vec![b'y'; 4096]].concat(),
        [vec![b'x'; 8191], vec![0xff]].concat(),
    ] {
        let args = [Value::bytes(key)];
        let output = script.call("run", &args, CallOptions::default()).unwrap();
        assert_eq!(output.value.to_string(), "[true, [3], true, [4], false]");
        assert!(output.stats.retained_memory_bytes < 2048);
        let mut options = CallOptions::default();
        options.limits.steps = Some(output.stats.steps);
        options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
        script.call("run", &args, options.clone()).unwrap();
        options.limits.steps = Some(output.stats.steps - 1);
        assert_eq!(
            script.call("run", &args, options).unwrap_err().kind,
            ErrorKind::Steps
        );
    }
}

#[test]
fn introspection_and_symbolic_calls_enforce_exact_budgets() {
    let script=Engine::new().compile("class C\nend\ndef run(name)\nbegin\n[\"x\".respond_to?(name),C.respond_to?(name),nil.is_type?(\"C?\"),[C.new,C].reduce(:is_a?)]\nrescue LimitError | RuntimeError\nraise \"caught quota\"\nend\nend").unwrap();
    let args = [Value::bytes(vec![b'z'; 64 << 10])];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.to_string(), "[false, false, true, true]");
    assert!(output.stats.steps >= (64 << 10) / 32);
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
fn cancellation_in_a_symbolic_method_prevents_later_effects() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    engine.register("after", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script=engine.compile("class C\ndef apply(x)\nstop();after()\nend\nend\nbegin\n[C.new,1].reduce(:apply);after()\nrescue RuntimeError | LimitError\nafter()\nensure\nafter()\nend").unwrap();
    let error = script
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
