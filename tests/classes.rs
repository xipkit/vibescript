mod common;

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
  @@instances: int = 0
  property count: int
  def initialize(@count: int)
    @@instances += 1
  end
  def increment(n: int = 1) -> int
    @count += n
  end
  alias bump increment
  def self.instances -> int
    @@instances
  end
end
def run -> array<bool | int>
  a = Counter.new(10)
  b = a.dup
  b.bump(3)
  [a.count, b.count, a == b, a.is_type?(:Counter), Counter.instances]
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
  @values: array<int>
  def initialize(value: int)
    @values = [value]
  end
  def append(value: int) -> array<int>
    @values.push(value)
  end
  def values -> array<int>
    @values
  end
end
def run -> array<array<int>>
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
  property link: array<Node> | { back: Node }
end
def run -> int
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
    // The typed property's writes are checked at runtime, which takes more
    // than the default step quota.
    options.limits.steps = Some(5_000_000);
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
  property link: Node, value: int
end
def make -> array<Node>
  a = Node.new
  b = Node.new
  a.value = 1
  b.value = 2
  a.link = b
  b.link = a
  [a, a, b]
end
def read(nodes: array<Node>) -> array<bool | int>
  [nodes[0] == nodes[1], nodes.fetch(0).link == nodes[2], nodes.fetch(2).link == nodes[0], nodes.fetch(0).value]
end
def change(nodes: array<Node>) -> array<int>
  nodes.fetch(0).value = 9
  [nodes.fetch(1).value, nodes.fetch(2).link.value]
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
  property link: Node?
end
def make -> Node?
  node: Node? = nil
  for i in 1..4096
    current = Node.new
    current.link = node
    node = current
  end
  node
end
def count(node: Node?) -> int
  count = 0
  current = node
  while current != nil
    count += 1
    current = current.link
  end
  count
end
"#,
        )
        .unwrap();
    // The typed property's writes are checked at runtime, which takes more
    // than the default step quota.
    let mut options = CallOptions::default();
    options.limits.steps = Some(5_000_000);
    let output = script.call("make", &[], options).unwrap();
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
end
class Empty
end
def construct -> array<bool>
  [C.new.is_type?(:C), Empty.new.is_type?(:Empty)]
end
"#,
        )
        .unwrap();
    let result = script
        .call("construct", &[], CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([true, true]));
    // A constructor's result, arguments a class does not take, and calling
    // `initialize` directly are refused before running.
    for (source, expected) in [
        (
            "class C\n  def initialize -> int\n    \"ignored\"\n  end\nend\nC.new",
            vec![("V0101", "\"ignored\"")],
        ),
        (
            "class C\n  def initialize\n    return \"ignored\"\n  end\nend\nC.new",
            vec![("V0117", "return")],
        ),
        (
            "class Empty\nend\nEmpty.new(1, 2, x: 3)",
            vec![("V0301", "new"), ("V0302", "x:")],
        ),
        (
            "class C\nend\nC.new.initialize",
            vec![("V0203", "initialize")],
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        let found: Vec<(String, usize)> = error
            .diagnostics()
            .iter()
            .map(|d| (d.code.to_string(), d.span.start))
            .collect();
        let expected: Vec<(String, usize)> = expected
            .into_iter()
            .map(|(code, text)| (code.to_owned(), source.find(text).unwrap()))
            .collect();
        assert_eq!(found, expected, "{source}");
    }
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
  def initialize(@status: Status)
    @count = 1
  end
  def bad(value: any)
    @count = value.as(int)
    effect()
  end
end
def good -> array<int | symbol>
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
    // A dynamic value is checked where it is narrowed, before later effects.
    assert_eq!(
        script
            .call("bad", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    // A value of the wrong type is refused before running.
    let source = "class Record\n  getter count: int\n  def initialize\n    @count = 1\n  end\n  def bad(value: string)\n    @count = value\n  end\nend";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.rfind("value").unwrap()
    );
}

#[test]
fn nominal_class_types_validate_arguments_returns_and_fields() {
    let script = Engine::new()
        .compile(
            r#"
class Node
  property link: Node?
  def initialize(@link: Node? = nil)
  end
end
class Other
end
def identity(value: Node) -> Node
  value
end
def good -> bool
  a = Node.new
  b = Node.new(a)
  identity(b).link == a
end
def other -> Other
  Other.new
end
"#,
        )
        .unwrap();
    let result = script.call("good", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!(true));
    // A host argument of another class is refused when the call starts.
    let other = script
        .call("other", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(
        script
            .call("identity", &[other], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    // Inside the program, classes are checked before it runs.
    for (call, text) in [
        ("Node.new(Other.new)", "Other.new"),
        ("identity(Other.new)", "Other.new"),
    ] {
        let source = format!(
            "class Node\n  property link: Node?\n  def initialize(@link: Node? = nil)\n  end\nend\nclass Other\nend\ndef identity(value: Node) -> Node\n  value\nend\n{call}"
        );
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0101"], "{call}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.rfind(text).unwrap(),
            "{call}"
        );
    }
}

#[test]
fn negative_property_paths_preserve_parent_growth_and_enforce_nested_types() {
    let source = |value: &str| {
        format!(
            "class C;getter rows:array<array<int>>;def initialize;@rows=[[1]];end;\
             def run -> array<any>;before=@rows;status=begin;\
             @rows[-1]&.push((while true;@rows.push([9]);break {value};end));\
             :accepted;rescue;:rejected;end;[status,@rows,before];end;end;C.new.run"
        )
    };
    let result = Engine::new()
        .compile(&source("2"))
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["accepted", [[1, 2], [9]], [[1]]])
    );
    // An element of the wrong type is refused before running.
    let bad = source("\"bad\"");
    let error = vibescript::Engine::new().compile(&bad).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        bad.find("while true").unwrap()
    );
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
    // A dynamic value is checked where it is narrowed, before the push.
    let script = engine
        .compile(
            r#"
class Holder
  getter values: array<int>
  def initialize
    @values = [1]
  end
  def bad(value: any) -> array<int>
    @values.push(value.as(int))
  end
end
def run -> array<int>
  object = Holder.new
  capture(object)
  object.bad("wrong")
end
def read(object: Holder) -> array<int>
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
    // A value of the wrong type is refused before running.
    let source = "class Holder\n  getter values: array<int>\n  def initialize\n    @values = [1]\n  end\n  def bad\n    @values.push(\"wrong\")\n  end\nend";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("\"wrong\"").unwrap()
    );
}

