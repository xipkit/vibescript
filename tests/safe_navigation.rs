use vibescript::{CallOptions, Engine, ErrorKind, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn safe_navigation_only_guards_nil_and_its_immediate_access() {
    let script = Engine::new()
        .compile(
            r##"
class Person
  property name
  def initialize(@name)
  end
  def greet(prefix)
    prefix + " " + @name
  end
  def <=>(other)
    7
  end
end
def run
  user = nil
  person = Person.new("Ada")
  [user&.name, person&.name, user&.greet("hi"), person&.greet("hi"),
   user&.name&.upcase, person&.name&.upcase, false&.itself,
   user&.missing.nil?, user&.missing&.nil?, person&.<=>(1)]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            null, "Ada", null, "hi Ada", null, "ADA", false, true, null, 7
        ])
    );
    let script = Engine::new().compile("nil&.missing.name").unwrap();
    assert!(script.run(CallOptions::default()).is_err());
}

#[test]
fn nil_receivers_skip_arguments_splats_keywords_and_blocks() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(1))
    });
    let script = engine
        .compile(
            r##"
def run
  value = nil
  value&.missing(effect())
  value&.missing(flag: effect())
  value&.missing(*[effect()])
  value&.missing(**{flag: effect()})
  value&.missing effect()
  value&.map { effect() }
  value&.map! { effect() }
  value&.field&.clear
  value&.missing(effect()).nil?
  effect()
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn safe_mutators_preserve_named_paths_and_collection_snapshots() {
    let script = Engine::new()
        .compile(
            r##"
def run
  array = [1]
  before = array
  array&.push(2)
  hash = {items: [3], absent: nil}
  before_hash = hash
  hash&.items&.push(4)
  hash&.absent&.push(missing())
  [array, before, hash, before_hash]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            [1, 2],
            [1],
            {"items": [3, 4], "absent": null},
            {"items": [3], "absent": null}
        ])
    );
}

#[test]
fn safe_assignment_targets_are_rejected_but_selectors_can_use_safe_reads() {
    for target in [
        "x&.name",
        "x&.profile.name",
        "x&.items[0]",
        "x&.fetch().name",
        "x.profile&.name",
        "x&.items.each { |item| item }[0]",
    ] {
        for operator in ["=", "+=", "-=", "*=", "/=", "%=", "**=", "||=", "&&="] {
            let source = format!("x = nil\n{target} {operator} 1");
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}");
            assert!(
                error.message.contains("safe navigation"),
                "{source}: {error}"
            );
        }
    }
    for targets in ["x&.name, y", "y, x&.items[0]", "(y, x&.name)"] {
        let source = format!("x = nil\n{targets} = 1, 2");
        assert_eq!(
            Engine::new().compile(&source).err().unwrap().kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
    for source in [
        "hash = {}; hash[nil&.missing] = 1",
        "hash = {}; hash[nil&.missing] += 1",
        "make(nil&.missing).field = 1",
    ] {
        Engine::new().compile(source).unwrap();
    }
}

#[test]
fn skipped_calls_do_not_allocate_arguments_or_retain_pending_paths() {
    let script = Engine::new()
        .compile(
            r##"
def run
  hash = {absent: nil}
  for i in 1..2000
    value = nil
    value&.push("x" * 1000000)
    value&.map! { |item| item + 1 }
    value&.field&.push(*(1..10000000).to_a)
    value&.missing(**{data: "x" * 1000000})
    hash&.absent&.push("x" * 1000000)
  end
  nil
end
"##,
        )
        .unwrap();
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(16 * 1024);
    let result = script.call("run", &[], options).unwrap();
    assert_eq!(json(&result.value), serde_json::Value::Null);
    assert_eq!(result.stats.retained_memory_bytes, 0, "{:?}", result.stats);
}

#[test]
fn a_nil_receiver_does_not_swallow_cancellation_or_ignored_exhaustion() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vibescript::CancellationToken;
    for operation in [
        "stop()&.missing(effect())",
        "stop()&.push(effect())",
        "stop()&.field&.map! { effect() }",
    ] {
        for exhaust in [false, true] {
            let token = CancellationToken::new();
            let cancelled = token.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let invoked = calls.clone();
            let effects = Arc::new(AtomicUsize::new(0));
            let observed = effects.clone();
            let mut engine = Engine::new();
            engine.register("stop", move |ctx, _| {
                invoked.fetch_add(1, Ordering::Relaxed);
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
            let source = format!("def run\n{operation}\neffect()\nend");
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
            assert_eq!(calls.load(Ordering::Relaxed), 1, "{operation}");
            assert_eq!(effects.load(Ordering::Relaxed), 0, "{operation}");
        }
    }
}

#[test]
fn safe_paths_preserve_match_data_protection_and_rendering() {
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
    for operation in [
        "m&.captures.push(\"x\")",
        "m&.dup.captures.clear",
        "m&.dup.clear",
        "m&.named_captures.clear",
        "m&.captures.map! { |value| effect(); value }",
        "m.dup&.captures.map! { |value| effect(); value }",
    ] {
        let source = format!("def run\nm = \"ab\".match(/(?<first>a)(b)/)\n{operation}\nend");
        let script = engine.compile(&source).unwrap();
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{operation}"
        );
        assert_eq!(effects.load(Ordering::Relaxed), 0, "{operation}");
    }
    let script = engine
        .compile(
            r##"
def run
  match = "ab".match(/(?<first>a)(b)/)
  copy = match&.captures
  copy.push("x")
  ["#{match&.dup}", match&.dup&.to_s, copy, match.captures]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["ab", "ab", ["a", "b", "x"], ["a", "b"]])
    );
}

