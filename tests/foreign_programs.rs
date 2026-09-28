//! Code that one script hands to another through the host arrives as
//! `any`. The receiving script can compare such a value, pass it on and
//! render it, which runs the foreign class's `to_s` in the receiving
//! invocation, but it cannot call the foreign code's other members. So the
//! foreign programs expose what these tests observe through `to_s`, and
//! blocks passed to another program's functions go through a required file,
//! whose exports the requiring script calls with static types.

mod common;

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, CancellationToken, Error, ErrorKind, Limits, ModuleConfig, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

/// The JSON text a foreign `to_s` rendered, parsed.
fn rendered(value: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(value.as_str().unwrap()).unwrap()
}

#[test]
fn foreign_modules_keep_code_and_hosts_with_fresh_globals_and_state() {
    let mut producer = vibescript::Engine::new();
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
  @@n: int = 0
  def self.bump(n: int = 1) -> array<any>
    @@n += n
    [C, @@n, Math.PI]
  end
  def self.enum_value(x: Status = :ready) -> Status
    x
  end
end
class Probe
  def to_s -> string
    JSON.stringify([M.bump(2), M.bump(3), M::A, M.enum_value.name])
  end
end
def make -> any
  M.bump
  [M, Probe.new]
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
    let mut consumer = vibescript::Engine::new();
    consumer.register("host", |_, _| Ok(Value::int(99)));
    let receiver = consumer
        .compile(
            r##"
enum Status
  Other
end
module M
  C = host()
end
def run(input: any) -> array<any>
  items = input.as(array<any>)
  ["#{items.fetch(1)}", items.fetch(0) == M, M::C, Math.PI]
end
"##,
        )
        .unwrap();
    common::scope(|scope| {
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
            let output = json(&job.join().unwrap().value);
            assert_eq!(
                rendered(&output[0]),
                serde_json::json!([
                    [11, 2, std::f64::consts::PI],
                    [11, 5, std::f64::consts::PI],
                    [1],
                    "Ready"
                ])
            );
            assert_eq!(
                output.as_array().unwrap()[1..],
                serde_json::json!([false, 99, std::f64::consts::PI])
                    .as_array()
                    .unwrap()[..]
            );
        }
    });
}

#[test]
fn foreign_instances_preserve_graphs_and_enforce_original_property_types() {
    let mut producer = vibescript::Engine::new();
    producer.register("host", |_, _| Ok(Value::int(10)));
    let source = producer
        .compile(
            r##"
class Node
  @@created: int = 0
  property values: array<int>
  property link: Node?
  def initialize(n: int)
    @values = [n]
    @link = nil
    @@created += 1
  end
  def self.created -> int; @@created; end
  def append(n: int); @values.push(n); end
  def +(n: int) -> int; @values.fetch(0) + n + host().as(int); end
  def [](i: int) -> int; @values.fetch(i); end
  def []=(i: int, n: int); @values[i] = n; end
  def to_s -> string; @values.fetch(0).to_s; end
end
class Graph
  @node: Node
  def initialize(@node: Node)
  end
  def to_s -> string
    before = Node.created
    @node.append(2)
    @node[0] = 8
    fresh = Node.new(4)
    JSON.stringify([@node.values, @node.link == @node, @node + 5, @node[0], "#{@node}",
      format("%s", @node), before, fresh.values, Node.created])
  end
end
def make -> any
  n = Node.new(7)
  n.link = n
  [Node, n, n, Graph.new(n)]
end
"##,
        )
        .unwrap();
    let graph = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    // A class of the same name in the receiving script is a different type,
    // so it never types the foreign instance.
    let receiver = vibescript::Engine::new()
        .compile(
            r##"
class Node
  property values: string
  def initialize(@values: string)
  end
end
def run(input: any) -> array<any>
  items = input.as(array<any>)
  n = items.fetch(1)
  rejected = ""
  begin
    n.as(Node)
  rescue => error
    rejected = error.message
  end
  ["#{items.fetch(3)}", "#{n}", format("%s", n), items.fetch(1) == items.fetch(2), rejected]
end
"##,
        )
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&graph), CallOptions::default())
            .unwrap();
        let output = json(&output.value);
        assert_eq!(
            rendered(&output[0]),
            serde_json::json!([[8, 2], true, 23, 8, "8", "8", 0, [4], 1])
        );
        assert_eq!(
            output.as_array().unwrap()[1..],
            serde_json::json!(["8", "8", true, "cast value expected Node, got instance"])
                .as_array()
                .unwrap()[..]
        );
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A directory of module files, removed when dropped.
struct Files(PathBuf);

