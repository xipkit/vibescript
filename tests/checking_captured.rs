use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ModuleConfig, Script, Value};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "captured-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![self.0.clone()],
                ..Default::default()
            })
            .unwrap();
        engine
    }

    fn write(&self, name: &str, source: &str) {
        fs::write(self.0.join(name), source).unwrap();
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn accepts(script: &Script, args: &[Value], options: &CallOptions) {
    for _ in 0..2 {
        assert_eq!(
            script
                .call("run", args, options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        let report = script.check_call("run", args, options).unwrap();
        assert!(report.is_clean(), "{report:?}");
    }
}

#[test]
fn supplied_instances_preserve_aliases_fields_and_call_isolation() {
    let source = Engine::new()
        .compile("class Box;property n:int;def initialize;@n=5;end;end;def make;Box.new;end")
        .unwrap();
    let value = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile("def run(a,b)->int;a.n+=1;if a==b && b.n==6;7;else;false;end;end")
        .unwrap();
    accepts(&receiver, &[value.clone(), value], &CallOptions::default());
}

#[test]
fn captured_namespace_state_survives_without_repeating_initializers() {
    let files = Files::new();
    files.write("counter.vibe", "module Counter;N=0;module Nested;N=3;end;def self.bump;N+=1;end;def self.value;N;end;end;def counter;Counter;end");
    let producer = files
        .engine()
        .compile("def make;c=require(:counter).counter();c.bump;c;end")
        .unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile("def run(c)->int;c.bump;if c.value==2 && c::Nested::N==3;7;else;false;end;end")
        .unwrap();
    accepts(&receiver, &[value], &CallOptions::default());
}

#[test]
fn captures_of_one_source_keep_distinct_class_identity_and_state() {
    let files = Files::new();
    files.write(
        "counter.vibe",
        "class Counter;N=0;def self.bump;N+=1;end;def self.value;N;end;end;def counter;Counter;end",
    );
    let producer = files
        .engine()
        .compile("def make;require(:counter).counter();end")
        .unwrap();
    let a = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let b = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(a,b,again)->int;a.bump;again.bump;if a != b && a==again && a.value==2 && b.value==0;7;else;false;end;end").unwrap();
    accepts(&receiver, &[a.clone(), b, a], &CallOptions::default());
}

#[test]
fn cyclic_instance_graphs_keep_nested_container_aliases() {
    let source = Engine::new().compile("class Node;property links;property n:int;def initialize;@n=1;@links=[];end;end;def make;a=Node.new;b=Node.new;a.links=[b];b.links=[a];a;end").unwrap();
    let value = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(a)->int;b=a.links[0];b.links[0].n=4;if a.n==4 && b.n==1 && b.links[0]==a;7;else;false;end;end").unwrap();
    accepts(&receiver, &[value], &CallOptions::default());
}

#[test]
fn lazy_instance_globals_do_not_replace_already_mutated_objects() {
    let source = Engine::new().compile("class Box;property n:int;def initialize(n=1);@n=n;end;end;def make;[Box.new(3),Box.new(5)];end").unwrap();
    let values = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let args = values.as_array().unwrap();
    let options = CallOptions {
        globals: [("other".into(), args[1].clone())].into(),
        ..Default::default()
    };
    let receiver = Engine::new().compile("def run(a)->int;a.n=4;b=a.class.new(8);other.n=6;if a.n==4 && b.n==8 && other.n==6;7;else;false;end;end").unwrap();
    accepts(&receiver, &[args[0].clone()], &options);
}

fn traced_namespace(marker: i64) -> Value {
    let source = Engine::new()
        .compile(&format!(
            "class Remote;trace.push({marker});def self.value;7;end;end;def make;Remote;end"
        ))
        .unwrap();
    source
        .call(
            "make",
            &[],
            CallOptions {
                globals: [("trace".into(), Value::array(vec![]))].into(),
                ..Default::default()
            },
        )
        .unwrap()
        .value
}

#[test]
fn foreign_initializers_follow_input_order_before_the_receiving_script() {
    let receiver = Engine::new().compile(
        "class Local;trace.push(3);end;def run(input)->int;if trace.length==3 && trace[0]==1 && trace[1]==2 && trace[2]==3;7;else;false;end;end"
    ).unwrap();
    let options = CallOptions {
        globals: [("trace".into(), Value::array(vec![]))].into(),
        ..Default::default()
    };
    let a = traced_namespace(1);
    let b = traced_namespace(2);
    for value in [
        Value::array(vec![a.clone(), b.clone()]),
        Value::object(vec![(b"z".to_vec(), a), (b"a".to_vec(), b)]),
    ] {
        accepts(&receiver, &[value], &options);
    }
}

#[test]
fn duplicate_keywords_keep_the_runtime_source_admission_order() {
    let files = Files::new();
    files.write("saved.vibe", "class Saved;end;def saved;Saved;end");
    let scoped = files
        .engine()
        .compile("def make;require(:saved).saved();end")
        .unwrap()
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [("trace".into(), Value::array(vec![]))].into(),
        ..Default::default()
    };
    for captured in [false, true] {
        let first = traced_namespace(1);
        let first = if captured {
            Value::array(vec![first, scoped.clone()])
        } else {
            first
        };
        let keywords = [
            ("a".into(), first),
            ("b".into(), traced_namespace(2)),
            ("a".into(), traced_namespace(3)),
        ];
        let expected = if captured {
            "trace.length==2 && trace[0]==3 && trace[1]==2"
        } else {
            "trace.length==3 && trace[0]==1 && trace[1]==2 && trace[2]==3"
        };
        let script = Engine::new()
            .compile(&format!(
                "def run(a:,b:)->int;if {expected};7;else;false;end;end"
            ))
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                script
                    .call_with_keywords("run", &[], &keywords, options.clone())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
            let report = script
                .check_call_with_keywords("run", &[], &keywords, &options)
                .unwrap();
            assert!(report.is_clean(), "{captured}: {report:?}");
        }
    }
}

#[test]
fn deferred_sources_initialize_at_the_read_and_unused_sources_stay_unread() {
    let receiver = Engine::new().compile(
        "def run->int;trace.push(0);a.value;trace.push(2);a.value;if trace.length==3 && trace[0]==0 && trace[1]==1 && trace[2]==2;7;else;false;end;end"
    ).unwrap();
    let options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("a".into(), traced_namespace(1)),
            ("unused".into(), traced_namespace(99)),
        ]
        .into(),
        ..Default::default()
    };
    accepts(&receiver, &[], &options);
}