#[test]
fn safe_calls_preserve_visibility_and_vm_recursion_limits() {
    let script = Engine::new()
        .compile(
            r##"
class C
  private def hidden
    7
  end
  protected def guarded
    8
  end
  public
  def probe(other)
    other&.guarded
  end
  def recur
    self&.recur
  end
end
def run
  [nil&.hidden, C.new.probe(nil), C.new.probe(C.new)]
end
def hidden
  C.new&.hidden
end
def guarded
  C.new&.guarded
end
def recursive
  C.new&.recur
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([null, null, 8]));
    for function in ["hidden", "guarded"] {
        assert_eq!(
            script
                .call(function, &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name
        );
    }
    let mut options = CallOptions::default();
    options.limits.recursion = 16;
    assert_eq!(
        script.call("recursive", &[], options).unwrap_err().kind,
        ErrorKind::Recursion
    );
}

#[test]
fn wrapped_members_preserve_assignment_and_name_boundaries() {
    let script = Engine::new()
        .compile(
            r##"
def run
  row = {}
  row.
    name = "Ada"
  snapshot = row
  row.
    name += "!"
  [row.name, snapshot.name, nil&.
    missing]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["Ada!", "Ada", null])
    );
    for separator in [".", "&.", "::"] {
        for name in ["@field", "@@field"] {
            let source = format!("nil{separator}{name}");
            assert_eq!(
                Engine::new().compile(&source).err().unwrap().kind,
                ErrorKind::Syntax,
                "{source}"
            );
        }
    }
}

#[test]
fn safe_reads_bind_implicit_block_parameters_and_index_results() {
    let script = Engine::new()
        .compile(
            r##"
class Box
  def initialize
    @values = [nil, [1]]
  end
  def [](index)
    @values[index]
  end
  def stored
    @values
  end
end
def run
  values = [nil, {name: "Ada"}]
  first = values.map { it&.name }
  second = values.map { _1&.name }
  box = Box.new
  box[0]&.push(missing())
  temporary = box[1]&.push(2)
  hash = {items: [3]}
  hash&.items << 4
  [first, second, temporary, box.stored, hash]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[null, "Ada"], [null, "Ada"], [1, 2], [null, [1]], {"items": [3, 4]}])
    );
}

#[test]
fn operator_symbols_preserve_member_and_safe_call_boundaries() {
    let script = Engine::new()
        .compile(
            r##"
def identity(value)
  value
end
def run
  ordinary = identity :&.to_s
  safe = :&&&.to_s
  [ordinary, safe, :&.nil?, :& &.nil?, (false ? nil : :&.to_s),
   {key: :&.to_s}, "#{:&.to_s}", %W(#{:&.to_s}), :&.
   to_s]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            "&", "&&", false, false, "&", {"key": "&"}, "&", ["&"], "&"
        ])
    );
    for source in [":&..:&", ":&...:&"] {
        let script = Engine::new().compile(source).unwrap();
        assert!(script.run(CallOptions::default()).is_err());
    }
}
