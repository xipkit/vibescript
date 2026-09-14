use vibescript::{CallOptions, Engine, ErrorKind, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn operators_preserve_left_dispatch_and_publish_compound_results() {
    let script = Engine::new()
        .compile(
            r##"
class Boxed
  property value: int
  def initialize(@value)
  end
  def +(other)
    Boxed.new(@value + other)
  end
  def *(other)
    @value * other
  end
  def ==(other)
    @value == other.value
  end
end
class Holder
  property value: Boxed
end
def run
  a = Boxed.new(3)
  original = a
  a += 2
  a = a + 4
  holder = Holder.new
  holder.value = a
  holder.value += 1
  [original.value, a.value, holder.value.value, a * 3,
   a == Boxed.new(9), a != Boxed.new(8), a.equal?(Boxed.new(9))]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([3, 9, 10, 27, true, true, false])
    );
}

#[test]
fn equality_fallback_negates_truthiness_and_explicit_inequality_wins() {
    let script = Engine::new()
        .compile(
            r##"
class Truthy
  def ==(other)
    []
  end
end
class Falsey
  def ==(other)
    nil
  end
end
class Explicit
  def ==(other)
    true
  end
  def !=(other)
    "own result"
  end
end
class Plain
end
def run
  plain = Plain.new
  [Truthy.new == 1, Truthy.new != 1, Falsey.new != 1,
   Explicit.new != 1, plain == plain, plain != Plain.new]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[], false, true, "own result", true, true])
    );
}

#[test]
fn index_methods_preserve_assignment_results_and_collection_snapshots() {
    let script = Engine::new()
        .compile(
            r##"
class Grid
  def initialize
    @slots = {}
  end
  def [](row, col)
    @slots.fetch("#{row}:#{col}", nil)
  end
  def []=(row, col, value)
    @slots["#{row}:#{col}"] = value
    "ignored"
  end
end
class Item
  property value
end
def assign(grid)
  grid[1, 2] = 4
end
def run
  grid = Grid.new
  assigned = assign(grid)
  grid[1, 2] += 5
  grid[3, 4] ||= 7
  grid[4, 5] &&= 9
  grid[0, 0] = [1]
  saved = grid[0, 0]
  grid[0, 0].push(2)
  grid[0, 0][0] = 8
  item = Item.new
  grid[5, 6] = item
  grid[5, 6].value = 12
  [assigned, grid[1, 2], grid[3, 4], grid[4, 5], saved, grid[0, 0], item.value]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([4, 9, 7, null, [1], [1], 12])
    );
}

#[test]
fn shovel_dispatches_its_own_method_and_preserves_array_behavior() {
    let script = Engine::new()
        .compile(
            r##"
class Bag
  getter items
  def initialize
    @items = []
  end
  def <<(value)
    @items.push(value)
    self
  end
  def push(value)
    @items.push(value * 10)
    self
  end
end
def run
  bag = Bag.new
  bag << 1 << 2
  bag.push(3)
  array = [1]
  original = array
  array << 2 << 3
  [bag.items, array, original]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[1, 2, 30], [1, 2], [1]])
    );
}

#[test]
fn index_assignment_orders_effects_and_evaluates_each_target_once() {
    let script = Engine::new()
        .compile(
            r##"
class Recorder
  getter events
  def initialize
    @events = []
    @value = 1
  end
  def receiver
    @events.push("receiver")
    self
  end
  def selector
    @events.push("selector")
    0
  end
  def rhs
    @events.push("rhs")
    2
  end
  def [](index)
    @events.push("get")
    @value
  end
  def []=(index, value)
    @events.push("set")
    @value = value
    99
  end
end
def plain_write(r)
  r.receiver[r.selector] = r.rhs
end
def compound_write(r)
  r.receiver[r.selector] += r.rhs
end
def plain
  r = Recorder.new
  value = plain_write(r)
  [value, r.events]
end
def compound
  r = Recorder.new
  value = compound_write(r)
  [value, r.events]
end
"##,
        )
        .unwrap();
    for (function, expected) in [
        (
            "plain",
            serde_json::json!([2, ["rhs", "receiver", "selector", "set"]]),
        ),
        (
            "compound",
            serde_json::json!([3, ["receiver", "selector", "get", "rhs", "set"]]),
        ),
    ] {
        let result = script.call(function, &[], CallOptions::default()).unwrap();
        assert_eq!(json(&result.value), expected);
    }
}

