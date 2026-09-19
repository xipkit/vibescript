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
fn deferred_source_initializers_preserve_alternative_import_histories() {
    let files = Files::new();
    files.write("left.vibe", "trace.push(1);def value;1;end");
    files.write("right.vibe", "trace.push(2);def value;2;end");
    let producer = files.engine().compile("class Remote;if choose;require(:left);else;require(:right);end;def self.value;7;end;end;def make;Remote;end").unwrap();
    let mut options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("choose".into(), Value::boolean(false)),
        ]
        .into(),
        ..Default::default()
    };
    let remote = producer.call("make", &[], options.clone()).unwrap().value;
    options.globals.insert("remote".into(), remote);
    let receiver = files.engine().compile("def run(flag:bool)->int;choose=flag;remote.value;remote.value;if trace.length==1;7;else;false;end;end").unwrap();
    for flag in [false, true] {
        accepts(&receiver, &[Value::boolean(flag)], &options);
    }
    let report = receiver.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

fn branching_type_options(choice: bool) -> (CallOptions, std::sync::Arc<AtomicUsize>) {
    branching_type_options_with_initializer(choice, "")
}

fn branching_type_options_with_initializer(
    choice: bool,
    initializer: &str,
) -> (CallOptions, std::sync::Arc<AtomicUsize>) {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature};
    let choices = Arc::new(AtomicUsize::new(0));
    let called = choices.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "choose",
        HostMethod::new("choose", move |_, _, _| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(Value::boolean(choice))
        })
        .with_signature(Signature {
            params: vec![],
            result: "bool".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    let producer = engine.compile(&format!("class Remote;{initializer};trace.push(0);if choose();left;else;right;end;def self.value;7;end;end;def make;Remote;end")).unwrap();
    let mut options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("left".into(), traced_namespace(1)),
            ("right".into(), traced_namespace(2)),
        ]
        .into(),
        ..Default::default()
    };
    let remote = producer.call("make", &[], options.clone()).unwrap().value;
    options.globals.insert("C".into(), remote);
    choices.store(0, Ordering::Relaxed);
    (options, choices)
}

#[test]
fn deferred_property_initializers_preserve_writes_to_other_fields() {
    for choice in [false, true] {
        for body in [
            "@item=nil",
            "bind(nil)",
            "@items.push(nil)",
            "@items[0]=nil",
            "@items.fill{nil}",
            "@items << nil",
        ] {
            let (mut options, choices) =
                branching_type_options_with_initializer(choice, "if trace.length>0;box.note=9;end");
            let producer = Engine::new().compile(&format!("class Holder;property note;property item:C?;property items:array<C?>;def initialize;@note=1;@items=[nil];end;def bind(@item);end;def work;{body};end;end;def make;Holder.new;end")).unwrap();
            let holder = producer.call("make", &[], options.clone()).unwrap().value;
            options.globals.insert("box".into(), holder.clone());
            options
                .globals
                .insert("trace".into(), Value::array(vec![Value::int(5)]));
            choices.store(0, Ordering::Relaxed);
            let receiver = Engine::new().compile("def run(target)->int;target.work;if target.note==9 && box.note==9 && trace.length==3;7;else;false;end;end").unwrap();
            accepts(&receiver, &[holder], &options);
            assert_eq!(choices.load(Ordering::Relaxed), 2, "{body}");
        }
    }
}