#[test]
fn captured_private_bindings_are_read_without_reexecuting_the_file() {
    let files = Files::new();
    files.write("saved.vibe", "seed=4;class Saved;def self.read;seed;end;def self.bump;seed+=1;end;end;seed+=1;def saved;Saved;end");
    let producer = files
        .engine()
        .compile("def make;require(:saved).saved();end")
        .unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile("def run(c)->int;c.bump;if c.read==6;7;else;false;end;end")
        .unwrap();
    accepts(&receiver, &[value], &CallOptions::default());
}

#[test]
fn alternate_lazy_roots_keep_every_nested_source_activation() {
    let child = Engine::new()
        .compile("class Child;trace.push(2);end;def make;Child.new;end")
        .unwrap();
    let options = CallOptions {
        globals: [("trace".into(), Value::array(vec![]))].into(),
        ..Default::default()
    };
    let child = child.call("make", &[], options.clone()).unwrap().value;
    let parent = Engine::new().compile("class Parent;trace.push(1);property child;end;def make(child);p=Parent.new;p.child=child;p;end").unwrap();
    let parent = parent.call("make", &[child], options).unwrap().value;
    let options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("left".into(), parent.clone()),
            ("right".into(), parent),
        ]
        .into(),
        ..Default::default()
    };
    let receiver = Engine::new().compile("def run(flag:bool)->int;if flag;left;else;right;end;if trace.length==2 && trace[0]==1 && trace[1]==2;7;else;false;end;end").unwrap();
    for flag in [false, true] {
        accepts(&receiver, &[Value::boolean(flag)], &options);
    }
    let report = receiver.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn failed_source_activation_is_catchable_and_does_not_repeat() {
    let producer = Engine::new().compile("class Remote;trace.push(1);if fail;raise(\"boom\");end;def self.value;7;end;end;def make;Remote;end").unwrap();
    let value = producer
        .call(
            "make",
            &[],
            CallOptions {
                globals: [
                    ("trace".into(), Value::array(vec![])),
                    ("fail".into(), Value::boolean(false)),
                ]
                .into(),
                ..Default::default()
            },
        )
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("fail".into(), Value::boolean(true)),
            ("remote".into(), value),
        ]
        .into(),
        ..Default::default()
    };
    let receiver = Engine::new().compile("def run->int;n=0;begin;remote.value;rescue;n+=1;end;begin;remote.value;rescue;n+=1;end;if n==2 && trace.length==1 && trace[0]==1;7;else;false;end;end").unwrap();
    accepts(&receiver, &[], &options);
}

