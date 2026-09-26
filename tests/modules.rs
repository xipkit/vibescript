mod common;

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
    run_on(Engine::new(), source)
}

/// Runs a program that writes fields into builtin namespaces or modules,
/// reads enclosing bindings from a module body, or calls a module held as
/// an `any` value. Static types refuse each of these
/// (`static_types_refuse_dynamic_namespace_access`), so these programs run
/// without them.
fn run_dynamic(source: &str) -> serde_json::Value {
    run_on(common::gradual_engine(), source)
}

fn run_on(engine: Engine, source: &str) -> serde_json::Value {
    let output = engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    json(&output.value)
}

#[test]
fn static_types_refuse_dynamic_namespace_access() {
    for (source, code, at) in [
        ("Math[\"probe\"]=7", "V0112", "Math["),
        ("Math.probe=7", "V0203", "probe="),
        (
            "module M\n data=1\nend\nM.data=\"a\".match(/(a)/)",
            "V0203",
            "data=\"",
        ),
        ("module M\nend\nM.respond_to?(:x)", "V0405", "respond_to?"),
        (
            "module M\n def self.f -> int;1;end\nend\ndef read(m: any) -> int;m.f;end",
            "V0106",
            "f;end",
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error)[0], code, "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.rfind(at).unwrap(),
            "{source}"
        );
    }
}

#[test]
fn builtin_namespace_writes_do_not_require_an_unrelated_read() {
    // A separate builtin read would register the fallback and hide the regression.
    for builtin in [
        "Hash", "Regexp", "Regex", "Time", "Duration", "JSON", "Math",
    ] {
        for assignment in [
            format!("{builtin}[:probe]=7"),
            format!("{builtin}.probe=7"),
            format!("{builtin}[:probe]=[1];{builtin}::probe[0]=7"),
        ] {
            for kind in ["module", "class"] {
                for source in [
                    format!("{kind} M;Result=begin;{assignment};end;end;M::Result"),
                    format!("{kind} M;def self.write;{assignment};end;end;M.write"),
                ] {
                    for unused in [String::new(), format!(";def unused;{builtin};end")] {
                        let source = source.clone() + &unused;
                        let script = common::gradual_engine()
                            .compile(&source)
                            .unwrap_or_else(|error| panic!("{source}: {error}"));
                        let result = script
                            .run(CallOptions::default())
                            .unwrap_or_else(|error| panic!("{source}: {error}"));
                        assert_eq!(result.value.as_int(), Some(7), "{source}");
                    }
                }
            }
        }
    }
}

#[test]
fn builtin_namespace_writes_cover_compound_nested_and_block_addresses() {
    for body in [
        "Math[:probe] ||= 7",
        "Math[:probe]=1;Math[:probe] &&= 7",
        "Math[:probe]=2;Math[:probe] += 5",
        "Math.probe=[2];Math.probe[0] += 5",
        "Math[:probe]={items:[2]};Math[:probe][:items][0] += 5",
        "Math[:probe]=[2];Math::probe[0] += 5",
        "[1].map{|n|Math[:probe]=n+6}.first",
    ] {
        let source = format!("module M;Result=begin;{body};end;end;M::Result");
        let script = common::gradual_engine()
            .compile(&source)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            script
                .run(CallOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .value
                .as_int(),
            Some(7),
            "{source}"
        );
    }
    assert_eq!(
        run_dynamic("module M;Math[:left],JSON[:right]=[3,7];end;0"),
        serde_json::json!(0)
    );
    assert_eq!(
        run_dynamic("module M;def self.write;Math[:probe]=7;end;Result=write;end;M::Result"),
        serde_json::json!(7)
    );
}