impl Files {
    fn new(files: &[(&str, &str)]) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".cache/tmp")
            .join(format!(
                "foreign-{}-{}",
                common::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        for (name, source) in files {
            fs::write(path.join(name), source).unwrap();
        }
        Self(path)
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

#[test]
fn foreign_blocks_use_their_defining_frames_and_unwind_through_ensure() {
    let files = Files::new(&[(
        "bridge.vibe",
        r##"
def invoke(x: int = 4, &block: int -> int) -> int
  begin
    yield(x)
  ensure
    notify()
  end
end
def iterate(&block: int -> int) -> array<int>
  [1, 2, 3].map { |v| yield(v) }
end
def fail -> int
  1 // 0
end
"##,
    )]);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("notify", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let receiver = engine
        .compile(
            r##"
def early -> int
  bridge = require("bridge")
  bridge.invoke { |x| return 9 }
  0
end
def run -> array<int | array<int>>
  bridge = require("bridge")
  n = 10
  a = bridge.invoke(6) { |x| x + n }
  b = bridge.iterate { |v| next v * 2 }
  c = begin
    bridge.fail
  rescue
    7
  end
  [a, b, c, early, bridge.invoke { |x| break 8 }]
end
"##,
        )
        .unwrap();
    let output = receiver
        .call(
            "run",
            &[],
            CallOptions {
                allow_require: true,
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([16, [2, 4, 6], 7, 9, 8])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn identical_namespace_indices_do_not_grant_foreign_protected_access() {
    const CLASS: &str = "class C
  @value: int = 0
  protected def hidden -> int; 7; end
  protected def value=(n: int); @value = n; end
  protected def +(n: int) -> int; 11; end";
    let source = vibescript::Engine::new()
        .compile(&format!(
            "{CLASS}
  def peer(other: C) -> array<int>; [other.hidden, other + 1]; end
  def to_s -> string; peer(self).inspect; end
end
def make -> any; C.new; end"
        ))
        .unwrap();
    let instance = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    // The receiving class declares the same members at the same indices, and
    // reaches them on its own instances, but a foreign instance is not a C.
    for (expression, expected) in [
        ("other.hidden", 7),
        ("other.value = 3", 3),
        ("other + 1", 11),
    ] {
        let receiver = vibescript::Engine::new()
            .compile(&format!(
                "{CLASS}
  def probe(other: C) -> int; {expression}; end
end
def run(other: C) -> int
  C.new.probe(other)
end
def own -> int
  C.new.probe(C.new)
end"
            ))
            .unwrap();
        assert_eq!(
            receiver
                .call(
                    "run",
                    std::slice::from_ref(&instance),
                    CallOptions::default()
                )
                .unwrap_err()
                .kind,
            ErrorKind::Type,
            "{expression}"
        );
        assert_eq!(
            receiver
                .call("own", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(expected),
            "{expression}"
        );
    }
    let receiver = vibescript::Engine::new()
        .compile("def run(other: any) -> string\n  \"#{other}\"\nend")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[instance], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!("[7, 11]")
    );
}

#[test]
fn failed_foreign_initialization_is_catchable_and_retried_only_in_a_new_call() {
    let failing = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let flag = failing.clone();
    let count = calls.clone();
    let mut producer = vibescript::Engine::new();
    producer.register("gate", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "initializer boom"))
        } else {
            Ok(Value::int(3))
        }
    });
    let source = producer
        .compile(
            "module M\n VALUE = gate()\nend\nclass Reader\n def to_s -> string; \"#{M::VALUE}\"; end\nend\ndef make -> any; [M, Reader.new]; end",
        )
        .unwrap();
    let namespace = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut consumer = vibescript::Engine::new();
    consumer.register("fetch", move |_, _| Ok(namespace.clone()));
    let receiver = consumer
        .compile(
            r##"
def run -> array<string>
  errors: array<string> = []
  2.times {
    begin
      m = fetch().as(array<any>)
      "#{m.fetch(1)}"
    rescue => error
      errors.push(error.message)
    end
  }
  errors
end
def discard
  run
end
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
    let producer = vibescript::Engine::new()
        .compile(
            "module M\n def self.fail -> int\n  1 // 0\n end\n def self.spin\n  while true\n  end\n end\nend\nclass Failing\n def to_s -> string; M.fail.to_s; end\nend\nclass Spinning\n def to_s -> string\n  M.spin\n  \"\"\n end\nend\ndef make -> any; [Failing.new, Spinning.new]; end",
        )
        .unwrap();
    let module = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = vibescript::Engine::new()
        .compile(
            "\n\n\n\ndef run(m: any) -> string\n \"#{m.as(array<any>).fetch(0)}\"\nend\ndef spin(m: any) -> string\n begin\n  \"#{m.as(array<any>).fetch(1)}\"\n rescue\n  \"99\"\n end\nend",
        )
        .unwrap();
    let error = receiver
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    let diagnostic = error.diagnostic.unwrap();
    assert_eq!(diagnostic.position.line, 3);
    assert!(diagnostic.code_frame.contains("1 // 0"));
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
    let mut engine_b = vibescript::Engine::new();
    engine_b.register("initialize_b", move |_, _| {
        log.lock().unwrap().push("B");
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "B failed"))
        } else {
            Ok(Value::int(2))
        }
    });
    let source_b = engine_b
        .compile(
            "module B\n VALUE = initialize_b()\nend\nclass Reader\n def to_s -> string; \"#{B::VALUE}\"; end\nend\ndef make -> any; [B, Reader.new]; end",
        )
        .unwrap();
    let b = source_b
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut engine_a = vibescript::Engine::new();
    let imported_b = b.clone();
    engine_a.register("fetch_b", move |_, _| Ok(imported_b.clone()));
    let log = events.clone();
    engine_a.register("initialize_a", move |_, _| {
        log.lock().unwrap().push("A");
        Ok(Value::int(3))
    });
    // A's initializer imports B, whose initializer runs first, and reads
    // B's value through B's reader.
    let source_a = engine_a
        .compile(
            "module A\n PARTIAL = 1\n VALUE = \"#{fetch_b().as(array<any>).fetch(1)}\".to_i + initialize_a().as(int)\nend\nclass Reader\n def to_s -> string; \"#{A::VALUE}\"; end\nend\nclass Partial\n def to_s -> string; \"#{A::PARTIAL}\"; end\nend\ndef make -> any; [A, Reader.new, Partial.new]; end",
        )
        .unwrap();
    let a = source_a
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    events.lock().unwrap().clear();
    let together = vibescript::Engine::new()
        .compile(
            "def run(a: any, b: any) -> array<string>\n [\"#{a.as(array<any>).fetch(1)}\", \"#{b.as(array<any>).fetch(1)}\"]\nend",
        )
        .unwrap();
    let output = together
        .call("run", &[a.clone(), b], CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!(["5", "2"]));
    assert_eq!(*events.lock().unwrap(), ["B", "A"]);
    let mut receiver = vibescript::Engine::new();
    receiver.register("fetch_a", move |_, _| Ok(a.clone()));
    let script = receiver
        .compile(
            r##"
def run -> array<string>
  values: array<string> = []
  2.times {
    begin
      a = fetch_a().as(array<any>)
      values.push("#{a.fetch(1)}")
    rescue => error
      values.push(error.message)
    end
  }
  begin
    values.push("#{fetch_a().as(array<any>).fetch(2)}")
  rescue => error
    values.push(error.message)
  end
  values
end
"##,
        )
        .unwrap();
    events.lock().unwrap().clear();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!(["5", "5", "1"]));
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
    assert_eq!(json(&output.value), serde_json::json!(["5", "5", "1"]));
}

