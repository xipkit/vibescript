use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn run(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    json(&output.value)
}

#[test]
fn bodies_and_blocks_keep_their_assignment_boundaries() {
    assert_eq!(
        run("x=1\nC=7\nmodule M\n x=2\n [3].each{x=3}\n D=x\n E=C\n C=9\nend\n[x,C,M.C,M.D,M.E]"),
        serde_json::json!([2, 7, 9, 2, 7])
    );
    assert_eq!(
        run("x=[1]\nmodule M\n [2].each{x.push(2)}\n C=x\nend\n[x,M.C]"),
        serde_json::json!([[1, 2], [1, 2]])
    );
    assert_eq!(
        run("module M\n C=N.C+1\n module N\n C=2\n end\nend\n[M.C,M::N::C]"),
        serde_json::json!([3, 2])
    );
    assert_eq!(
        run("module M\n return 7\n C=1\nend\n9"),
        serde_json::json!(9)
    );
    for body in ["[2].each{x+=1}", "[2].each{return 7}"] {
        let source = format!("x=1\nmodule M\n{body}\nend\n9");
        assert!(
            Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .is_err()
        );
    }
}

#[test]
fn namespace_identity_and_collection_values_have_distinct_mutation_rules() {
    for receiver in ["M.data", "M::data", "M.data.dup"] {
        let source = format!(
            "module M\n data=1\nend\nM.data=\"a\".match(/(a)/);{receiver}.captures.push(\"x\")"
        );
        assert_eq!(
            Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument,
            "{receiver}"
        );
    }
    assert_eq!(
        run("module M\n @@a=[]\n def self.f;@@a.push(1);@@a;end\nend\n[M.f,M.f]"),
        serde_json::json!([[1], [1, 1]])
    );
    assert_eq!(
        run("module M\n A=[]\nend\nx=[1];M.A=x;M::A[0]=3;[x,M.A]"),
        serde_json::json!([[1], [3]])
    );
    assert_eq!(
        run("module M\n A=[1]\nend\nx=M.dup;x.C=3;M.cycle=M;[x==M,M.C,M.cycle.cycle==M,\"#{M}\"]"),
        serde_json::json!([true, 3, true, "<Class M>"])
    );
    assert_eq!(
        run(
            "module M\n A=[1]\nend\nM.A[0]=2;M.A.push(3);M::A.push(4);before=M.A;M::A[0]=9;[before,M.A]"
        ),
        serde_json::json!([[1], [9]])
    );
}

#[test]
fn methods_preserve_visibility_binding_blocks_and_setter_results() {
    assert_eq!(
        run(r#"
module M
 C=4
 private def self.hidden;7;end
 public def self.check
  [hidden,respond_to?(:hidden),self.respond_to?(:hidden),self.respond_to?(:hidden,true)]
 end
 def self.apply(x: int, extra: 2)
  yield(x+C+extra)
 end
 def self.value=(x: int)
  @@value=x+1
  99
 end
end
def assign
 M.value=5
end
[M.check,M.apply(3,extra:1){|x|x*2},assign,M.value]
"#),
        serde_json::json!([[7, true, false, true], 16, 5, 6])
    );
    for expression in [
        "M.hidden",
        "M::hidden",
        "M.apply(false){1}",
        "M.respond_to?(:hidden,1)",
        "M.equal?(M){1}",
    ] {
        let source = format!(
            "module M\n private def self.hidden;7;end\n public def self.apply(x: int);yield(x);end\nend\n{expression}"
        );
        assert!(
            Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{expression}"
        );
    }
}

#[test]
fn initializers_run_once_per_call_and_release_their_state() {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    let script = engine
        .compile("module M\n C=effect()\nend\ndef run\n M.C;M.C;nil\nend")
        .unwrap();
    for _ in 0..4 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(output.stats.retained_memory_bytes, 0);
    }
    assert_eq!(count.load(Ordering::SeqCst), 4);
    let script = engine
        .compile("module M\n @@n=0\n def self.advance;@@n+=1;@@n;end\nend\ndef run;M.advance;end")
        .unwrap();
    std::thread::scope(|scope| {
        let jobs = (0..8)
            .map(|_| scope.spawn(|| script.call("run", &[], CallOptions::default()).unwrap()))
            .collect::<Vec<_>>();
        for job in jobs {
            assert_eq!(job.join().unwrap().value.as_int(), Some(1));
        }
    });
}

#[test]
fn namespace_fields_are_accounted_without_retaining_cycles_or_replaced_arrays() {
    let script = Engine::new().compile("module M\n A=[]\n def self.work\n  400.times{@@a=\"x\"*2048}\n  @@a=self\n  nil\n end\nend\ndef run;M.work;end").unwrap();
    for _ in 0..4 {
        let output = script
            .call(
                "run",
                &[],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(65536),
                        steps: None,
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(output.stats.retained_memory_bytes, 0);
        assert!(output.stats.peak_memory_bytes < 65536);
    }
    for (limits, expected) in [
        (
            Limits {
                steps: Some(20),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
        (
            Limits {
                memory_bytes: Some(1024),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        assert_eq!(
            script
                .call(
                    "run",
                    &[],
                    CallOptions {
                        limits,
                        ..CallOptions::default()
                    }
                )
                .unwrap_err()
                .kind,
            expected
        );
    }
}

#[test]
fn initialization_observes_cancellation_and_stops_later_effects() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let mut engine = Engine::new();
    engine.register("cancel_now", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    engine.register("effect", move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("module M\n cancel_now()\n effect()\nend\ndef run;effect();end")
        .unwrap();
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                cancellation: token,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    for body in ["[1].each{return 7}", "break 7", "C=1/0"] {
        let source = format!("module M\n{body}\neffect()\nend\ndef run;effect();end");
        assert!(
            engine
                .compile(&source)
                .unwrap()
                .call("run", &[], CallOptions::default())
                .is_err()
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn escaped_namespace_metadata_is_accounted_and_cannot_dispatch_foreign_code() {
    let script = Engine::new()
        .compile("module M\n C=1\n def self.f;C;end\nend\ndef make;M;end\ndef read(m);m.f;end")
        .unwrap();
    let output = script.call("make", &[], CallOptions::default()).unwrap();
    assert_eq!(size_of::<Value>(), 16);
    assert!(output.stats.retained_memory_bytes > 0);
    assert_eq!(output.value.type_name(), "class");
    assert_eq!(
        script
            .call(
                "read",
                std::slice::from_ref(&output.value),
                CallOptions::default()
            )
            .unwrap()
            .value
            .as_int(),
        Some(1)
    );
    drop(script);
    let foreign = Engine::new()
        .compile("module N\n def self.f;99;end\nend\ndef read(m);m.f;end")
        .unwrap();
    let error = foreign
        .call("read", &[output.value], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert!(error.message.contains("different compiled script"));
}