#[test]
fn builtin_namespace_fallback_keeps_shadowing_and_per_call_isolation() {
    let script = common::gradual_engine()
        .compile("module M;def self.write;Math[:probe][0]+=5;end;end;def run;M.write;end")
        .unwrap();
    let original = Value::hash(vec![(b"probe".to_vec(), Value::array(vec![Value::int(2)]))]);
    let options = CallOptions {
        globals: [("Math".into(), original.clone())].into(),
        ..CallOptions::default()
    };
    for _ in 0..3 {
        assert_eq!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        assert_eq!(json(&original), serde_json::json!({"probe":[2]}));
    }
    let script = common::gradual_engine().compile("module M;Math={probe:[2]};def self.write;Math[:probe][0]+=5;end;end;def run;M.write;end").unwrap();
    let options = CallOptions {
        globals: [("Math".into(), Value::bytes(vec![b'x'; 128 * 1024]))].into(),
        limits: Limits {
            memory_bytes: Some(32 * 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    for _ in 0..3 {
        assert_eq!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
    assert_eq!(
        run(
            "module Math;Items=[2];end;module M;Result=begin;Math::Items[0]=Math::Items.fetch(0)+5;end;end;[M::Result,Math::Items]"
        ),
        serde_json::json!([7, [7]])
    );
}

#[test]
fn builtin_namespace_updates_obey_limits_and_release_temporary_state() {
    let script = common::gradual_engine()
        .compile("module M;64.times{Math[:items]='x'*512};end;def run;0;end")
        .unwrap();
    let baseline = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in [0, 1, 8, 15, 16] {
            let options = CallOptions {
                limits: Limits {
                    steps: (kind == ErrorKind::Steps).then_some(baseline.stats.steps * sample / 16),
                    memory_bytes: (kind == ErrorKind::Memory)
                        .then_some(baseline.stats.peak_memory_bytes * sample as usize / 16),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            let result = script.call("run", &[], options);
            if sample == 16 {
                assert_eq!(result.unwrap().stats.retained_memory_bytes, 0);
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
            }
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut options = CallOptions::default();
        if kind == ErrorKind::Cancelled {
            options.cancellation.cancel();
        } else {
            options.deadline = Some(std::time::Instant::now());
        }
        assert_eq!(script.call("run", &[], options).unwrap_err().kind, kind);
    }
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .stats
            .retained_memory_bytes,
        0
    );
}

#[test]
fn bodies_and_blocks_keep_their_assignment_boundaries() {
    assert_eq!(
        run_dynamic(
            "x=1\nC=7\nmodule M\n x=2\n [3].each{x=3}\n D=x\n E=C\n C=9\nend\n[x,C,M.C,M.D,M.E]"
        ),
        serde_json::json!([2, 7, 9, 2, 7])
    );
    assert_eq!(
        run_dynamic("x=[1]\nmodule M\n [2].each{x.push(2)}\n C=x\nend\n[x,M.C]"),
        serde_json::json!([[1, 2], [1, 2]])
    );
    assert_eq!(
        run_dynamic("module M\n C=N.C+1\n module N\n C=2\n end\nend\n[M.C,M::N::C]"),
        serde_json::json!([3, 2])
    );
    assert_eq!(
        run_dynamic("module M\n return 7\n C=1\nend\n9"),
        serde_json::json!(9)
    );
    for body in ["[2].each{x+=1}", "[2].each{return 7}"] {
        let source = format!("x=1\nmodule M\n{body}\nend\n9");
        assert!(
            common::gradual_engine()
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
            common::gradual_engine()
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
        run(
            "module M\n @@a: array<int> = []\n def self.f -> array<int>;@@a.push(1);@@a;end\nend\n[M.f,M.f]"
        ),
        serde_json::json!([[1], [1, 1]])
    );
    assert_eq!(
        run_dynamic("module M\n A=[]\nend\nx=[1];M.A=x;M::A[0]=3;[x,M.A]"),
        serde_json::json!([[1], [3]])
    );
    assert_eq!(
        run_dynamic(
            "module M\n A=[1]\nend\nx=M.dup;x.C=3;M.cycle=M;[x==M,M.C,M.cycle.cycle==M,\"#{M}\"]"
        ),
        serde_json::json!([true, 3, true, "<Class M>"])
    );
    assert_eq!(
        run_dynamic(
            "module M\n A=[1]\nend\nM.A[0]=2;M.A.push(3);M::A.push(4);before=M.A;M::A[0]=9;[before,M.A]"
        ),
        serde_json::json!([[1], [9]])
    );
}

#[test]
fn methods_preserve_visibility_binding_blocks_and_setter_results() {
    assert_eq!(
        run_dynamic(
            r#"
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
"#
        ),
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
            common::gradual_engine()
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
        .compile("module M\n @@n: int=0\n def self.advance -> int;@@n+=1;@@n;end\nend\ndef run -> int;M.advance;end")
        .unwrap();
    common::scope(|scope| {
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
    let script = Engine::new().compile("module M\n A=[]\n @@a: any=nil\n def self.work\n  400.times{@@a=\"x\"*2048}\n  @@a=self\n  nil\n end\nend\ndef run;M.work;end").unwrap();
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
    // Returning from a module body is refused at compile time; breaking out
    // of it and dividing by zero fail when it initializes.
    for body in ["[1].each{return 7}", "break 7", "C=1//0"] {
        let source = format!("module M\n{body}\neffect()\nend\ndef run;effect();end");
        assert!(
            engine
                .compile(&source)
                .and_then(|script| script.call("run", &[], CallOptions::default()))
                .is_err()
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn escaped_namespace_metadata_is_accounted_and_dispatches_original_code() {
    let script = common::gradual_engine()
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
    let foreign = common::gradual_engine()
        .compile("module N\n def self.f;99;end\nend\ndef read(m);m.f;end")
        .unwrap();
    let result = foreign
        .call("read", &[output.value], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    assert_eq!(result.stats.retained_memory_bytes, 0);
}
