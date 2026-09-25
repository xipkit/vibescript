mod common;

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
  def initialize(@value: int)
  end
  def +(other: int) -> Boxed
    Boxed.new(@value + other)
  end
  def *(other: int) -> int
    @value * other
  end
  def ==(other: Boxed) -> bool
    @value == other.value
  end
end
class Holder
  property value: Boxed
end
def run -> array<bool | int>
  a = Boxed.new(3)
  original = a
  a += 2
  a = a + 4
  holder = Holder.new
  holder.value = a
  holder.value += 1
  [original.value, a.value, holder.value.value, a * 3,
   a == Boxed.new(9), a != Boxed.new(8)]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([3, 9, 10, 27, true, true])
    );
}

#[test]
fn equality_fallback_negates_truthiness_and_explicit_inequality_wins() {
    let script = Engine::new()
        .compile(
            r##"
class Truthy
  def ==(other: int) -> array<int>
    []
  end
end
class Falsey
  def ==(other: int) -> nil
    nil
  end
end
class Explicit
  def ==(other: any) -> bool
    true
  end
  def !=(other: int) -> string
    "own result"
  end
end
class Plain
end
def run -> array<bool | string | array<any>>
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
  @slots: hash<string, int>
  def initialize
    @slots = {}
  end
  def [](row: int, col: int) -> int
    @slots.fetch("#{row}:#{col}", 0)
  end
  def []=(row: int, col: int, value: int)
    @slots["#{row}:#{col}"] = value
    "ignored"
  end
end
class Rows
  @slots: hash<string, array<int>>
  def initialize
    @slots = {}
  end
  def [](row: int, col: int) -> array<int>
    @slots.fetch("#{row}:#{col}")
  end
  def []=(row: int, col: int, value: array<int>)
    @slots["#{row}:#{col}"] = value
  end
end
class Items
  @slots: hash<string, Item>
  def initialize
    @slots = {}
  end
  def [](row: int, col: int) -> Item
    @slots.fetch("#{row}:#{col}")
  end
  def []=(row: int, col: int, value: Item)
    @slots["#{row}:#{col}"] = value
  end
end
class Item
  property value: int
end
# The checker types an index assignment as the result of []=, while
# its value is the assigned value, so assign returns any.
def assign(grid: Grid) -> any
  grid[1, 2] = 4
end
def run -> array<any>
  grid = Grid.new
  assigned = assign(grid)
  grid[1, 2] += 5
  rows = Rows.new
  rows[0, 0] = [1]
  saved = rows[0, 0]
  rows[0, 0].push(2)
  rows[0, 0][0] = 8
  items = Items.new
  item = Item.new
  items[5, 6] = item
  items[5, 6].value = 12
  [assigned, grid[1, 2], saved, rows[0, 0], item.value]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([4, 9, [1], [1], 12]));
    // ||= and &&= test their target, which must be a bool.
    for operator in ["||=", "&&="] {
        let source = format!(
            "class Grid\n  def [](row: int, col: int) -> int\n    0\n  end\n  \
             def []=(row: int, col: int, value: int)\n  end\nend\n\
             grid = Grid.new\ngrid[3, 4] {operator} 7\n"
        );
        let error = common::static_engine().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0104"], "{operator}");
    }
}

#[test]
fn shovel_dispatches_its_own_method_and_preserves_array_behavior() {
    let script = Engine::new()
        .compile(
            r##"
class Bag
  getter items: array<int>
  def initialize
    @items = []
  end
  def <<(value: int) -> Bag
    @items.push(value)
    self
  end
  def push(value: int) -> Bag
    @items.push(value * 10)
    self
  end
end
def run -> array<array<int>>
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
  @value: int
  getter events: array<string>
  def initialize
    @events = []
    @value = 1
  end
  def receiver -> Recorder
    @events.push("receiver")
    self
  end
  def selector -> int
    @events.push("selector")
    0
  end
  def rhs -> int
    @events.push("rhs")
    2
  end
  def [](index: int) -> int
    @events.push("get")
    @value
  end
  def []=(index: int, value: int)
    @events.push("set")
    @value = value
    99
  end
end
# The checker types an index assignment as the result of []=, while
# its value is the assigned value, so plain_write returns any.
def plain_write(r: Recorder) -> any
  r.receiver[r.selector] = r.rhs
end
def compound_write(r: Recorder) -> int
  r.receiver[r.selector] += r.rhs
end
def plain -> array<any>
  r = Recorder.new
  value = plain_write(r)
  [value, r.events]
end
def compound -> array<int | array<string>>
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
  private def +(other: any) -> int
    1
  end
end
class Related
  protected def [](index: int) -> int
    index + 2
  end
  def read(other: Related) -> int
    other[3]
  end
end
class Leaky
  def +(other: int) -> any
    break 3
  end
  def [](index: int) -> any
    next 4
  end
  def []=(index: int, value: int)
    break 5
  end
end
def hidden -> int
  Hidden.new + 1
end
def protected_outside -> int
  Related.new[1]
end
def protected_inside -> int
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
  @name: string
  def initialize(@name: string)
  end
  private def to_s(prefix: string = "name", *, suffix: string = "!") -> string
    "#{prefix}=#{@name}#{suffix}"
  end
end
class Required
  def to_s(value: any) -> string
    "unreachable"
  end
end
class NonString
  def to_s -> array<NonString>
    [self]
  end
end
def run -> array<string>
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
  def +(other: any) -> any
    self + other
  end
  def to_s -> string
    "#{{self}}"
  end
end
def run -> any
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
  def +(value: any) -> C
    stop()
    effect()
    self
  end
  alias_method :"<<", :"+"
  alias_method :"[]", :"+"
  def []=(key: any, value: any)
    stop()
    effect()
  end
  def push(value: any) -> C
    effect()
    self
  end
  def to_s -> string
    stop()
    effect()
    "ignored"
  end
end
def run -> any
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
  @payload: string
  @value: int
  def initialize(@value: int)
    @payload = "x" * 512
  end
  def +(value: int) -> Counter
    Counter.new(@value + value)
  end
  def to_s -> string
    "v=#{@value}"
  end
end
def run -> array<string>
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
    engine.register("bad", |_, _| Ok(Value::bytes("bad return")));
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            r##"
class C
  getter state: int
  def initialize
    @state = 0
  end
  def +(value: int) -> C
    @state += 1
    bad().as(C)
  end
  def []=(key: int, value: int) -> int
    @state = value
    bad().as(int)
  end
end
def add -> any
  c = C.new
  begin
    c += 1
    effect()
  ensure
    capture(c)
  end
end
def write -> any
  c = C.new
  begin
    c[0] = 7
    effect()
  ensure
    capture(c)
  end
end
def read(c: C) -> int
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
