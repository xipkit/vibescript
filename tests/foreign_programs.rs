use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, Error, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn foreign_modules_keep_code_and_hosts_with_fresh_globals_and_state() {
    let mut producer = Engine::new();
    producer.register("host", |_, _| Ok(Value::int(11)));
    let script = producer
        .compile(
            r##"
enum Status
  Ready
end
module M
  C = host()
  A = [1]
  @@n = 0
  def self.bump(n: int = 1)
    @@n += n
    [C, @@n, Math.PI]
  end
  def self.enum_value(x: Status = :ready) -> Status
    x
  end
  def self.update
    Math.PI = 11
  end
end
def make
  M.bump
  M.update
  M
end
"##,
        )
        .unwrap();
    let namespace = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    drop(script);
    drop(producer);
    let mut consumer = Engine::new();
    consumer.register("host", |_, _| Ok(Value::int(99)));
    let receiver = consumer
        .compile(
            r##"
enum Status
  Other
end
module M
  C = 99
end
def run(m)
  Math.PI = 22
  first = m.bump(2)
  second = m.send(:bump, 3)
  m::A[0] = 7
  m.extra = m
  [first, second, m.A, m.extra.equal?(m), M.C, Math.PI, m.enum_value.name]
end
"##,
        )
        .unwrap();
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    receiver
                        .call(
                            "run",
                            std::slice::from_ref(&namespace),
                            CallOptions::default(),
                        )
                        .unwrap()
                })
            })
            .collect();
        for job in jobs {
            assert_eq!(
                json(&job.join().unwrap().value),
                serde_json::json!([
                    [11, 2, std::f64::consts::PI],
                    [11, 5, std::f64::consts::PI],
                    [7],
                    true,
                    99,
                    22,
                    "Ready"
                ])
            );
        }
    });
}

#[test]
fn foreign_instances_preserve_graphs_and_enforce_original_property_types() {
    let mut producer = Engine::new();
    producer.register("host", |_, _| Ok(Value::int(10)));
    let source = producer
        .compile(
            r##"
class Node
  @@created = 0
  property values: array<int>
  property link
  def initialize(n: int)
    @values = [n]
    @@created += 1
  end
  def self.created; @@created; end
  def append(n); @values.push(n); end
  def bad; @values.send(:push, "bad"); end
  def +(n); @values[0] + n + host(); end
  def [](i); @values[i]; end
  def []=(i, n); @values[i] = n; end
  def to_s; @values[0].to_s; end
end
def make
  n = Node.new(7)
  n.link = n
  [Node, n, n]
end
"##,
        )
        .unwrap();
    let graph = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile(
            r##"
class Node
  property values: string
end
def run(input)
  n = input[1]
  n.append(2)
  begin
    n.bad
  rescue
    rejected = true
  end
  before = n.class.created
  fresh = input[0].new(4)
  n[0] = 8
  [n.values, input[2].values, n.link == n, n + 5, n[0], "#{n}",
   format("%s", n), before, fresh.values, n.class.created, rejected]
end
"##,
        )
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&graph), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[8, 2], [8, 2], true, 23, 8, "8", "8", 0, [4], 1, true])
        );
    }
}

