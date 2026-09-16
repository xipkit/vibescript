use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, ModuleConfig, Value, stringify_json,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let path = base.join(format!(
                "require-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create require fixtures: {error}"),
            }
        }
    }

    fn write(&self, name: &str, source: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![self.0.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        engine
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn module_exports_share_private_state_and_reset_each_call() {
    let files = Files::new();
    files.write(
        "counter.vibe",
        r#"
x=0
private def hidden;99;end
def add(n);x+=n;x;end
export def zero;7;end
enum Status
 Ready
end
class Internal
end
"#,
    );
    let script = files
        .engine()
        .compile(
            r#"def run
 m=require("counter",as: :Counter)
 [m.add(2),Counter.add(3),add(4),m.zero,m.Status::Ready.name,Status::Ready.name,m.keys]
end"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([2, 5, 9, 7, "Ready", "Ready", ["Status", "add", "zero"]])
        );
    }
}

#[test]
fn indexed_scoped_and_symbolic_calls_keep_the_module_target() {
    let files = Files::new();
    files.write(
        "calls.vibe",
        "x=0\ndef add(n=1);x+=n;x;end\ndef apply(n,extra:2);yield(n+extra);end",
    );
    let engine = files.engine();
    for expression in [
        "m.add(2)",
        "m::add(2)",
        "m[:add](2)",
        "m.fetch(:add)(2)",
        "m.send(:add,2)",
        "m.public_send(:add,2)",
    ] {
        let source = format!("m=require(:calls);[{expression},m.add()]");
        let output = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(
            json(&output.value),
            serde_json::json!([2, 3]),
            "{expression}"
        );
    }
    for expression in [
        "m.apply(3,extra:4){|n|n*2}",
        "m[:apply](3,extra:4){|n|n*2}",
        "m::apply(3,extra:4){|n|n*2}",
        "m.public_send(:apply,3,extra:4){|n|n*2}",
    ] {
        let source = format!("m=require(:calls);{expression}");
        let output = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(output.value.as_int(), Some(14), "{expression}");
    }
}

#[test]
fn exported_functions_cannot_be_extracted_stored_passed_or_returned() {
    let files = Files::new();
    files.write("functions.vibe", "def fn(n);n;end\ndef zero;7;end");
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    for expression in [
        "m[:fn]",
        "m::fn",
        "m[:zero]",
        "m::zero",
        "m.fetch(:fn)",
        "m.dig(:fn)",
        "m.values",
        "m.values_at(:fn)",
        "m.fetch_values(:fn)",
        "x=m[:fn];x(1)",
        "[m[:fn]]",
        "{value:m[:fn]}",
        "effect(m[:fn])",
        "effect(m::fn)",
        "m.each_value{|fn|effect(fn)}",
        "m.each{|key,fn|effect(fn)}",
        "m.each_value{effect()}",
        "for key,fn in m;effect(fn);end",
        "effect(**m)",
        "m[:fn].call(1)",
    ] {
        let source = format!("m=require(:functions);{expression};effect()");
        let error = engine
            .compile(&source)
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"))
            .run(CallOptions::default())
            .expect_err(expression);
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error:?}");
        assert!(
            error.message.contains("cannot be used as a value"),
            "{expression}: {error:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn aliases_reuse_a_module_but_reject_conflicts_before_initialization() {
    let files = Files::new();
    files.write("module.vibe", "effect();def value;7;end");
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let output = engine
        .compile("a=require(:module,as: :M);b=require(:module,as: :M);[a.value,b.value,M.value]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([7, 7, 7]));
    assert_eq!(effects.swap(0, Ordering::SeqCst), 1);
    for source in [
        "M=nil;require(:module,as: :M)",
        "def M;1;end;require(:module,as: :M)",
        "require(:module,as: :Math)",
        "require(:module,as: :effect)",
        "def run(M);require(:module,as: :M);end;run(1)",
        "module Scope;M=1;def self.load;require(:module,as: :M);end;end;Scope.load",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{source}");
    }
}

#[test]
fn retained_modules_are_isolated_at_call_boundaries() {
    let files = Files::new();
    files.write("counter.vibe", "x=0;def add(n);x+=n;x;end");
    let maker = files.engine().compile("require(:counter)").unwrap();
    let module = maker.run(CallOptions::default()).unwrap().value;
    let receiver = Engine::new()
        .compile("def run(m);[m.add(2),m.add(3)];end")
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&module), CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), serde_json::json!([2, 5]));
        assert!(output.stats.retained_memory_bytes < 1024);
    }
}

