use vibescript::{CallOptions, Engine, ErrorKind, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn constructors_methods_aliases_and_shared_identity() {
    let script = Engine::new()
        .compile(
            r#"
class Counter
  @@instances = 0
  property count
  def initialize(@count)
    @@instances += 1
    return "ignored"
  end
  def increment(n: int = 1)
    @count += n
  end
  alias bump increment
  def self.instances
    @@instances
  end
end
def run
  a = Counter.new(10)
  b = a.dup
  b.bump(3)
  [a.count, b.count, a == b, a.class == Counter, Counter.instances]
end
"#,
        )
        .unwrap();
    for _ in 0..2 {
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&result.value),
            serde_json::json!([13, 13, true, true, 1])
        );
        assert_eq!(result.value.as_array().unwrap().len(), 5);
    }
}

#[test]
fn fields_keep_array_snapshots_and_object_identity() {
    let script = Engine::new()
        .compile(
            r#"
class Holder
  def initialize(value)
    @values = [value]
  end
  def append(value)
    @values.push(value)
  end
  def values
    @values
  end
end
def run
  object = Holder.new(1)
  alias = object
  before = object.values
  alias.append(2)
  object.values.push(99)
  [before, object.values, alias.values]
end
"#,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[1], [1, 2], [1, 2]])
    );
}