#[test]
fn deferred_property_types_resume_direct_stores_and_completed_mutations() {
    for choice in [false, true] {
        for (body, valid, effects) in [
            (
                "@item=(begin;trace.push(9);nil;end)",
                "@item==nil && value==nil",
                1,
            ),
            (
                "bind(begin;trace.push(9);nil;end)",
                "@item==nil && value==nil",
                1,
            ),
            (
                "@items.push(begin;trace.push(9);nil;end)",
                "@items.length==3 && @items[2]==nil && value.length==3",
                1,
            ),
            (
                "@items[-1]=(begin;trace.push(9);nil;end)",
                "@items.length==2 && @items[1]==nil && value==nil",
                1,
            ),
            (
                "@items.fill{trace.push(9);nil}",
                "@items.length==2 && @items[0]==nil && @items[1]==nil && value.length==2",
                2,
            ),
            (
                "@items.delete_if{trace.push(9);true}",
                "@items.length==0 && value.length==0",
                2,
            ),
            (
                "@items.push(begin;trace.push(9);nil;end).push(nil)",
                "@items.length==3 && value.length==4",
                1,
            ),
            (
                "[1].map{@items.push(begin;trace.push(9);nil;end)}",
                "@items.length==3 && value.length==1 && value[0].length==3",
                1,
            ),
            (
                "@items << (begin;trace.push(9);nil;end)",
                "@items.length==3 && value.length==3",
                1,
            ),
            (
                "@items.fill{trace.push(9);break 7}",
                "@items.length==2 && value==7",
                1,
            ),
        ] {
            let (options, choices) = branching_type_options(choice);
            let producer = Engine::new().compile(&format!("class Holder;property item:C?;property items:array<C?>;def initialize;@items=[nil,nil];end;def bind(@item);end;def work->int;value=begin;{body};end;if {valid};7;else;false;end;end;end;def make;Holder.new;end")).unwrap();
            let holder = producer.call("make", &[], options.clone()).unwrap().value;
            choices.store(0, Ordering::Relaxed);
            let receiver = Engine::new().compile(&format!("def run(box)->int;n=box.work;C.value;if n==7 && trace.length=={} && trace[0]==9 && trace[{}]==0;7;else;false;end;end", effects + 2, effects)).unwrap();
            accepts(&receiver, &[holder], &options);
            assert_eq!(choices.load(Ordering::Relaxed), 2, "{body}");
        }
    }
}