#[test]
fn foreign_blocks_use_their_defining_frames_and_unwind_through_ensure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut producer = Engine::new();
    producer.register("notify", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let source = producer
        .compile(
            r##"
module Bridge
  def self.invoke(x: int = 4)
    begin
      yield(x)
    ensure
      notify()
    end
  end
  def self.iterate
    [1, 2, 3].map { |v| yield(v) }
  end
  def self.fail
    1 / 0
  end
end
def make; Bridge; end
"##,
        )
        .unwrap();
    let module = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile(
            r##"
def early(m)
  m.invoke { return 9 }
  0
end
def run(m)
  n = 10
  a = m.invoke(6) { |x| x + n }
  b = m.iterate { |v| next v * 2 }
  c = begin; m.fail; rescue; 7; end
  [a, b, c, early(m), m.invoke { break 8 }]
end
"##,
        )
        .unwrap();
    let output = receiver
        .call("run", &[module], CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([16, [2, 4, 6], 7, 9, 8])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn identical_namespace_indices_do_not_grant_foreign_protected_access() {
    let source = Engine::new()
        .compile(
            r##"
class C
  protected def hidden; 7; end
  protected def value=(n); @value = n; end
  protected def +(n); 11; end
  def peer(other); [other.hidden, other + 1]; end
end
def make; C.new; end
"##,
        )
        .unwrap();
    let instance = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    for expression in [
        "other.hidden",
        "other.public_send(:hidden)",
        "other.value = 3",
        "other + 1",
    ] {
        let receiver = Engine::new().compile(&format!(
            "class C\n def probe(other)\n {expression}\n end\nend\ndef run(other)\n C.new.probe(other)\nend"
        )).unwrap();
        assert_eq!(
            receiver
                .call(
                    "run",
                    std::slice::from_ref(&instance),
                    CallOptions::default()
                )
                .unwrap_err()
                .kind,
            ErrorKind::Name,
            "{expression}"
        );
    }
    let receiver = Engine::new()
        .compile("def run(other); [other.peer(other), other.send(:hidden)]; end")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[instance], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([[7, 11], 7])
    );
}

#[test]
fn failed_foreign_initialization_is_catchable_and_retried_only_in_a_new_call() {
    let failing = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let flag = failing.clone();
    let count = calls.clone();
    let mut producer = Engine::new();
    producer.register("gate", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "initializer boom"))
        } else {
            Ok(Value::int(3))
        }
    });
    let source = producer
        .compile("module M\n VALUE=gate()\nend\ndef make; M; end")
        .unwrap();
    let namespace = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut consumer = Engine::new();
    consumer.register("fetch", move |_, _| Ok(namespace.clone()));
    let receiver = consumer
        .compile(
            r##"
def run
  errors = []
  2.times do
    begin
      m = fetch()
      m.VALUE
    rescue => error
      errors.push(error.message)
    end
  end
  errors
end
def discard; run(); nil; end
"##,
        )
        .unwrap();
    failing.store(true, Ordering::SeqCst);
    let output = receiver.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!(["initializer boom", "source script initialization failed"])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    failing.store(false, Ordering::SeqCst);
    let output = receiver.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([]));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let discarded = receiver
        .call("discard", &[], CallOptions::default())
        .unwrap();
    assert_eq!(discarded.stats.retained_memory_bytes, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn foreign_errors_use_original_source_and_receiving_limits() {
    let producer = Engine::new().compile("module M\n def self.fail\n  1 / 0\n end\n def self.spin\n  while true; end\n end\nend\ndef make; M; end").unwrap();
    let module = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile(
            "\n\n\n\ndef run(m)\n m.fail\nend\ndef spin(m)\n begin; m.spin; rescue; 99; end\nend",
        )
        .unwrap();
    let error = receiver
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    let diagnostic = error.diagnostic.unwrap();
    assert_eq!(diagnostic.position.line, 3);
    assert!(diagnostic.code_frame.contains("1 / 0"));
    assert!(
        diagnostic
            .frames
            .iter()
            .any(|frame| frame.position.line == 6)
    );
    let error = receiver
        .call(
            "spin",
            &[module],
            CallOptions {
                limits: Limits {
                    steps: Some(1000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn nested_foreign_initializers_resume_their_callers_and_poison_abandoned_state() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(AtomicBool::new(false));
    let log = events.clone();
    let flag = failing.clone();
    let mut engine_b = Engine::new();
    engine_b.register("initialize_b", move |_, _| {
        log.lock().unwrap().push("B");
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "B failed"))
        } else {
            Ok(Value::int(2))
        }
    });
    let source_b = engine_b
        .compile("module B\n VALUE=initialize_b()\nend\ndef make; B; end")
        .unwrap();
    let b = source_b
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut engine_a = Engine::new();
    let imported_b = b.clone();
    engine_a.register("fetch_b", move |_, _| Ok(imported_b.clone()));
    let log = events.clone();
    engine_a.register("initialize_a", move |_, _| {
        log.lock().unwrap().push("A");
        Ok(Value::int(3))
    });
    let source_a = engine_a
        .compile(
            "module A\n PARTIAL=1\n VALUE=fetch_b().VALUE + initialize_a()\nend\ndef make; A; end",
        )
        .unwrap();
    let a = source_a
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    events.lock().unwrap().clear();
    let together = Engine::new()
        .compile("def run(a,b); [a.VALUE,b.VALUE]; end")
        .unwrap();
    let output = together
        .call("run", &[a.clone(), b], CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([5, 2]));
    assert_eq!(*events.lock().unwrap(), ["B", "A"]);
    let mut receiver = Engine::new();
    receiver.register("fetch_a", move |_, _| Ok(a.clone()));
    let script = receiver
        .compile(
            r#"
def run
  values = []
  2.times do
    begin
      a = fetch_a()
      values.push(a.VALUE)
    rescue => error
      values.push(error.message)
    end
  end
  begin
    values.push(fetch_a().PARTIAL)
  rescue => error
    values.push(error.message)
  end
  values
end
"#,
        )
        .unwrap();
    events.lock().unwrap().clear();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([5, 5, 1]));
    assert_eq!(*events.lock().unwrap(), ["B", "A"]);
    events.lock().unwrap().clear();
    failing.store(true, Ordering::SeqCst);
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            "B failed",
            "source script initialization failed",
            "source script initialization failed"
        ])
    );
    assert_eq!(*events.lock().unwrap(), ["B"]);
    failing.store(false, Ordering::SeqCst);
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([5, 5, 1]));
}