#[test]
fn operator_visibility_and_nonlocal_control_remain_call_boundaries() {
    let script = Engine::new()
        .compile(
            r##"
class Hidden
  private def +(other)
    1
  end
end
class Related
  protected def [](index)
    index + 2
  end
  def read(other)
    other[3]
  end
end
class Leaky
  def +(other)
    break 3
  end
  def [](index)
    next 4
  end
  def []=(index, value)
    break 5
  end
end
def hidden
  Hidden.new + 1
end
def protected_outside
  Related.new[1]
end
def protected_inside
  Related.new.read(Related.new)
end
def bad_plus
  for n in [1, 2]
    Leaky.new + n
  end
end
def bad_get
  for n in [1, 2]
    Leaky.new[n]
  end
end
def bad_set
  object = Leaky.new
  for n in [1, 2]
    object[n] = n
  end
end
"##,
        )
        .unwrap();
    let result = script
        .call("protected_inside", &[], CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!(5));
    for function in ["hidden", "protected_outside"] {
        assert_eq!(
            script
                .call(function, &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name
        );
    }
    for function in ["bad_plus", "bad_get", "bad_set"] {
        assert_eq!(
            script
                .call(function, &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
    }
}

#[test]
fn interpolation_uses_optional_private_conversions_and_keeps_container_rendering() {
    let script = Engine::new()
        .compile(
            r##"
class Named
  def initialize(@name)
  end
  private def to_s(prefix = "name", suffix: "!")
    "#{prefix}=#{@name}#{suffix}"
  end
end
class Required
  def to_s(value)
    "unreachable"
  end
end
class NonString
  def to_s
    [self]
  end
end
def run
  named = Named.new("Ada")
  ["a#{named}b", "#{[named]}", "#{Required.new}", "#{NonString.new}"]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            "aname=Ada!b",
            "[<Named instance>]",
            "<Required instance>",
            "<NonString instance>"
        ])
    );
    assert!(result.stats.retained_memory_bytes < 1024);
}

#[test]
fn recursive_operators_and_conversions_exhaust_vm_recursion() {
    for body in ["Recursive.new + 1", "\"#{Recursive.new}\""] {
        let script = Engine::new()
            .compile(&format!(
                r##"
class Recursive
  def +(other)
    self + other
  end
  def to_s
    "#{{self}}"
  end
end
def run
  {body}
end
"##
            ))
            .unwrap();
        let mut options = CallOptions::default();
        options.limits.recursion = 32;
        assert_eq!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Recursion
        );
    }
}

#[test]
fn cancellation_and_exhaustion_stop_operator_and_conversion_effects() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vibescript::CancellationToken;
    for operation in [
        "c + 1",
        "c += 1",
        "c = c + 1",
        "c[0]",
        "c[0] = 1",
        "c[0] += 1",
        "c[0].push(1)",
        "c << 1",
        "\"#{c}\"",
    ] {
        for exhaust in [false, true] {
            let token = CancellationToken::new();
            let cancelled = token.clone();
            let effects = Arc::new(AtomicUsize::new(0));
            let observed = effects.clone();
            let mut engine = Engine::new();
            engine.register("stop", move |ctx, _| {
                if exhaust {
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
            let source = format!(
                r##"
class C
  def +(value)
    stop()
    effect()
  end
  alias_method :"<<", :"+"
  alias_method :"[]", :"+"
  def []=(key, value)
    stop()
    effect()
  end
  def to_s
    stop()
    effect()
    "ignored"
  end
end
def run
  c = C.new
  {operation}
  effect()
end
"##
            );
            let script = engine.compile(&source).unwrap();
            let options = CallOptions {
                cancellation: token,
                ..CallOptions::default()
            };
            assert_eq!(
                script.call("run", &[], options).unwrap_err().kind,
                if exhaust {
                    ErrorKind::Steps
                } else {
                    ErrorKind::Cancelled
                },
                "{operation}"
            );
            assert_eq!(effects.load(Ordering::Relaxed), 0, "{operation}");
        }
    }
}

#[test]
fn repeated_operator_results_release_old_instances_and_conversion_storage() {
    let script = Engine::new()
        .compile(
            r##"
class Counter
  def initialize(@value)
    @payload = "x" * 512
  end
  def +(value)
    Counter.new(@value + value)
  end
  def to_s
    "v=#{@value}"
  end
end
def run
  counter = Counter.new(0)
  initial = counter
  for i in 1..2000
    counter += 1
  end
  ["#{initial}", "#{counter}"]
end
"##,
        )
        .unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = Some(5_000_000);
    options.limits.memory_bytes = Some(64 * 1024);
    let result = script.call("run", &[], options).unwrap();
    assert_eq!(json(&result.value), serde_json::json!(["v=0", "v=2000"]));
    assert!(
        result.stats.retained_memory_bytes < 1024,
        "{:?}",
        result.stats
    );
}

#[test]
fn typed_return_failures_preserve_prior_method_effects_and_stop_the_caller() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let saved = Arc::new(Mutex::new(None));
    let captured = saved.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("capture", move |_, args| {
        *captured.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            r##"
class C
  getter state
  def initialize
    @state = 0
  end
  def +(value) -> int
    @state += 1
    "bad return"
  end
  def []=(key, value) -> int
    @state = value
    "bad return"
  end
end
def add
  c = C.new
  capture(c)
  c += 1
  effect()
end
def write
  c = C.new
  capture(c)
  c[0] = 7
  effect()
end
def read(c)
  c.state
end
"##,
        )
        .unwrap();
    for (function, value) in [("add", 1), ("write", 7)] {
        assert_eq!(
            script
                .call(function, &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
        assert_eq!(effects.load(Ordering::Relaxed), 0);
        let captured = saved.lock().unwrap().take().unwrap();
        let result = script
            .call("read", &[captured], CallOptions::default())
            .unwrap();
        assert_eq!(json(&result.value), serde_json::json!(value));
    }
}