#[test]
fn unreachable_cycles_are_reclaimed_during_execution() {
    let script = Engine::new()
        .compile(
            r#"
class Node
  property link
end
def run
  for i in 1..5000
    a = Node.new
    b = Node.new
    a.link = [b]
    b.link = {back: a}
  end
  7
end
"#,
        )
        .unwrap();
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(192 * 1024);
    let result = script.call("run", &[], options).unwrap();
    assert_eq!(json(&result.value), serde_json::json!(7));
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn imported_graphs_preserve_cycles_and_isolate_mutation() {
    let script = Engine::new()
        .compile(
            r#"
class Node
  property link, value
end
def make
  a = Node.new
  b = Node.new
  a.value = 1
  b.value = 2
  a.link = b
  b.link = a
  [a, a, b]
end
def read(nodes)
  [nodes[0] == nodes[1], nodes[0].link == nodes[2], nodes[2].link == nodes[0], nodes[0].value]
end
def change(nodes)
  nodes[0].value = 9
  [nodes[1].value, nodes[2].link.value]
end
"#,
        )
        .unwrap();
    let output = script.call("make", &[], CallOptions::default()).unwrap();
    assert!(output.stats.retained_memory_bytes > 0);
    for _ in 0..2 {
        let read = script
            .call(
                "read",
                std::slice::from_ref(&output.value),
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(json(&read.value), serde_json::json!([true, true, true, 1]));
        let changed = script
            .call(
                "change",
                std::slice::from_ref(&output.value),
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(json(&changed.value), serde_json::json!([9, 9]));
    }
}

#[test]
fn long_object_chains_import_without_recursive_rust_calls() {
    let script = Engine::new()
        .compile(
            r#"
class Node
  property link
end
def make
  node = nil
  for i in 1..4096
    current = Node.new
    current.link = node
    node = current
  end
  node
end
def count(node)
  count = 0
  while node
    count += 1
    node = node.link
  end
  count
end
"#,
        )
        .unwrap();
    let output = script.call("make", &[], CallOptions::default()).unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = Some(20_000_000);
    let counted = script.call("count", &[output.value], options).unwrap();
    assert_eq!(json(&counted.value), serde_json::json!(4096));
    assert_eq!(counted.stats.retained_memory_bytes, 0);
}

#[test]
fn constructor_boundaries_remain_guarded() {
    let script = Engine::new()
        .compile(
            r#"
class C
  def initialize -> int
    "ignored"
  end
end
class Empty
end
def construct
  [C.new.class == C, Empty.new(1, 2, x: 3).class == Empty]
end
def hidden
  C.new.initialize
end
"#,
        )
        .unwrap();
    let result = script
        .call("construct", &[], CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([true, true]));
    assert_eq!(
        script
            .call("hidden", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
}

#[test]
fn typed_backing_fields_reject_before_later_effects_and_normalize_enums() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            r#"
enum Status
  Ready
end
class Record
  property status: Status
  getter count: int
  def initialize(@status)
    @count = 1
  end
  def bad(value)
    @count = value
    effect()
  end
end
def good
  record = Record.new(:ready)
  [record.status.symbol, record.count]
end
def bad
  record = Record.new(:ready)
  record.bad("wrong")
end
"#,
        )
        .unwrap();
    let good = script.call("good", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&good.value), serde_json::json!(["ready", 1]));
    assert_eq!(
        script
            .call("bad", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn nominal_class_types_validate_arguments_returns_and_fields() {
    let script = Engine::new()
        .compile(
            r#"
class Node
  property link: Node?
  def initialize(@link = nil)
  end
end
class Other
end
def identity(value: Node) -> Node
  value
end
def good
  a = Node.new
  b = Node.new(a)
  identity(b).link == a
end
def wrong_field
  Node.new(Other.new)
end
def wrong_argument
  identity(Other.new)
end
"#,
        )
        .unwrap();
    let result = script.call("good", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!(true));
    for function in ["wrong_field", "wrong_argument"] {
        assert_eq!(
            script
                .call(function, &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
    }
}

#[test]
fn negative_property_paths_preserve_parent_growth_and_enforce_nested_types() {
    for (value, expected) in [
        ("2", serde_json::json!(["accepted", [[1, 2], [9]], [[1]]])),
        (
            "\"bad\"",
            serde_json::json!(["rejected", [[1], [9]], [[1]]]),
        ),
    ] {
        let source = format!(
            "class C;getter rows:array<array<int>>;def initialize;@rows=[[1]];end;\
             def run;before=@rows;status=begin;\
             @rows[-1].push((while true;@rows.push([9]);break {value};end));\
             :accepted;rescue;:rejected;end;[status,@rows,before];end;end;C.new.run"
        );
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(json(&result.value), expected, "{source}");
        if value == "\"bad\"" {
            let unhandled = source.replace("rescue;:rejected", "rescue;raise");
            let error = Engine::new()
                .compile(&unhandled)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Type);
            assert!(
                error
                    .message
                    .starts_with("instance variable @rows expected")
            );
        }
    }
}

#[test]
fn rejected_nested_property_mutations_preserve_the_previous_field() {
    use std::sync::{Arc, Mutex};
    let captured = Arc::new(Mutex::new(None));
    let saved = captured.clone();
    let mut engine = Engine::new();
    engine.register("capture", move |_, args| {
        *saved.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            r#"
class Holder
  getter values: array<int>
  def initialize
    @values = [1]
  end
  def bad
    @values.push("wrong")
  end
end
def run
  object = Holder.new
  capture(object)
  object.bad
end
def read(object)
  object.values
end
"#,
        )
        .unwrap();
    let error = script.call("run", &[], CallOptions::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{error}");
    let object = captured.lock().unwrap().take().unwrap();
    let result = script
        .call("read", &[object], CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1]));
}

#[test]
fn instance_assignments_shadow_class_constants_and_preserve_visibility_locals() {
    let script = Engine::new()
        .compile(
            r#"
class Counter
  TOTAL = 2
  protected = 5
  protected
  def local
    TOTAL ||= 9
    TOTAL += 1
    TOTAL
  end
  def self.shared
    TOTAL += 1
  end
  private
end
def run
  [Counter.new.local, Counter.TOTAL, Counter.shared, Counter.TOTAL]
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([10, 2, 3, 3]));
}

#[test]
fn cancelled_and_exhausted_constructors_stop_effects_and_preserve_captured_objects() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use vibescript::CancellationToken;
    for exhaustion in [false, true] {
        let token = CancellationToken::new();
        let cancelled = token.clone();
        let captured = Arc::new(Mutex::new(None));
        let saved = captured.clone();
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let mut engine = Engine::new();
        engine.register("capture", move |_, args| {
            *saved.lock().unwrap() = Some(args[0].clone());
            Ok(Value::nil())
        });
        engine.register("stop", move |ctx, _| {
            if exhaustion {
                let _ = ctx.charge(u64::MAX);
            } else {
                cancelled.cancel();
                let _ = ctx.checkpoint();
            }
            Ok(Value::nil())
        });
        engine.register("effect", move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        });
        let script = engine
            .compile(
                r#"
class Node
  property link, value
  def initialize(n)
    @value = n
    @link = self
    if n == 1
      capture(self)
    end
    if n == 1000
      stop()
      effect()
    end
  end
end
def run
  for n in 1..1000
    Node.new(n)
  end
  effect()
end
def read(node)
  [node.value, node.link == node]
end
"#,
            )
            .unwrap();
        let mut options = CallOptions {
            cancellation: token,
            ..CallOptions::default()
        };
        options.limits.memory_bytes = Some(192 * 1024);
        let error = script.call("run", &[], options).unwrap_err();
        assert_eq!(
            error.kind,
            if exhaustion {
                ErrorKind::Steps
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(effects.load(Ordering::Relaxed), 0);
        let object = captured.lock().unwrap().take().unwrap();
        let output = script
            .call("read", &[object], CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), serde_json::json!([1, true]));
    }
}

#[test]
fn concurrent_calls_import_independent_objects_and_class_state() {
    let script = Engine::new()
        .compile(
            r#"
class Counter
  @@calls = 0
  property value, link
  def initialize(@value = 0)
    @link = self
  end
  def increment(n)
    @@calls += 1
    @value += n
    [@value, @@calls, @link == self]
  end
end
def make
  Counter.new(10)
end
def change(counter, n)
  counter.increment(n)
end
"#,
        )
        .unwrap();
    let initial = script.call("make", &[], CallOptions::default()).unwrap();
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (1..=8)
            .map(|n| {
                let value = initial.value.clone();
                let script = &script;
                scope.spawn(move || {
                    let result = script
                        .call("change", &[value, Value::int(n)], CallOptions::default())
                        .unwrap();
                    assert_eq!(json(&result.value), serde_json::json!([10 + n, 1, true]));
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap();
        }
    });
    let unchanged = script
        .call(
            "change",
            &[initial.value, Value::int(0)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(json(&unchanged.value), serde_json::json!([10, 1, true]));
}

#[test]
fn incoming_instance_containers_preserve_cycles_aliases_and_call_isolation() {
    let producer = Engine::new()
        .compile(
            r#"
class Node
  property links, value
  def initialize(@value)
    @links = []
  end
end
def make
  a = Node.new(1)
  b = Node.new(2)
  a.links = [{next: b, again: b}]
  b.links = [{next: a}]
  a
end
"#,
        )
        .unwrap();
    let original = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let source = r#"
def visit(a)
  b = a.links[0][:next]
  b.links[0][:next].value = 4
  [a.value, b.value, b == a.links[0][:again], b.links[0][:next] == a]
end
def positional(a); visit(a); end
def keyword(a:); visit(a); end
def global; visit(incoming); end
def host; visit(fetch()); end
"#;
    let supplied = original.clone();
    let mut engine = Engine::new();
    engine.register("fetch", move |_, _| Ok(supplied.clone()));
    let receiver = engine.compile(source).unwrap();
    for unlimited in [false, true] {
        let mut options = CallOptions::default();
        if unlimited {
            options.limits.memory_bytes = None;
        }
        let outputs = [
            receiver.call(
                "positional",
                std::slice::from_ref(&original),
                options.clone(),
            ),
            receiver.call_with_keywords(
                "keyword",
                &[],
                &[("a".into(), original.clone())],
                options.clone(),
            ),
            receiver.call(
                "global",
                &[],
                CallOptions {
                    globals: [("incoming".into(), original.clone())].into(),
                    ..options.clone()
                },
            ),
            receiver.call("host", &[], options),
        ];
        for output in outputs {
            let output = output.unwrap();
            assert_eq!(json(&output.value), serde_json::json!([4, 2, true, true]));
        }
    }
    let read = Engine::new()
        .compile("def read(a);[a.value,a.links[0][:next].value];end")
        .unwrap();
    assert_eq!(
        json(
            &read
                .call("read", &[original], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([1, 2])
    );
}

#[test]
fn instance_methods_write_class_constants_in_place() {
    let script = Engine::new()
        .compile(
            r#"
class Consts
  LIST = [1, [2, 3]]
  H = {a: 1}
  def poke() -> int
    LIST[0] = 9
    LIST[1][0] = 20
    H[:b] = 2
    H.a = 5
    [1].each { LIST[0] += 1 }
    LIST[0]
  end
  def shadow
    LIST = [3]
    LIST[0] = 4
    LIST.push(5)
    LIST
  end
  def mutate
    LIST.push(6)
    LIST.size
  end
  def self.read
    [LIST, H]
  end
end
def run
  c = Consts.new
  [c.poke, c.shadow, c.mutate, Consts.read, Consts::LIST]
end
def exact -> int
  if Consts.new.poke == 10
    7
  else
    "wrong"
  end
end
"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([
                10,
                [4, 5],
                2,
                [[10, [20, 3]], {"a": 5, "b": 2}],
                [10, [20, 3]]
            ])
        );
    }
    // The checker writes through the same class field, so the updated element is known.
    let report = script
        .check_function("exact", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    let output = script.call("exact", &[], CallOptions::default()).unwrap();
    assert_eq!(output.value.as_int(), Some(7));
}

#[test]
fn block_break_replaces_the_constructor_result() {
    let script = Engine::new()
        .compile(
            r#"
class Built
  def initialize() -> int
    begin
      yield
    ensure
      @cleaned = true
    end
    5
  end
  def self.make
    new { break 9 }
  end
end
def run
  [Built.new { break 7 }, Built.new { break }, Built.new { break "x" }, Built.make,
   Built.new { 1 }.class == Built]
end
def typed -> int
  Built.new { break 7 }
end
def mistyped -> int
  Built.new { break "x" }
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([7, null, "x", 9, true])
    );
    let report = script
        .check_function("typed", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    let report = script
        .check_function("mistyped", &CallOptions::default())
        .unwrap();
    assert!(
        report.diagnostics[0].message.contains("Return value"),
        "{report:?}"
    );
    let error = script
        .call("mistyped", &[], CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("expected int"), "{}", error.message);
}