#[test]
fn foreign_initializers_observe_receiving_cancellation_and_memory_limits() {
    let active = Arc::new(AtomicBool::new(false));
    let flag = active.clone();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut engine = Engine::new();
    engine.register("initialize", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            token.cancel();
        }
        Ok(Value::nil())
    });
    let producer = engine
        .compile("module M\n initialize()\n VALUES=(1..1024).map{|n| n}\nend\ndef make; M; end")
        .unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new()
        .compile("def run(m); m.VALUES.size; end")
        .unwrap();
    let baseline = receiver
        .call("run", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(baseline.value.as_int(), Some(1024));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    let error = receiver
        .call(
            "run",
            std::slice::from_ref(&value),
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(baseline.stats.peak_memory_bytes - 1),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    active.store(true, Ordering::SeqCst);
    let error = receiver
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
fn pending_initializer_dependencies_are_found_in_already_imported_cyclic_graphs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine_b = Engine::new();
    engine_b.register("initialize_b", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(17))
    });
    let source_b = engine_b
        .compile("module B\n VALUE=initialize_b()\nend\ndef make; B; end")
        .unwrap();
    let b = source_b
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let container = Engine::new()
        .compile(
            r#"
class Holder
  property cycle, link
end
def make(b)
  h = Holder.new
  h.cycle = h
  h.link = {entry: [b]}
  h
end
"#,
        )
        .unwrap();
    let holder = container
        .call("make", std::slice::from_ref(&b), CallOptions::default())
        .unwrap()
        .value;
    let imported = holder.clone();
    let mut engine_a = Engine::new();
    engine_a.register("fetch", move |_, _| Ok(imported.clone()));
    let source_a = engine_a
        .compile("module A\n VALUE=fetch().link.entry[0].VALUE + 2\nend\ndef make; A; end")
        .unwrap();
    let a = source_a
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    calls.store(0, Ordering::SeqCst);
    let receiver = Engine::new()
        .compile("def run(a,b,h); [a.VALUE,b.VALUE,h.cycle == h]; end")
        .unwrap();
    let output = receiver
        .call("run", &[a, b, holder], CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([19, 17, true]));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_initialization_blocks_instance_field_writes_without_a_setter() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let mut producer = Engine::new();
    producer.register("gate", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "initializer failed"))
        } else {
            Ok(Value::nil())
        }
    });
    let source = producer
        .compile("class C\n gate()\nend\ndef make; C.new; end")
        .unwrap();
    let instance = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut consumer = Engine::new();
    consumer.register("fetch", move |_, _| Ok(instance.clone()));
    let receiver = consumer
        .compile(
            r#"
def run
  begin; fetch(); rescue; nil; end
  object = fetch()
  begin
    object.extra = 9
    "allowed"
  rescue => error
    error.message
  end
end
"#,
        )
        .unwrap();
    failing.store(true, Ordering::SeqCst);
    let output = receiver.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!("source script initialization failed")
    );
}