#[test]
fn aliases_published_by_a_failed_initializer_keep_the_source_failure() {
    let producer = Engine::new().compile("class Remote;N=0;cache=Remote;if fail;raise(\"boom\");end;def self.value;0;end;end;def make;Remote;end").unwrap();
    let value = producer
        .call(
            "make",
            &[],
            CallOptions {
                globals: [
                    ("cache".into(), Value::nil()),
                    ("fail".into(), Value::boolean(false)),
                ]
                .into(),
                ..Default::default()
            },
        )
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [
            ("cache".into(), Value::nil()),
            ("fail".into(), Value::boolean(true)),
            ("remote".into(), value),
        ]
        .into(),
        ..Default::default()
    };
    for expression in [
        "cache.value",
        "cache::N",
        "cache.new",
        "cache.respond_to?(:value)",
        "cache.send(:value)",
        "cache.public_send(:value)",
    ] {
        let receiver = Engine::new().compile(&format!("def run->int;begin;remote;rescue;nil;end;v=begin;{expression};rescue;7;end;if v==7;7;else;false;end;end")).unwrap();
        accepts(&receiver, &[], &options);
    }
}

#[test]
fn declaration_inputs_can_alias_a_captured_global_in_both_mutation_directions() {
    for body in [
        "if a==other;false;else;7;end",
        "a.n=7;other.n=false;a.n",
        "other.n=7;a.n=false;other.n",
        "a.n=false;other.n",
    ] {
        let script = Engine::new().compile(&format!(
            "class Box;property n;def initialize;@n=0;end;end;def make;Box.new;end;def run(a:Box)->int;{body};end"
        )).unwrap();
        let value = script
            .call("make", &[], CallOptions::default())
            .unwrap()
            .value;
        let options = CallOptions {
            globals: [("other".into(), value.clone())].into(),
            ..Default::default()
        };
        assert!(
            script.call("run", &[value], options.clone()).is_err(),
            "{body}"
        );
        let report = script.check_function("run", &options).unwrap();
        assert!(report.incomplete.is_empty(), "{body}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{body}: {report:?}");
    }
}

#[test]
fn foreign_property_type_lookup_initializes_a_deferred_namespace() {
    let producer = Engine::new()
        .compile("class Holder;property item:C?;end;def make;Holder.new;end")
        .unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let options = CallOptions {
        globals: [
            ("C".into(), traced_namespace(1)),
            ("trace".into(), Value::array(vec![])),
        ]
        .into(),
        ..Default::default()
    };
    let receiver = Engine::new().compile("def run(holder)->int;holder.item=nil;if trace.length==1 && trace[0]==1;7;else;false;end;end").unwrap();
    accepts(&receiver, &[value], &options);
}

#[test]
fn host_signature_lookups_initialize_deferred_sources_without_executing_the_host() {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature, SignatureParam};
    for result_type in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let mut engine = Engine::new();
        engine.register_method(
            "probe",
            HostMethod::new("probe", move |_, _, _| {
                called.fetch_add(1, Ordering::Relaxed);
                Ok(Value::nil())
            })
            .with_signature(Signature {
                params: if result_type {
                    vec![]
                } else {
                    vec![SignatureParam {
                        name: "value".into(),
                        ty: "C?".into(),
                        optional: false,
                    }]
                },
                result: if result_type { "C?" } else { "nil" }.into(),
                ..Default::default()
            })
            .unwrap(),
        );
        let probe = if result_type { "probe()" } else { "probe(nil)" };
        let script = engine
            .compile(&format!(
                "def run->int;{probe};if trace.length==1 && trace[0]==1;7;else;false;end;end"
            ))
            .unwrap();
        let options = CallOptions {
            globals: [
                ("C".into(), traced_namespace(1)),
                ("trace".into(), Value::array(vec![])),
            ]
            .into(),
            ..Default::default()
        };
        accepts(&script, &[], &options);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn host_block_boundaries_initialize_deferred_signature_sources() {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature, SignatureParam};
    for result_type in [false, true] {
        for rounds in [0, 1, 2] {
            for body in ["nil", "break nil"] {
                let calls = Arc::new(AtomicUsize::new(0));
                let called = calls.clone();
                let mut engine = Engine::new();
                let method = HostMethod::new_with_block("probe", move |call, _, _| {
                    called.fetch_add(1, Ordering::Relaxed);
                    for _ in 0..rounds {
                        call.call_block(&[])?;
                    }
                    Ok(Value::nil())
                })
                .with_signature(Signature {
                    params: if result_type {
                        vec![]
                    } else {
                        vec![SignatureParam {
                            name: "value".into(),
                            ty: "C?".into(),
                            optional: false,
                        }]
                    },
                    result: if result_type { "C?" } else { "nil" }.into(),
                    accepts_block: true,
                })
                .unwrap();
                engine.register_method("probe", method);
                let args = if result_type { "" } else { "nil" };
                let script = engine.compile(&format!("def run->int;probe({args}){{{body}}};if trace.length==1 && trace[0]==1;7;else;false;end;end")).unwrap();
                let options = CallOptions {
                    globals: [
                        ("C".into(), traced_namespace(1)),
                        ("trace".into(), Value::array(vec![])),
                    ]
                    .into(),
                    ..Default::default()
                };
                accepts(&script, &[], &options);
                assert_eq!(calls.load(Ordering::Relaxed), 2);
            }
        }
    }
}