#[test]
fn failed_initialization_is_retryable_and_does_not_publish_exports() {
    let files = Files::new();
    files.write("failure.vibe", "attempt();def exported;7;end");
    let mut engine = files.engine();
    let attempts = Arc::new(AtomicUsize::new(0));
    let captured = attempts.clone();
    engine.register("attempt", move |_, _| {
        if captured.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(vibescript::Error::new(ErrorKind::Runtime, "first attempt"))
        } else {
            Ok(Value::nil())
        }
    });
    let output = engine
        .compile(
            r#"first=begin;require(:failure,as: :M);rescue;true;end
missing=begin;exported();rescue;true;end
m=require(:failure,as: :M)
[first,missing,m.exported,M.exported,exported()]"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([true, true, 7, 7, 7])
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn host_transfers_cannot_admit_detached_exported_functions() {
    let files = Files::new();
    files.write("exports.vibe", "def fn(n);n;end");
    let module = files
        .engine()
        .compile("require(:exports)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let function = module.as_hash().unwrap()[0].1.clone();
    let mut engine = Engine::new();
    let returned = function.clone();
    engine.register("detached", move |_, _| Ok(returned.clone()));
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let receiver = engine.compile("def run(m);effect();m;end").unwrap();
    for value in [
        function.clone(),
        Value::array(vec![function.clone()]),
        Value::hash(vec![(b"fn".to_vec(), function)]),
    ] {
        let error = receiver
            .call("run", &[value], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    for expression in ["detached()", "detached()(1)", "effect(detached())"] {
        let source = format!("{expression};effect()");
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn cached_compilation_preserves_each_scripts_registered_callbacks() {
    let files = Files::new();
    files.write("host.vibe", "def value;host_value();end");
    let mut engine = files.engine();
    engine.register("host_value", |_, _| Ok(Value::int(1)));
    let first = engine.compile("require(:host).value").unwrap();
    engine.register("host_value", |_, _| Ok(Value::int(2)));
    let second = engine.compile("require(:host).value").unwrap();
    for (script, expected) in [(&second, 2), (&first, 1), (&second, 2), (&first, 1)] {
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(expected)
        );
    }
    let module = engine
        .compile("require(:host)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut receiving = Engine::new();
    receiving.register("host_value", |_, _| Ok(Value::int(99)));
    let receiver = receiving.compile("def run(m);m.value;end").unwrap();
    assert_eq!(
        receiver
            .call("run", &[module], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(2)
    );
}

#[test]
fn cache_modes_clear_and_pins_keep_per_call_compilation_consistent() {
    let files = Files::new();
    for development in [false, true] {
        files.write("cache.vibe", "def value;1;end");
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![files.0.clone()],
                development,
                ..ModuleConfig::default()
            })
            .unwrap();
        let script = engine.compile("require(:cache).value").unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(1)
        );
        files.write("cache.vibe", "def value;222;end");
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(if development { 222 } else { 1 })
        );
        engine.clear_module_cache();
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(222)
        );
    }
    files.write("cache.vibe", "def value;1;end");
    let mut engine = files.engine();
    let cache_file = files.0.join("cache.vibe");
    let holder = Arc::new(std::sync::OnceLock::<std::sync::Weak<Engine>>::new());
    let captured = holder.clone();
    engine.register("replace_source", move |_, _| {
        fs::write(&cache_file, "def value;222;end").unwrap();
        captured
            .get()
            .unwrap()
            .upgrade()
            .unwrap()
            .clear_module_cache();
        Ok(Value::nil())
    });
    let engine = Arc::new(engine);
    holder.set(Arc::downgrade(&engine)).unwrap();
    let script = engine
        .compile("a=require(:cache);replace_source();b=require(\"cache.vibe\");[a.value,b.value]")
        .unwrap();
    assert_eq!(
        json(&script.run(CallOptions::default()).unwrap().value),
        serde_json::json!([1, 1])
    );
    assert_eq!(
        engine
            .compile("require(:cache).value")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(222)
    );
}

#[test]
fn relative_imports_policy_and_cycle_diagnostics_use_file_origins() {
    let files = Files::new();
    files.write(
        "package/main.vibe",
        "m=require(\"./child\");def value;m.value;end",
    );
    files.write("package/child.vibe", "def value;7;end");
    let engine = files.engine();
    assert_eq!(
        engine
            .compile("require(\"package/main\").value")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    files.write("a.vibe", "require(:b)");
    files.write("b.vibe", "require(:a)");
    let error = engine
        .compile("require(:a)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(
        error.message.contains("a.vibe -> b.vibe -> a.vibe"),
        "{error:?}"
    );
    let mut denied = Engine::new();
    denied
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            deny: vec!["package/child".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let error = denied
        .compile("require(\"package/main\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("denied by policy"), "{error:?}");
}

#[test]
fn exported_calls_observe_receiving_budgets_and_cancellation() {
    let files = Files::new();
    files.write(
        "work.vibe",
        "def work(n);a=[];for i in 1..n;a.push(i);end;a.length;end",
    );
    let module = files
        .engine()
        .compile("require(:work)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(m);m.work(100);end").unwrap();
    let baseline = receiver
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap();
    assert_eq!(baseline.value.as_int(), Some(100));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    let mut options = CallOptions::default();
    options.limits.steps = Some(baseline.stats.steps);
    assert!(
        receiver
            .call("run", std::slice::from_ref(&module), options.clone())
            .is_ok()
    );
    options.limits.steps = Some(baseline.stats.steps - 1);
    assert_eq!(
        receiver
            .call("run", std::slice::from_ref(&module), options)
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
    assert_eq!(
        receiver
            .call("run", std::slice::from_ref(&module), options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        receiver
            .call(
                "run",
                &[module],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn required_code_resolves_receiving_declarations_aliases_and_private_assignment() {
    let files = Files::new();
    files.write(
        "reader.vibe",
        r#"
def values;[Root.answer,State::Ready.name,Math.PI,Parent.zero];end
def typed(value: Root);value.answer;end
def optional;before=Parent.zero;Parent={zero:19};[before,Parent.zero];end
def shadow;Root=11;Root;end
def local;secret;end
"#,
    );
    files.write("peer.vibe", "def zero;17;end");
    let module = files
        .engine()
        .compile("require(:reader)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let receiver = files
        .engine()
        .compile(
            r#"
class Root
 def self.answer;7;end
 def answer;9;end
end
module Math
 PI=22
end
enum State
 Ready
end
def run(m)
 require(:peer,as: :Parent)
 secret=123
 hidden=begin;m.local;rescue;"hidden";end
 [m.values,m.typed(Root.new),m.optional,Parent.zero,m.shadow,Root.answer,hidden]
end
"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&module), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[7, "Ready", 22, 17], 9, [17, 19], 17, 11, 7, "hidden"])
        );
    }
}

#[test]
fn dynamic_root_aliases_support_assignment_nested_writes_and_parameter_shadowing() {
    let files = Files::new();
    files.write("empty.vibe", "1");
    let engine = files.engine();
    let output = engine
        .compile(
            r#"
def change
 A={items:[1],n:2}
 A.items.push(3)
 A.n+=1
end
def shadow(A);A.n=8;A.n;end
def read;A;end
require(:empty,as: :A)
[change(),A.n,read.items,shadow({n:4}),A.n]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([3, 3, [1, 3], 8, 3]));
    let output = engine
        .compile(
            r#"
require(:empty,as: :A)
A=1
def bump;A+=2;A;end
[bump(),A]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([3, 3]));
}

#[test]
fn required_enums_resolve_global_and_alias_type_annotations() {
    let files = Files::new();
    files.write("state.vibe", "enum State\n Ready\nend");
    let output = files
        .engine()
        .compile(
            r#"
def plain(value: State);value.name;end
def namespaced(value: M.State);value.name;end
require(:state,as: :M)
[plain(State::Ready),namespaced(M.State::Ready)]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!(["Ready", "Ready"]));
}

#[test]
fn foreign_required_functions_can_find_new_receiving_hosts_and_root_functions() {
    let files = Files::new();
    files.write("foreign.vibe", "def run;[late_host(),shared(),Math()];end");
    let module = files
        .engine()
        .compile("require(:foreign)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut engine = Engine::new();
    engine.register("late_host", |_, _| Ok(Value::int(3)));
    let receiver = engine
        .compile("def shared;5;end\ndef Math;7;end\ndef run(m);m.run;end")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[module], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([3, 5, 7])
    );
}

#[test]
fn exported_targets_are_selected_before_arguments_change_the_module() {
    let files = Files::new();
    files.write(
        "selection.vibe",
        "def fn(n);n+1;end\ndef push(n,extra:0);n+extra+2;end",
    );
    let engine = files.engine();
    for (expression, expected) in [
        ("m.fn(begin;m.fn=7;end)", 8),
        ("m[:fn](begin;m.fn=7;end)", 8),
        ("m::fn(begin;m.fn=7;end)", 8),
        ("m.push(begin;m.push=7;end)", 9),
        ("m.push(begin;m.push=7;end,extra:3)", 12),
        ("m.public_send(:push,begin;m.push=7;end)", 9),
    ] {
        let source = format!("m=require(:selection);{expression}");
        let result = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(result.value.as_int(), Some(expected), "{expression}");
    }
}

#[test]
fn auto_calls_in_mutation_paths_preserve_returned_collection_values() {
    let files = Files::new();
    files.write("collections.vibe", "data=[1];def items;data;end");
    let output = files
        .engine()
        .compile("m=require(:collections);m.items.push(2);[m.items,m.keys]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([[1], ["items"]]));
}

#[test]
fn namespace_initializers_run_before_file_bodies_once_per_call() {
    let files = Files::new();
    files.write(
        "order.vibe",
        "class Hidden\n event(1)\nend\nevent(2)\ndef value;3;end",
    );
    let mut engine = files.engine();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    engine.register("event", move |_, args| {
        captured.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    let script = engine
        .compile("a=require(:order);b=require(:order);[a.value,b.value,a.keys]")
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            json(&script.run(CallOptions::default()).unwrap().value),
            serde_json::json!([3, 3, ["value"]])
        );
    }
    assert_eq!(*events.lock().unwrap(), [1, 2, 1, 2]);
}

#[test]
fn receiving_policy_controls_relative_require_from_retained_functions() {
    let files = Files::new();
    files.write(
        "package/main.vibe",
        "def child;require(\"./child\").value;end",
    );
    files.write("package/child.vibe", "def value;7;end");
    let module = files
        .engine()
        .compile("require(\"package/main\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            deny: vec!["package/child".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let denied = engine.compile("def run(m);m.child;end").unwrap();
    let error = denied
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("denied by policy"), "{error:?}");
    let receiver = Engine::new().compile("def run(m);m.child;end").unwrap();
    assert_eq!(
        receiver
            .call("run", &[module], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn configured_cache_source_limits_and_mid_initializer_cancellation_are_enforced() {
    let files = Files::new();
    files.write("one.vibe", "def value;1;end");
    files.write("two.vibe", "def value;2;end");
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            cache_limit: 1,
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("require(:one);require(:two)").unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert!(error.message.contains("cache limit reached"), "{error:?}");
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            source_limit: 4,
            ..ModuleConfig::default()
        })
        .unwrap();
    let error = engine
        .compile("require(:one)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(
        error.message.contains("source exceeds maximum size"),
        "{error:?}"
    );
    files.write("cancel.vibe", "stop();effect();def value;1;end");
    let mut engine = files.engine();
    let token = CancellationToken::new();
    let captured = token.clone();
    engine.register("stop", move |_, _| {
        captured.cancel();
        Ok(Value::nil())
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("begin;require(:cancel);rescue;effect();ensure;effect();end;effect()")
        .unwrap();
    let error = script
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}