#[test]
fn instance_assignments_shadow_class_constants_and_preserve_visibility_locals() {
    // A method cannot assign a capitalized name, so it cannot shadow a
    // constant with a local.
    let source =
        "class Counter\n  TOTAL = 2\n  def local -> int\n    TOTAL = 9\n    TOTAL\n  end\nend\n";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0102"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("TOTAL = 9").unwrap()
    );
    let script = Engine::new()
        .compile(
            r#"
class Counter
  TOTAL = 2
  @@shared: int = 2
  protected = 5
  protected
  def local -> int
    total = TOTAL + 7
    total += 1
    total
  end
  def self.shared -> int
    @@shared += 1
  end
  private
end
def run -> array<int>
  [Counter.new.local, Counter.TOTAL, Counter.shared, Counter.TOTAL]
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([10, 2, 3, 2]));
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
  property link: Node, value: int
  def initialize(n: int)
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
def run -> any
  for n in 1..1000
    Node.new(n)
  end
  effect()
end
def read(node: Node) -> array<bool | int>
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
  @@calls: int = 0
  property value: int, link: Counter
  def initialize(@value: int = 0)
    @link = self
  end
  def increment(n: int) -> array<bool | int>
    @@calls += 1
    @value += n
    [@value, @@calls, @link == self]
  end
end
def make -> Counter
  Counter.new(10)
end
def change(counter: Counter, n: int) -> array<bool | int>
  counter.increment(n)
end
"#,
        )
        .unwrap();
    let initial = script.call("make", &[], CallOptions::default()).unwrap();
    common::scope(|scope| {
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
    use std::sync::{Arc, Mutex};
    // A class is named only in its own script, so the instances come back
    // to the script that made them.
    let source = r#"
class Node
  property links: array<{ next: Node, again?: Node }>, value: int
  def initialize(@value: int)
    @links = []
  end
end
def make -> Node
  a = Node.new(1)
  b = Node.new(2)
  a.links = [{next: b, again: b}]
  b.links = [{next: a}]
  a
end
def visit(a: Node) -> array<any>
  b = a.links.fetch(0)["next"]
  b.links.fetch(0)["next"].value = 4
  [a.value, b.value, b == a.links.fetch(0)["again"], b.links.fetch(0)["next"] == a]
end
def positional(a: Node) -> array<any>; visit(a); end
def keyword(*, a: Node) -> array<any>; visit(a); end
def global -> array<any>; visit(incoming.as(Node)); end
def host -> array<any>; visit(fetch().as(Node)); end
def read(a: Node) -> array<int>; [a.value, a.links.fetch(0)["next"].value]; end
"#;
    let supplied: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::nil()));
    let held = supplied.clone();
    let mut engine = Engine::new();
    engine.register("fetch", move |_, _| Ok(held.lock().unwrap().clone()));
    engine.declare_global("incoming", "").unwrap();
    let script = engine.compile(source).unwrap();
    let with = |value: &Value, options: CallOptions| CallOptions {
        globals: [("incoming".into(), value.clone())].into(),
        ..options
    };
    let original = script
        .call("make", &[], with(&Value::nil(), CallOptions::default()))
        .unwrap()
        .value;
    *supplied.lock().unwrap() = original.clone();
    for unlimited in [false, true] {
        let mut options = with(&original, CallOptions::default());
        if unlimited {
            options.limits.memory_bytes = None;
        }
        let outputs = [
            script.call(
                "positional",
                std::slice::from_ref(&original),
                options.clone(),
            ),
            script.call_with_keywords(
                "keyword",
                &[],
                &[("a".into(), original.clone())],
                options.clone(),
            ),
            script.call("global", &[], options.clone()),
            script.call("host", &[], options),
        ];
        for output in outputs {
            let output = output.unwrap();
            assert_eq!(json(&output.value), serde_json::json!([4, 2, true, true]));
        }
    }
    assert_eq!(
        json(
            &script
                .call(
                    "read",
                    std::slice::from_ref(&original),
                    with(&original, CallOptions::default())
                )
                .unwrap()
                .value
        ),
        serde_json::json!([1, 2])
    );
}

