//! A captured namespace reaches a typed script as `any`, which the script
//! can compare and return but not call. So these callers import captured
//! scopes through their arguments and host results, which runs their
//! initializers, and return them; the tests read the state each returned
//! scope's environment holds. Scenarios that call a scope's methods use a
//! required file instead, whose modules and classes each call gives an
//! environment of their own.

use super::*;
use crate::{CallOptions, CancellationToken, Engine, Limits, Script, namespace::Namespace};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A module whose `COUNT` its initializer takes from the host.
const COUNTER: &str = r#"
module Counter
  COUNT = initialize()
end
"#;

fn captured(script: &Script, name: &str) -> Value {
    let mut ctx = CallContext::new(CallOptions::default());
    let environment = crate::objects::environment(&mut ctx).unwrap();
    let definition = script
        .inner
        .code
        .program
        .namespaces
        .iter()
        .find(|definition| definition.name == name)
        .unwrap();
    let namespace = Namespace::import(&mut ctx, &Namespace::untracked(definition.clone())).unwrap();
    let namespace = Namespace::with_environment(&mut ctx, &namespace, environment).unwrap();
    crate::objects::finish(&mut ctx).unwrap();
    Value(Kind::Namespace(namespace))
}

/// The constant `name` of a captured namespace a call returned, as its
/// environment holds it.
fn constant(value: &Value, name: &str) -> Value {
    let Kind::Namespace(namespace) = &value.0 else {
        panic!("not a namespace: {value}");
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let environment = namespace.environment.as_ref().unwrap();
    let state = scopes::namespace(&mut ctx, environment, namespace.definition.index).unwrap();
    crate::objects::field(&mut ctx, &state.fields, name)
        .unwrap()
        .unwrap()
}

/// The `COUNT` of each captured namespace in `values`.
fn counts(values: &[Value]) -> Vec<Option<i64>> {
    values
        .iter()
        .map(|value| constant(value, "COUNT").as_int())
        .collect()
}

/// Compiles `COUNTER` with an `initialize` host function that returns 1,
/// 2, ... and counts its calls in `initialized`.
fn counter(initialized: &Arc<AtomicUsize>) -> Script {
    let observed = initialized.clone();
    let mut engine = Engine::new();
    engine.register("initialize", move |_, _| {
        Ok(Value::int(
            observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
        ))
    });
    engine.compile(COUNTER).unwrap()
}

fn json(value: &Value) -> serde_json::Value {
    let output = crate::stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn distinct_environments_of_one_code_preserve_aliases_and_independent_state() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let caller = Engine::new()
        .compile(
            "def run(a: any, b: any, again: any) -> array<any>\n [a == b, a == again, a, b, again]\nend",
        )
        .unwrap();
    for _ in 0..2 {
        initialized.store(0, Ordering::SeqCst);
        let result = caller
            .call(
                "run",
                &[a.clone(), b.clone(), a.clone()],
                CallOptions::default(),
            )
            .unwrap();
        let values = result.value.as_array().unwrap();
        assert_eq!(
            json(&Value::array(values[..2].to_vec())),
            serde_json::json!([false, true])
        );
        assert_eq!(counts(&values[2..]), [Some(1), Some(2), Some(1)]);
        assert_eq!(initialized.load(Ordering::SeqCst), 2);
    }
    let caller = Engine::new()
        .compile("def run(*, a: any, b: any) -> array<any>\n [a == b, a, b]\nend")
        .unwrap();
    initialized.store(0, Ordering::SeqCst);
    let result = caller
        .call_with_keywords(
            "run",
            &[],
            &[("b".into(), b), ("a".into(), a)],
            CallOptions::default(),
        )
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(json(&values[0]), serde_json::json!(false));
    assert_eq!(counts(&values[1..]), [Some(2), Some(1)]);
}

#[test]
fn retained_state_is_copied_without_repeating_completed_initializers() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let incoming = captured(&source, "Counter");
    let caller = Engine::new()
        .compile(
            r#"
def warm(m: any) -> any
  m
end
def change(m: any) -> array<any>
  [m == m, m]
end
def discard(m: any)
end
"#,
        )
        .unwrap();
    let warmed = caller
        .call("warm", &[incoming], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
    assert_eq!(counts(std::slice::from_ref(&warmed)), [Some(1)]);
    drop(source);
    // WASI has no threads.
    #[cfg(not(target_os = "wasi"))]
    std::thread::scope(|threads| {
        for _ in 0..4 {
            let caller = &caller;
            let warmed = &warmed;
            threads.spawn(move || {
                let result = caller
                    .call(
                        "change",
                        std::slice::from_ref(warmed),
                        CallOptions::default(),
                    )
                    .unwrap();
                let values = result.value.as_array().unwrap();
                assert_eq!(json(&values[0]), serde_json::json!(true));
                assert_eq!(counts(&values[1..]), [Some(1)]);
            });
        }
    });
    let discarded = caller
        .call("discard", &[warmed], CallOptions::default())
        .unwrap();
    assert_eq!(discarded.stats.retained_memory_bytes, 0);
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
}

#[test]
fn successful_host_results_admit_every_environment_of_already_known_code() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let both = Value::array(vec![
        a.clone(),
        Value::hash(vec![(b"second".to_vec(), b.clone())]),
    ]);
    let mut engine = Engine::new();
    engine.register("first", move |_, _| Ok(a.clone()));
    engine.register("second", move |_, _| Ok(b.clone()));
    engine.register("both", move |_, _| Ok(both.clone()));
    let caller = engine
        .compile(
            r#"
def sequential -> array<any>
  a = first()
  b = second()
  [a == b, a, b]
end
def graph -> array<any>
  values = both().as(array<any>)
  a = values.fetch(0)
  b = values.fetch(1).as(hash<string, any>).fetch("second")
  [a == b, a, b]
end
"#,
        )
        .unwrap();
    let result = caller
        .call("sequential", &[], CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(json(&values[0]), serde_json::json!(false));
    assert_eq!(counts(&values[1..]), [Some(1), Some(2)]);
    initialized.store(0, Ordering::SeqCst);
    let result = caller.call("graph", &[], CallOptions::default()).unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(json(&values[0]), serde_json::json!(false));
    assert_eq!(counts(&values[1..]), [Some(1), Some(2)]);
}

/// A module directory, removed when dropped. Each call gives a required
/// file its own environment, like a captured namespace's.
struct Files(std::path::PathBuf);

impl Files {
    fn new(name: &str, source: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".cache/tmp")
            .join(format!(
                "scopes-{}-{}",
                crate::loading::test_support::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(name), source).unwrap();
        Self(path)
    }

    fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        engine
            .set_module_config(crate::ModuleConfig {
                paths: vec![self.0.clone()],
                ..crate::ModuleConfig::default()
            })
            .unwrap();
        engine
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = std::fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

fn requiring() -> CallOptions {
    CallOptions {
        allow_require: true,
        ..CallOptions::default()
    }
}

#[test]
fn captured_fields_keep_pending_addresses_snapshots_and_recoverable_writes() {
    // A required file's functions write its module's constants and class
    // variables, which live in the file's environment, as a captured
    // namespace's do. Compound writes go through shape fields, since an
    // array element read is optional.
    let files = Files::new(
        "fields.vibe",
        r#"
module Counter
  ITEMS = [1]
  DATA = { values: [{ n: 2 }] }
  module Nested
    VALUES = [3]
  end
  @@extra: array<{ n: int }> = []
  def self.pending -> array<{ n: int }>
    @@extra = [{ n: 7 }]
    @@extra[0]["n"] += @@extra.push({ n: 8 }).fetch(-1)["n"]
    @@extra
  end
  def self.reject
    @@extra.insert(10, { n: 0 })
  end
  def self.extra -> array<{ n: int }>
    @@extra
  end
end
def change -> array<any>
  old = Counter::ITEMS
  Counter::ITEMS[0] = 2
  Counter::DATA["values"][0]["n"] += 3
  Counter::Nested::VALUES[0] = 4
  Counter.pending
  begin
    Counter.reject
  rescue
    nil
  end
  begin
    Counter::DATA["values"][0]["n"] //= 0
  rescue
    nil
  end
  [old, Counter::ITEMS, Counter::DATA["values"], Counter::Nested::VALUES, Counter.extra]
end
def scope -> any
  Counter
end
"#,
    );
    let caller = files
        .engine()
        .compile(
            "def run -> array<any>\n fields = require(\"fields\")\n [fields.change, fields.scope]\nend\ndef keep(m: any) -> any\n m\nend",
        )
        .unwrap();
    let result = caller.call("run", &[], requiring()).unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        json(&values[0]),
        serde_json::json!([[1], [2], [{"n": 5}], [4], [{"n": 15}, {"n": 8}]])
    );
    let retained = values[1].clone();
    drop(result);
    let kept = caller
        .call("keep", &[retained], CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&constant(&kept.value, "ITEMS")),
        serde_json::json!([2])
    );
    assert_eq!(
        json(&constant(&kept.value, "DATA")),
        serde_json::json!({"values": [{"n": 5}]})
    );
}

