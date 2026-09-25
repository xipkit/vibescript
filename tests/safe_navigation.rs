mod common;

use vibescript::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Signature, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

/// A host function without parameters whose result has type `result`.
fn typed(
    name: &str,
    result: &str,
    function: impl Fn(&mut CallContext) -> vibescript::Result<Value> + Send + Sync + 'static,
) -> HostMethod {
    HostMethod::new(name, move |ctx, _, _| function(ctx))
        .with_signature(Signature {
            params: vec![],
            result: result.into(),
            accepts_block: false,
        })
        .unwrap()
}

#[test]
fn safe_navigation_only_guards_nil_and_its_immediate_access() {
    let script = Engine::new()
        .compile(
            r##"
class Person
  property name: string
  def initialize(@name: string)
  end
  def greet(prefix: string) -> string
    prefix + " " + @name
  end
  def <=>(other: int) -> int
    7
  end
end
def run -> array<any>
  user: Person? = nil
  person: Person? = Person.new("Ada")
  flag: bool? = false
  [user&.name, person&.name, user&.greet("hi"), person&.greet("hi"),
   user&.name&.upcase, person&.name&.upcase, flag&.to_s,
   user&.name == nil, user&.name&.length, person&.<=>(1)]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            null, "Ada", null, "hi Ada", null, "ADA", "false", true, null, 7
        ])
    );
    // The access after an unguarded dot reads the possibly nil result.
    let source = "user: string? = nil\nuser&.upcase.length";
    let error = common::static_engine().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0203"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("length").unwrap()
    );
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
    engine.register_method(
        "effect",
        typed("effect", "int", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::int(1))
        }),
    );
    let script = engine
        .compile(
            r##"
class K
  def one(value: int) -> int
    value
  end
  def keyword(*, flag: int) -> int
    flag
  end
end
def run -> int
  value: array<int>? = nil
  k: K? = nil
  value&.push(effect())
  k&.keyword(flag: effect())
  value&.push(*[effect()])
  k&.keyword(**{flag: effect()})
  k&.one effect()
  value&.map { effect() }
  value&.map! { effect() }
  value&.first&.abs
  value&.push(effect()) == nil
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
def missing -> int
  raise "never"
end
def run -> array<any>
  array: array<int>? = [1]
  before = array
  array&.push(2)
  hash: { items: array<int>?, absent: array<int>? } = {items: [3], absent: nil}
  before_hash = hash
  hash["items"]&.push(4)
  hash["absent"]&.push(missing)
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
    // The grammar accepts a safe read in a selector or a call's receiver.
    for source in [
        "hash = {}; hash[nil&.missing] = 1",
        "hash = {}; hash[nil&.missing] += 1",
        "make(nil&.missing).field = 1",
    ] {
        vibescript::surface::parse::parse(source)
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
    }
}

#[test]
fn skipped_calls_do_not_allocate_arguments_or_retain_pending_paths() {
    let script = Engine::new()
        .compile(
            r##"
class K
  def keyword(*, data: string) -> int
    1
  end
end
def run
  value: array<string>? = nil
  ints: array<int>? = nil
  k: K? = nil
  holder: { absent: array<string>? } = {absent: nil}
  for i in 1..2000
    value&.push("x" * 1000000)
    value&.map! { |item| item + "!" }
    ints&.push(*(1..10000000).to_a)
    k&.keyword(**{data: "x" * 1000000})
    holder["absent"]&.push("x" * 1000000)
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
        "stop()&.push(effect())",
        "stop()&.fetch(effect())",
        "stop()&.map { effect() }",
    ] {
        for exhaust in [false, true] {
            let token = CancellationToken::new();
            let cancelled = token.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let invoked = calls.clone();
            let effects = Arc::new(AtomicUsize::new(0));
            let observed = effects.clone();
            let mut engine = Engine::new();
            engine.register_method(
                "stop",
                typed("stop", "array<int>?", move |ctx| {
                    invoked.fetch_add(1, Ordering::Relaxed);
                    if exhaust {
                        let _ = ctx.charge(u64::MAX);
                    } else {
                        cancelled.cancel();
                        let _ = ctx.checkpoint();
                    }
                    Ok(Value::nil())
                }),
            );
            engine.register_method(
                "effect",
                typed("effect", "int", move |_| {
                    observed.fetch_add(1, Ordering::Relaxed);
                    Ok(Value::int(0))
                }),
            );
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
    for operation in [
        "m&.captures&.push(\"x\")",
        "m&.dup&.captures&.clear",
        "m&.named_captures&.clear",
    ] {
        let source = format!("def run\nm = \"ab\".match(/(?<first>a)(b)/)\n{operation}\nend");
        let script = Engine::new().compile(&source).unwrap();
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{operation}"
        );
    }
    let script = Engine::new()
        .compile(
            r##"
def run -> array<any>
  match = "ab".match(/(?<first>a)(b)/)
  copy = match&.captures
  copy&.push("x")
  ["#{match&.dup}", match&.dup&.to_s, copy, match&.captures]
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
  private def hidden -> int
    7
  end
  protected def guarded -> int
    8
  end
  public
  def probe(other: C?) -> int?
    other&.guarded
  end
  def recur -> int?
    me: C? = self
    me&.recur
  end
end
def run -> array<any>
  n: C? = nil
  [n&.hidden, C.new.probe(nil), C.new.probe(C.new)]
end
def hidden -> int?
  c: C? = C.new
  c&.hidden
end
def guarded -> int?
  c: C? = C.new
  c&.guarded
end
def recursive -> int?
  c: C? = C.new
  c&.recur
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
class Row
  property name: string
  def initialize
    @name = ""
  end
end
def run -> array<any>
  row = Row.new
  row.
    name = "Ada"
  snapshot = row.name
  row.
    name += "!"
  missing: string? = nil
  [row.name, snapshot, missing&.
    upcase]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["Ada!", "Ada", null])
    );
    // A hash field is indexed, not dotted.
    let source = "row = {name: \"x\"}\nrow.\n  name = \"Ada\"";
    let error = common::static_engine().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0415"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find(".\n").unwrap()
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
  @values: array<array<int>?> = [nil, [1]]
  def [](index: int) -> array<int>?
    @values[index]
  end
  def stored -> array<array<int>?>
    @values
  end
end
def missing -> int
  raise "never"
end
def run -> array<any>
  values: array<{ name: string }?> = [nil, {name: "Ada"}]
  first = values.map { it&.fetch("name") }
  second = values.map { _1&.fetch("name") }
  box = Box.new
  box[0]&.push(missing)
  temporary = box[1]&.push(2)
  hash = {items: [3]}
  hash["items"] << 4
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
def identity(value: any) -> any
  value
end
def run -> array<any>
  ordinary = identity :&.to_s
  safe: symbol? = :&&
  [ordinary, safe&.to_s, :& == nil, (false ? nil : :&.to_s),
   {key: :&.to_s}, "#{:&.to_s}", :&.
   to_s]
end
"##,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["&", "&&", false, "&", {"key": "&"}, "&", "&"])
    );
    // Symbols are not numbers, so a range of them is refused.
    for source in [":&..:&", ":&...:&"] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0101", "V0101"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, 0, "{source}");
    }
}