fn counted_namespace(calls: &Arc<AtomicUsize>) -> Value {
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("initialize", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let script = engine
        .compile("module M\n VALUE=initialize()\nend\ndef make; M; end")
        .unwrap();
    let namespace = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    calls.store(0, Ordering::SeqCst);
    namespace
}

#[test]
fn rejected_host_graphs_do_not_initialize_retained_sources() {
    let calls = Arc::new(AtomicUsize::new(0));
    let namespace = counted_namespace(&calls);
    let deep = (0..10_001).fold(Value::nil(), |value, _| Value::array(vec![value]));
    let rejected = Value::array(vec![namespace.clone(), deep.clone()]);
    let accepted = namespace.clone();
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("rejected", move |_, _| Ok(rejected.clone()));
    engine.register("accepted", move |_, _| Ok(accepted.clone()));
    engine.register("count", move |_, _| {
        Ok(Value::int(count.load(Ordering::SeqCst) as i64))
    });
    let script = engine
        .compile(
            r#"
def inputs(a, b); nil; end
def run
  begin
    rejected()
  rescue LimitError => error
    message = error.message
  end
  before = count()
  value = accepted().VALUE
  [message, before, value, count()]
end
"#,
        )
        .unwrap();
    assert_eq!(
        script
            .call("inputs", &[namespace, deep], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["value nesting too deep", 0, 7, 1])
    );
}

#[test]
fn host_retention_does_not_initialize_an_unreturned_source() {
    let calls = Arc::new(AtomicUsize::new(0));
    let retained = counted_namespace(&calls);
    let accepted = retained.clone();
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("retain", move |ctx, _| {
        ctx.import(&retained)?;
        Ok(Value::nil())
    });
    engine.register("accepted", move |_, _| Ok(accepted.clone()));
    engine.register("count", move |_, _| {
        Ok(Value::int(count.load(Ordering::SeqCst) as i64))
    });
    let script = engine
        .compile("def run; retain(); [count(), accepted().VALUE, count()]; end")
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([0, 7, 1]));
}

#[test]
fn foreign_initialization_preserves_argument_and_graph_discovery_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let values: Vec<_> = ["A", "B"]
        .into_iter()
        .map(|label| {
            let events = events.clone();
            let mut engine = Engine::new();
            engine.register("initialize", move |_, _| {
                events.lock().unwrap().push(label);
                Ok(Value::nil())
            });
            engine
                .compile("module M\n initialize()\nend\ndef make; M; end")
                .unwrap()
                .call("make", &[], CallOptions::default())
                .unwrap()
                .value
        })
        .collect();
    let script = Engine::new()
        .compile("def inputs(a,b); nil; end\ndef graph(x); nil; end\ndef named(a:, b:); nil; end")
        .unwrap();
    events.lock().unwrap().clear();
    script
        .call("inputs", &values, CallOptions::default())
        .unwrap();
    assert_eq!(*events.lock().unwrap(), ["A", "B"]);
    events.lock().unwrap().clear();
    let graph = Value::hash(vec![(
        b"items".to_vec(),
        Value::array(vec![
            values[0].clone(),
            values[1].clone(),
            values[0].clone(),
        ]),
    )]);
    script
        .call("graph", &[graph], CallOptions::default())
        .unwrap();
    assert_eq!(*events.lock().unwrap(), ["A", "B"]);
    events.lock().unwrap().clear();
    script
        .call_with_keywords(
            "named",
            &[],
            &[
                ("b".into(), values[1].clone()),
                ("a".into(), values[0].clone()),
            ],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(*events.lock().unwrap(), ["B", "A"]);
}