#[test]
fn protected_dispatch_and_nominal_types_distinguish_captured_classes() {
    // A typed script cannot instantiate a captured class, but an instance
    // an earlier call made of a required file's class belongs to another
    // environment of the same class code.
    let files = Files::new(
        "peers.vibe",
        r#"
class C
  @value: int = 0
  protected def hidden -> int; 7; end
  protected def value=(n: int); @value = n; end
  protected def +(n: int) -> int; 11; end
  def peer(other: C) -> int; other.hidden; end
  def set_peer(other: C) -> int; other.value = 3; end
  def add_peer(other: C) -> int; other + 1; end
  def typed_peer(other: C) -> int; 13; end
end
def make -> C; C.new; end
def peer(other: any) -> int; C.new.peer(other.as(C)); end
def set_peer(other: any) -> int; C.new.set_peer(other.as(C)); end
def add_peer(other: any) -> int; C.new.add_peer(other.as(C)); end
def typed_peer(other: any) -> int; C.new.typed_peer(other.as(C)); end
"#,
    );
    let engine = files.engine();
    for (method, expected) in [
        ("peer", 7),
        ("set_peer", 3),
        ("add_peer", 11),
        ("typed_peer", 13),
    ] {
        let caller = engine
            .compile(&format!(
                "def make -> any\n require(\"peers\").make\nend\ndef run(other: any) -> int\n require(\"peers\").{method}(other)\nend\ndef own -> int\n peers = require(\"peers\")\n peers.{method}(peers.make)\nend"
            ))
            .unwrap();
        let other = caller.call("make", &[], requiring()).unwrap().value;
        let error = caller.call("run", &[other], requiring()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{method}: {error}");
        let result = caller.call("own", &[], requiring()).unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{method}");
    }
}

#[test]
fn invalid_later_arguments_stop_before_captured_initializers() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let incoming = captured(&source, "Counter");
    let caller = Engine::new()
        .compile("def run(a: any, b: any)\nend")
        .unwrap();
    let mut deep = Value::nil();
    for _ in 0..crate::budget::MAX_VALUE_DEPTH + 1 {
        deep = Value::array(vec![deep]);
    }
    assert_eq!(
        caller
            .call("run", &[incoming.clone(), deep], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert_eq!(
        caller
            .call(
                "run",
                &[incoming, Value::bytes(vec![0; 1 << 20])],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(32768),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
}

#[test]
fn captured_methods_keep_caller_blocks_and_ensure_on_the_receiving_stack() {
    // The methods of a required file's module run in the file's
    // environment, as a captured namespace's do.
    let files = Files::new(
        "counter.vibe",
        r#"
module Counter
  @@count: int = 0
  def self.invoke(&block: int -> int) -> int
    begin
      yield(@@count)
    ensure
      @@count += 1
    end
  end
  def self.current -> int; @@count; end
end
def invoke(&block: int -> int) -> int
  Counter.invoke { |x| yield(x) }
end
def current -> int; Counter.current; end
"#,
    );
    let caller = files
        .engine()
        .compile(
            "def run -> array<int>\n m = require(\"counter\")\n n = 10\n a = m.invoke { |x| x + n }\n b = m.invoke { |x| break 7 }\n [a, b, m.current]\nend",
        )
        .unwrap();
    let result = caller.call("run", &[], requiring()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([10, 7, 2]));
}

#[test]
fn rejected_and_unreturned_scopes_are_only_initialized_when_later_accepted() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let never_returned = captured(&source, "Counter");
    let deep = (0..crate::budget::MAX_VALUE_DEPTH + 1)
        .fold(Value::nil(), |value, _| Value::array(vec![value]));
    let rejected = Value::array(vec![a.clone(), deep]);
    let unreturned = b.clone();
    let count = initialized.clone();
    let mut consumer = Engine::new();
    consumer.register("rejected", move |_, _| Ok(rejected.clone()));
    consumer.register("retain", move |ctx, _| {
        ctx.import(&unreturned)?;
        ctx.import(&never_returned)?;
        Ok(Value::nil())
    });
    consumer.register("first", move |_, _| Ok(a.clone()));
    consumer.register("second", move |_, _| Ok(b.clone()));
    consumer.register("count", move |_, _| {
        Ok(Value::int(count.load(Ordering::SeqCst) as i64))
    });
    let caller = consumer
        .compile(
            r#"
def run -> array<any>
  begin
    rejected()
  rescue LimitError
    nil
  end
  retain()
  before = count()
  second_scope = second()
  after_second = count()
  first_scope = first()
  [before, after_second, count(), second_scope, first_scope]
end
"#,
        )
        .unwrap();
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        json(&Value::array(values[..3].to_vec())),
        serde_json::json!([0, 1, 2])
    );
    assert_eq!(counts(&values[3..]), [Some(1), Some(2)]);
    assert_eq!(initialized.load(Ordering::SeqCst), 2);
}

#[test]
fn same_code_scopes_initialize_in_argument_and_graph_discovery_order() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let source = counter(&initialized);
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let graph = Value::array(vec![
        b.clone(),
        Value::hash(vec![(b"a".to_vec(), a.clone())]),
        b.clone(),
    ]);
    let mut consumer = Engine::new();
    consumer.register("graph", move |_, _| Ok(graph.clone()));
    let caller = consumer
        .compile(
            r#"
def positional(a: any, b: any, c: any) -> array<any>
  [a, b, c]
end
def keywords(*, a: any, b: any) -> array<any>
  [a, b]
end
def run -> array<any>
  v = graph().as(array<any>)
  [v.fetch(0), v.fetch(1).as(hash<string, any>).fetch("a"), v.fetch(2)]
end
"#,
        )
        .unwrap();
    let result = caller
        .call(
            "positional",
            &[a.clone(), b.clone(), a.clone()],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        counts(result.value.as_array().unwrap()),
        [Some(1), Some(2), Some(1)]
    );
    initialized.store(0, Ordering::SeqCst);
    let result = caller
        .call_with_keywords(
            "keywords",
            &[],
            &[("b".into(), b), ("a".into(), a)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(counts(result.value.as_array().unwrap()), [Some(2), Some(1)]);
    initialized.store(0, Ordering::SeqCst);
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        counts(result.value.as_array().unwrap()),
        [Some(1), Some(2), Some(1)]
    );
}

#[test]
fn captured_initializers_obey_receiving_budgets_cancellation_and_cleanup() {
    let active = Arc::new(AtomicBool::new(false));
    let flag = active.clone();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut producer = Engine::new();
    producer.register("initialize", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            token.cancel();
        }
        Ok(Value::nil())
    });
    let source = producer
        .compile("module M\n initialize()\n VALUES = (1..1024).map { |n| n }\nend")
        .unwrap();
    let value = captured(&source, "M");
    let caller = Engine::new()
        .compile("def run(m: any) -> bool\n m == m\nend\ndef read(m: any) -> any\n m\nend")
        .unwrap();
    let read = caller
        .call("read", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(
        constant(&read.value, "VALUES")
            .as_array()
            .map(<[Value]>::len),
        Some(1024)
    );
    let baseline = caller
        .call("run", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(json(&baseline.value), serde_json::json!(true));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    for (limits, expected) in [
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes - 1),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            Limits {
                steps: Some(baseline.stats.steps - 1),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
    ] {
        let error = caller
            .call(
                "run",
                std::slice::from_ref(&value),
                CallOptions {
                    limits,
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
    let exact = caller
        .call(
            "run",
            std::slice::from_ref(&value),
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(baseline.stats.peak_memory_bytes),
                    steps: Some(baseline.stats.steps),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(json(&exact.value), serde_json::json!(true));
    assert_eq!(exact.stats.retained_memory_bytes, 0);
    active.store(true, Ordering::SeqCst);
    let error = caller
        .call(
            "run",
            &[value],
            CallOptions {
                cancellation,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn failed_initializers_do_not_poison_fresh_host_result_snapshots() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut producer = Engine::new();
    producer.register("initialize", move |_, _| {
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(Error::new(ErrorKind::Runtime, "scope failed"));
        }
        Ok(Value::int(7))
    });
    let source = producer.compile(COUNTER).unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let mut consumer = Engine::new();
    consumer.register("first", move |_, _| Ok(a.clone()));
    consumer.register("second", move |_, _| Ok(b.clone()));
    let caller = consumer
        .compile(
            r#"
def run -> array<any>
  failed = ""
  begin
    first()
  rescue => error
    failed = error.message
  end
  other = second()
  again: any = nil
  begin
    again = first()
  rescue => error
    again = error.message
  end
  [failed, other, again]
end
def retry_scope -> bool
  first() == nil
end
"#,
        )
        .unwrap();
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(json(&values[0]), serde_json::json!("scope failed"));
    assert_eq!(counts(&values[1..]), [Some(7), Some(7)]);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let result = caller
        .call("retry_scope", &[], CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!(false));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}