#[test]
fn partial_namespace_snapshots_never_read_missing_class_variables_as_nil() {
    use std::sync::{Arc, Mutex};
    let saved = Arc::new(Mutex::new(None));
    let capture = saved.clone();
    let mut engine = Engine::new();
    engine.register("capture", move |_, args| {
        *capture.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            "class C
        def read -> int; @@n; end
        def bump -> int; @@n += 1; end
        capture(C.new)
        @@n: int = 7
    end
    def read(value: C) -> int; value.read; end
    def bump(value: C) -> int; value.bump; end
    C.new.read",
        )
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap().value.as_int(),
        Some(7)
    );
    let value = saved.lock().unwrap().take().unwrap();
    for function in ["read", "bump"] {
        let error = script
            .call(
                function,
                std::slice::from_ref(&value),
                CallOptions::default(),
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Runtime);
        assert!(
            error
                .message
                .contains("class variable @@n is not initialized"),
            "{error}"
        );
    }
}

/// Steps a call of `run` takes.
fn steps(source: &str) -> u64 {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}\ndoes not compile: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}\nfails: {error}"))
        .stats
        .steps
}

/// The steps one more call of `value` adds to `run` in a program declaring
/// `class`.
fn steps_per_call(class: &str, construct: &str) -> u64 {
    let run = |calls: usize| {
        let reads = vec!["c.value"; calls].join(" + ");
        format!("{class}\ndef run -> int\n  c = {construct}\n  {reads}\nend\n")
    };
    steps(&run(2)) - steps(&run(1))
}

#[test]
fn methods_of_classes_that_assign_every_field_first_skip_their_result_check() {
    let method = "  def value -> int\n    @count\n  end\n";
    // `initialize` assigns `@count` before any method can read it.
    let proven = steps_per_call(
        &format!(
            "class Counter\n  property count: int\n  def initialize\n    @count = 1\n  end\n{method}end"
        ),
        "Counter.new",
    );
    // Without `initialize`, a method may read `@count` before a setter
    // assigns it, so the runtime keeps checking the result.
    let unproven = steps_per_call(
        &format!("class Counter\n  property count: int\n{method}end"),
        "Counter.new\n  c.count = 1",
    );
    assert!(proven < unproven, "{proven} >= {unproven}");
    // A class whose `initialize` calls a method on `self` that may read a
    // field it has not assigned yet, even on a path this call skips, does
    // not compile.
    let early = format!(
        "class Counter\n  @count: int\n  def initialize\n    @count = pick(false)\n  end\n  def pick(read: bool) -> int\n    if read\n      value\n    else\n      1\n    end\n  end\n{method}end"
    );
    let error = Engine::new().compile(&early).err().unwrap();
    assert_eq!(error.diagnostics()[0].code.to_string(), "V0205");
}

#[test]
fn a_field_of_a_class_without_initialize_fails_where_a_method_returns_it() {
    // No `initialize` assigns the field, so its methods keep their result
    // check.
    let source =
        "class C\n  @x: int\n  def x -> int\n    @x\n  end\nend\ndef run -> int\n  C.new.x\nend\n";
    let error = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}\ndoes not compile: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}");
    assert!(
        error
            .message
            .contains("return value for x expected int, got nil"),
        "{source}\n{}",
        error.message
    );
}

#[test]
fn a_field_read_before_initialize_assigns_it_does_not_compile() {
    for (source, at) in [
        // `initialize` reads it through a method before assigning it.
        (
            "class C\n  @x: int\n  def initialize\n    @x = peek\n  end\n  def peek -> int\n    @x\n  end\n  def x -> int\n    @x\n  end\nend",
            "peek\n  end\n  def peek",
        ),
        // It reads it directly.
        (
            "class C\n  @x: int\n  @y: int\n  def initialize\n    @y = @x\n    @x = 1\n  end\n  def x -> int\n    @y\n  end\nend",
            "@x\n    @x = 1",
        ),
        // It passes `self` on before assigning it.
        (
            "def show(c: C) -> int\n  c.x\nend\nclass C\n  getter x: int\n  def initialize\n    @x = show(self)\n  end\nend",
            "self)",
        ),
        // A default calls a method that reads a field declared after it.
        (
            "class C\n  @y: int = peek\n  @x: int = 1\n  def peek -> int\n    @x\n  end\n  def x -> int\n    @y\n  end\nend",
            "peek\n  @x",
        ),
    ] {
        let source = format!("{source}\ndef run -> int\n  C.new.x\nend\n");
        let error = Engine::new().compile(&source).err().unwrap();
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code.to_string(), "V0205", "{source}");
        assert_eq!(diagnostic.span.start, source.find(at).unwrap(), "{source}");
    }
}