#[test]
fn foreign_initializers_observe_receiving_cancellation_and_memory_limits() {
    let active = Arc::new(AtomicBool::new(false));
    let flag = active.clone();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("initialize", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            token.cancel();
        }
        Ok(Value::nil())
    });
    let producer = engine
        .compile(
            "module M\n initialize()\n VALUES = (1..1024).map { |n| n }\nend\nclass Reader\n def to_s -> string; M::VALUES.length.to_s; end\nend\ndef make -> any; [M, Reader.new]; end",
        )
        .unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = vibescript::Engine::new()
        .compile("def run(m: any) -> int\n \"#{m.as(array<any>).fetch(1)}\".to_i\nend")
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
    let mut engine_b = vibescript::Engine::new();
    engine_b.register("initialize_b", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(17))
    });
    let source_b = engine_b
        .compile(
            "module B\n VALUE = initialize_b()\nend\nclass Reader\n def to_s -> string; \"#{B::VALUE}\"; end\nend\ndef make -> any; [B, Reader.new]; end",
        )
        .unwrap();
    let b = source_b
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    // The holder renders its cycle and B's value, which reads B through the
    // graph that holds it.
    let container = vibescript::Engine::new()
        .compile(
            r##"
class Holder
  property cycle: Holder?
  property link: { entry: array<any> }
  def initialize
    @cycle = nil
    @link = { entry: [] }
  end
  def to_s -> string
    entry = "#{@link["entry"].fetch(0).as(array<any>).fetch(1)}"
    JSON.stringify({ cycle: @cycle == self, entry: entry })
  end
end
def make(b: any) -> any
  h = Holder.new
  h.cycle = h
  h.link = { entry: [b] }
  h
end
"##,
        )
        .unwrap();
    let holder = container
        .call("make", std::slice::from_ref(&b), CallOptions::default())
        .unwrap()
        .value;
    let imported = holder.clone();
    let mut engine_a = vibescript::Engine::new();
    engine_a.register("fetch", move |_, _| Ok(imported.clone()));
    let source_a = engine_a
        .compile(
            "module A\n VALUE = JSON.parse_as(\"#{fetch()}\", { cycle: bool, entry: string })[\"entry\"].to_i + 2\nend\nclass Reader\n def to_s -> string; \"#{A::VALUE}\"; end\nend\ndef make -> any; [A, Reader.new]; end",
        )
        .unwrap();
    let a = source_a
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    calls.store(0, Ordering::SeqCst);
    let receiver = vibescript::Engine::new()
        .compile(
            "def run(a: any, b: any, h: any) -> array<string>\n [\"#{a.as(array<any>).fetch(1)}\", \"#{b.as(array<any>).fetch(1)}\", \"#{h}\"]\nend",
        )
        .unwrap();
    let output = receiver
        .call("run", &[a, b, holder], CallOptions::default())
        .unwrap();
    let output = json(&output.value);
    assert_eq!(
        output.as_array().unwrap()[..2],
        serde_json::json!(["19", "17"]).as_array().unwrap()[..]
    );
    assert_eq!(
        rendered(&output[2]),
        serde_json::json!({"cycle": true, "entry": "17"})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_initialization_blocks_instance_field_writes_without_a_setter() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let mut producer = vibescript::Engine::new();
    producer.register("gate", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            Err(Error::new(ErrorKind::Runtime, "initializer failed"))
        } else {
            Ok(Value::nil())
        }
    });
    let source = producer
        .compile(
            "class C\n gate()\n def to_s -> string; \"allowed\"; end\nend\ndef make -> any; C.new; end",
        )
        .unwrap();
    let instance = source
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut consumer = vibescript::Engine::new();
    consumer.register("fetch", move |_, _| Ok(instance.clone()));
    // A field without a setter cannot be written at all, so the receiver
    // calls the instance's to_s after its class failed to initialize.
    let receiver = consumer
        .compile(
            r##"
def run -> string
  begin
    fetch()
  rescue
    nil
  end
  object = fetch()
  begin
    "#{object}"
  rescue => error
    error.message
  end
end
"##,
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
    let mut engine = vibescript::Engine::new();
    engine.register("initialize", move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let script = engine
        .compile(
            "module M\n VALUE = initialize()\nend\nclass Reader\n def to_s -> string; \"#{M::VALUE}\"; end\nend\ndef make -> any; [M, Reader.new]; end",
        )
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
    let mut engine = vibescript::Engine::new();
    engine.register("rejected", move |_, _| Ok(rejected.clone()));
    engine.register("accepted", move |_, _| Ok(accepted.clone()));
    engine.register("count", move |_, _| {
        Ok(Value::int(count.load(Ordering::SeqCst) as i64))
    });
    let script = engine
        .compile(
            r##"
def inputs(a: any, b: any)
end
def run -> array<any>
  message = ""
  begin
    rejected()
  rescue LimitError => error
    message = error.message
  end
  before = count()
  value = "#{accepted().as(array<any>).fetch(1)}"
  [message, before, value, count()]
end
"##,
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
        serde_json::json!(["value nesting too deep", 0, "7", 1])
    );
}

#[test]
fn host_retention_does_not_initialize_an_unreturned_source() {
    let calls = Arc::new(AtomicUsize::new(0));
    let retained = counted_namespace(&calls);
    let accepted = retained.clone();
    let count = calls.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("retain", move |ctx, _| {
        ctx.import(&retained)?;
        Ok(Value::nil())
    });
    engine.register("accepted", move |_, _| Ok(accepted.clone()));
    engine.register("count", move |_, _| {
        Ok(Value::int(count.load(Ordering::SeqCst) as i64))
    });
    let script = engine
        .compile(
            "def run -> array<any>\n retain()\n [count(), \"#{accepted().as(array<any>).fetch(1)}\", count()]\nend",
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([0, "7", 1]));
}

#[test]
fn foreign_initialization_preserves_argument_and_graph_discovery_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let values: Vec<_> = ["A", "B"]
        .into_iter()
        .map(|label| {
            let events = events.clone();
            let mut engine = vibescript::Engine::new();
            engine.register("initialize", move |_, _| {
                events.lock().unwrap().push(label);
                Ok(Value::nil())
            });
            engine
                .compile("module M\n initialize()\nend\ndef make -> any; M; end")
                .unwrap()
                .call("make", &[], CallOptions::default())
                .unwrap()
                .value
        })
        .collect();
    let script = vibescript::Engine::new()
        .compile(
            "def inputs(a: any, b: any)\nend\ndef graph(x: any)\nend\ndef named(*, a: any, b: any)\nend",
        )
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

#[test]
fn static_types_refuse_calls_on_foreign_code() {
    // A foreign namespace or instance is `any` to the receiving script.
    let source = "def run(m: any) -> any\n  m.bump(2)\nend";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0106"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("bump").unwrap()
    );
}