#[test]
fn deferred_property_rejections_preserve_fields_and_cleanup_on_every_branch() {
    for choice in [false, true] {
        for body in [
            "@item=1",
            "bind(1)",
            "@items.push(1)",
            "@items[0]=1",
            "@items.fill{1}",
        ] {
            let (options, choices) = branching_type_options(choice);
            let producer = Engine::new().compile(&format!("class Holder;property item:C?;property items:array<C?>;def initialize;@items=[nil,nil];end;def bind(@item);end;def work->int;trace.push(9);begin;{body};false;rescue RuntimeError;if @item==nil && @items.length==2 && @items[0]==nil;7;else;false;end;ensure;trace.push(8);end;end;end;def make;Holder.new;end")).unwrap();
            let holder = producer.call("make", &[], options.clone()).unwrap().value;
            choices.store(0, Ordering::Relaxed);
            let receiver = Engine::new().compile("def run(box)->int;value=box.work;if value==7 && trace[0]==9 && trace.last==8;7;else;false;end;end").unwrap();
            for _ in 0..2 {
                let before = choices.load(Ordering::Relaxed);
                let report = receiver
                    .check_call("run", std::slice::from_ref(&holder), &options)
                    .unwrap();
                assert_eq!(choices.load(Ordering::Relaxed), before);
                assert!(report.incomplete.is_empty(), "{body}: {report:?}");
                assert_eq!(report.diagnostics.len(), 1, "{body}: {report:?}");
                assert!(
                    report.diagnostics[0].message.contains("Property"),
                    "{body}: {report:?}"
                );
                assert_eq!(
                    receiver
                        .call("run", std::slice::from_ref(&holder), options.clone())
                        .unwrap()
                        .value
                        .as_int(),
                    Some(7)
                );
            }
            assert_eq!(choices.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn deferred_property_types_resolve_in_the_current_source() {
    for choice in [false, true] {
        for method in ["def write;@item=nil;end", "def write(@item=nil);end"] {
            let (options, choices) = branching_type_options(choice);
            let script = Engine::new().compile(&format!("class Holder;property item:C?;{method};end;def run->int;box=Holder.new;box.write;if box.item==nil && trace.length==2 && trace[0]==0;7;else;false;end;end")).unwrap();
            accepts(&script, &[], &options);
            let report = script.check_function("run", &options).unwrap();
            assert!(report.is_clean(), "{method}: {report:?}");
            assert_eq!(choices.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn deferred_predicates_resume_native_and_namespace_calls() {
    for choice in [false, true] {
        for receiver in ["nil", "[1]", "Local", "Local.new", "M"] {
            for call in ["is_type?(\"C\")", "send(:is_type?, \"C\")"] {
                let (options, choices) = branching_type_options(choice);
                let script = Engine::new().compile(&format!("class Local;end;module M;end;def run->int;answer={receiver}.{call};if !answer && trace.length==2 && trace[0]==0;7;else;false;end;end")).unwrap();
                accepts(&script, &[], &options);
                let report = script.check_function("run", &options).unwrap();
                assert!(report.is_clean(), "{receiver}.{call}: {report:?}");
                assert_eq!(choices.load(Ordering::Relaxed), 2);
            }
        }
    }
}

#[test]
fn deferred_predicate_name_alternatives_only_initialize_the_selected_type() {
    let (mut options, first) = branching_type_options(false);
    let (other, second) = branching_type_options(true);
    options
        .globals
        .insert("D".into(), other.globals["C"].clone());
    for receiver in ["nil", "Local.new"] {
        let script = Engine::new().compile(&format!("class Local;end;def run(flag:bool)->int;name=if flag;\"C\";else;\"D\";end;answer={receiver}.is_type?(begin;trace.push(9);name;end);if !answer && trace.length==3 && trace[0]==9 && trace[1]==0;7;else;false;end;end")).unwrap();
        for flag in [false, true] {
            accepts(&script, &[Value::boolean(flag)], &options);
        }
        let calls = (
            first.load(Ordering::Relaxed),
            second.load(Ordering::Relaxed),
        );
        let report = script.check_function("run", &options).unwrap();
        assert!(report.is_clean(), "{receiver}: {report:?}");
        assert_eq!(
            (
                first.load(Ordering::Relaxed),
                second.load(Ordering::Relaxed)
            ),
            calls
        );
    }
}

#[test]
fn deferred_predicate_alternatives_preserve_invalid_queries() {
    for receiver in ["nil", "Local.new"] {
        for names in [
            ("\"C\"", "9"),
            ("9", "\"C\""),
            ("\"C\"", "\"Missing.Type\""),
        ] {
            let (options, choices) = branching_type_options(true);
            let script = Engine::new().compile(&format!("class Local;end;def run(flag:bool)->int;name=if flag;{};else;{};end;begin;{receiver}.is_type?(name);7;rescue RuntimeError;7;end;end", names.0, names.1)).unwrap();
            for flag in [false, true] {
                assert_eq!(
                    script
                        .call("run", &[Value::boolean(flag)], options.clone())
                        .unwrap()
                        .value
                        .as_int(),
                    Some(7)
                );
            }
            let called = choices.load(Ordering::Relaxed);
            let report = script.check_function("run", &options).unwrap();
            assert!(
                report.incomplete.is_empty(),
                "{receiver}, {names:?}: {report:?}"
            );
            assert_eq!(
                report.diagnostics.len(),
                1,
                "{receiver}, {names:?}: {report:?}"
            );
            assert!(
                !report.diagnostics[0].message.contains("Return value"),
                "{report:?}"
            );
            assert_eq!(choices.load(Ordering::Relaxed), called);
        }
    }
}

#[test]
fn deferred_type_branches_resume_supplied_default_and_return_boundaries() {
    for choice in [false, true] {
        for (definitions, call, length, first) in [
            ("def target(x:C?);7;end", "target(nil)", 2, 0),
            ("def target(x:C?=nil);7;end", "target()", 2, 0),
            ("def target->C?;nil;end", "target()", 2, 0),
            (
                "def target->C?;begin;nil;ensure;trace.push(9);end;end",
                "target()",
                3,
                9,
            ),
        ] {
            let (options, choices) = branching_type_options(choice);
            let script = Engine::new().compile(&format!("{definitions};def run->int;{call};C.value;if trace.length=={length} && trace[0]=={first};7;else;false;end;end")).unwrap();
            accepts(&script, &[], &options);
            assert_eq!(choices.load(Ordering::Relaxed), 2);
            for function in ["run", "target"] {
                let report = script.check_function(function, &options).unwrap();
                assert!(report.is_clean(), "{definitions}, {function}: {report:?}");
            }
            assert_eq!(choices.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn deferred_host_contract_branches_resume_without_repeating_callbacks_or_blocks() {
    use std::sync::Arc;
    use vibescript::{HostMethod, Signature, SignatureParam};
    for choice in [false, true] {
        for result_type in [false, true] {
            for body in [None, Some("nil"), Some("break nil")] {
                let (options, choices) = branching_type_options(choice);
                let calls = Arc::new(AtomicUsize::new(0));
                let blocks = Arc::new(AtomicUsize::new(0));
                let called = calls.clone();
                let mut engine = Engine::new();
                let method = if body.is_some() {
                    let entered = blocks.clone();
                    HostMethod::new_with_block("probe", move |call, _, _| {
                        called.fetch_add(1, Ordering::Relaxed);
                        entered.fetch_add(1, Ordering::Relaxed);
                        call.call_block(&[])?;
                        Ok(Value::nil())
                    })
                } else {
                    HostMethod::new("probe", move |_, _, _| {
                        called.fetch_add(1, Ordering::Relaxed);
                        Ok(Value::nil())
                    })
                };
                engine.register_method(
                    "probe",
                    method
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
                            accepts_block: body.is_some(),
                        })
                        .unwrap(),
                );
                let args = if result_type { "" } else { "nil" };
                let block = body.map_or(String::new(), |body| format!("{{{body}}}"));
                let script = engine.compile(&format!("def run->int;probe({args}){block};C.value;if trace.length==2 && trace[0]==0;7;else;false;end;end")).unwrap();
                accepts(&script, &[], &options);
                assert_eq!(choices.load(Ordering::Relaxed), 2);
                assert_eq!(calls.load(Ordering::Relaxed), 2);
                assert_eq!(
                    blocks.load(Ordering::Relaxed),
                    if body.is_some() { 2 } else { 0 }
                );
            }
        }
    }
}

#[test]
fn deferred_import_branches_preserve_failed_activation_and_rescue_effects() {
    let files = Files::new();
    files.write("left.vibe", "trace.push(1);raise(\"failed import\")");
    files.write("right.vibe", "trace.push(2);def value;2;end");
    let producer = files.engine().compile("class Remote;if choose;require(:left);else;require(:right);end;def self.value;7;end;end;def make;Remote;end").unwrap();
    let mut options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("choose".into(), Value::boolean(false)),
        ]
        .into(),
        ..Default::default()
    };
    let remote = producer.call("make", &[], options.clone()).unwrap().value;
    options.globals.insert("remote".into(), remote);
    let receiver = files.engine().compile("def run(flag:bool)->int;choose=flag;n=0;begin;remote.value;rescue;n+=1;end;begin;remote.value;rescue;n+=1;end;if trace.length==1 && (n==0 || n==2);7;else;false;end;end").unwrap();
    for flag in [false, true] {
        accepts(&receiver, &[Value::boolean(flag)], &options);
    }
    let report = receiver.check_function("run", &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn deferred_import_branches_keep_pending_mutations_and_later_imports() {
    let files = Files::new();
    files.write("left.vibe", "trace.push(1);items.push(1);def value;1;end");
    files.write("right.vibe", "trace.push(2);items.push(2);def value;2;end");
    let producer = files.engine().compile("class Remote;if choose;require(:left);else;require(:right);end;def self.value;7;end;end;def make;Remote;end").unwrap();
    let mut options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            (
                "items".into(),
                Value::array(vec![Value::int(1), Value::int(2)]),
            ),
            ("choose".into(), Value::boolean(false)),
        ]
        .into(),
        ..Default::default()
    };
    let remote = producer.call("make", &[], options.clone()).unwrap().value;
    options.globals.insert("remote".into(), remote);
    for body in [
        "items[-1]+=begin;remote.value;1;end;if items.length==3 && items[1]==3 && trace.length==1;7;else;false;end",
        "remote.value;require(:left);require(:right);if items.length==4 && trace.length==2;7;else;false;end",
        "values=[1,2].map do |n|;remote.value;n+1;end;if values.length==2 && values[0]==2 && values[1]==3 && trace.length==1;7;else;false;end",
    ] {
        let receiver = files
            .engine()
            .compile(&format!("def run(flag:bool)->int;choose=flag;{body};end"))
            .unwrap();
        for flag in [false, true] {
            accepts(&receiver, &[Value::boolean(flag)], &options);
        }
        let report = receiver.check_function("run", &options).unwrap();
        assert!(report.is_clean(), "{body}: {report:?}");
    }
}

#[test]
fn deferred_import_branches_resume_file_reads_and_alias_conflicts() {
    let files = Files::new();
    files.write("consumer.vibe", "def value;C.value;end");
    files.write("replacement.vibe", "trace.push(9);def value;7;end");
    for choice in [false, true] {
        for conflict in [false, true] {
            let (options, choices) = branching_type_options(choice);
            let source = if conflict {
                "def run->int;begin;require(:replacement,as: :C);0;rescue;7;end;end"
            } else {
                "def run->int;m=require(:consumer);m.value();if trace.length==2 && trace[0]==0;7;else;false;end;end"
            };
            let receiver = files.engine().compile(source).unwrap();
            for _ in 0..2 {
                assert_eq!(
                    receiver
                        .call("run", &[], options.clone())
                        .unwrap()
                        .value
                        .as_int(),
                    Some(7)
                );
                let report = receiver.check_call("run", &[], &options).unwrap();
                assert!(report.incomplete.is_empty(), "{report:?}");
                if conflict {
                    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
                    assert!(
                        report.diagnostics[0]
                            .message
                            .contains("alias already defined"),
                        "{report:?}"
                    );
                } else {
                    assert!(report.is_clean(), "{report:?}");
                }
            }
            assert_eq!(choices.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn deferred_source_continuations_keep_contradictions_from_either_history() {
    let script = Engine::new()
        .compile("def run->int;C.value;if trace[1]==1;7;else;false;end;end")
        .unwrap();
    for choice in [false, true] {
        let (options, choices) = branching_type_options(choice);
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(
            report.diagnostics.iter().any(|issue| issue
                .message
                .contains("Return value: expected int, got bool")),
            "{report:?}"
        );
        assert_eq!(choices.load(Ordering::Relaxed), 0);
        let result = script.call("run", &[], options);
        if choice {
            assert_eq!(result.unwrap().value.as_int(), Some(7));
        } else {
            assert_eq!(result.unwrap_err().kind, vibescript::ErrorKind::Type);
        }
        assert_eq!(choices.load(Ordering::Relaxed), 1);
    }
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
fn foreign_general_inputs_keep_required_file_bindings_and_distinct_captures() {
    let files = Files::new();
    files.write("owned.vibe", "base=7;class Box;property n:int;def initialize;@n=base;end;def answer;@n=base;@n;end;end;def klass;Box;end;def make;Box.new;end");
    let producer = files
        .engine()
        .compile("def pair;m=require(:owned);[m.klass(),m.make()];end")
        .unwrap();
    let a = producer
        .call("pair", &[], CallOptions::default())
        .unwrap()
        .value;
    let b = producer
        .call("pair", &[], CallOptions::default())
        .unwrap()
        .value;
    let a = a.as_array().unwrap();
    let b = b.as_array().unwrap();
    let options = CallOptions {
        globals: [("A".into(), a[0].clone()), ("B".into(), b[0].clone())].into(),
        ..Default::default()
    };
    let receiver = Engine::new()
        .compile("def run(a:A,b:B)->int;a.n=7;b.n=9;if a==b;false;else;a.answer;end;end")
        .unwrap();
    assert_eq!(
        receiver
            .call("run", &[a[1].clone(), b[1].clone()], options.clone())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    for whole in [false, true] {
        let report = if whole {
            receiver.check(&options)
        } else {
            receiver.check_function("run", &options)
        }
        .unwrap();
        assert!(report.is_clean(), "{report:?}");
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
