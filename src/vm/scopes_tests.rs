use super::*;
use crate::{CallOptions, CancellationToken, Engine, Limits, Script, namespace::Namespace};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const COUNTER: &str = r#"
module Counter
  COUNT = 0
  ITEMS = [1]
  DATA = {values: [2]}
  module Nested
    VALUES = [3]
  end
  def self.bump; COUNT += 1; COUNT; end
  def self.current; COUNT; end
  def self.append(n); ITEMS=ITEMS.push(n); ITEMS; end
  def self.pending
    @@extra=[7]
    @@extra[0] += @@extra.push(8).last
    @@extra
  end
  def self.reject; @@extra.insert(); end
  def self.identity; Counter; end
  def self.invoke
    begin
      yield(COUNT)
    ensure
      COUNT += 1
    end
  end
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

/// An engine for these tests' programs, which pass captured modules between
/// scripts as values and reassign module constants. A script cannot name
/// another script's module type, so they compile without static types.
fn untyped() -> Engine {
    let mut engine = Engine::new();
    engine.set_static_types(false);
    engine
}

fn json(value: &Value) -> serde_json::Value {
    let output = crate::stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn distinct_environments_of_one_code_preserve_aliases_and_independent_state() {
    let source = untyped().compile(COUNTER).unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let caller = untyped()
        .compile(
            "def run(a,b,again); [a==b,a==again,a.identity==a,a.bump,b.current,again.bump,b.bump,a.current,b.current]; end",
        )
        .unwrap();
    for _ in 0..2 {
        let result = caller
            .call(
                "run",
                &[a.clone(), b.clone(), a.clone()],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(
            json(&result.value),
            serde_json::json!([false, true, true, 1, 0, 2, 1, 2, 1])
        );
    }
    let caller = untyped()
        .compile("def run(a:,b:); [a.bump,b.current,a.identity==a,b.identity==b]; end")
        .unwrap();
    let result = caller
        .call_with_keywords(
            "run",
            &[],
            &[("b".into(), b), ("a".into(), a)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1, 0, true, true]));
}

#[test]
fn retained_state_is_copied_without_repeating_completed_initializers() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let observed = initialized.clone();
    let mut engine = untyped();
    engine.register("initialize", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(0))
    });
    let source = engine
        .compile(&COUNTER.replace("COUNT = 0", "COUNT = initialize()"))
        .unwrap();
    let incoming = captured(&source, "Counter");
    let caller = untyped()
        .compile(
            r#"
def warm(m); m.link=m; m.bump; m.append(2); m; end
def change(m); [m.link==m,m.current,m.bump,m.append(3)]; end
def discard(m); m.bump; nil; end
"#,
        )
        .unwrap();
    let warmed = caller
        .call("warm", &[incoming], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
    drop(source);
    drop(engine);
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
                assert_eq!(
                    json(&result.value),
                    serde_json::json!([true, 1, 2, [1, 2, 3]])
                );
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
    let source = untyped().compile(COUNTER).unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let both = Value::array(vec![
        a.clone(),
        Value::hash(vec![(b"second".to_vec(), b.clone())]),
    ]);
    let mut engine = untyped();
    engine.register("first", move |_, _| Ok(a.clone()));
    engine.register("second", move |_, _| Ok(b.clone()));
    engine.register("both", move |_, _| Ok(both.clone()));
    let caller = engine
        .compile(
            r#"
def sequential
  a=first()
  a.bump
  b=second()
  [a.current,b.current,a.identity==a,b.identity==b]
end
def graph
  values=both()
  a=values[0]
  b=values[1][:second]
  [a.bump,b.current,a==b]
end
"#,
        )
        .unwrap();
    assert_eq!(
        json(
            &caller
                .call("sequential", &[], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([1, 0, true, true])
    );
    assert_eq!(
        json(
            &caller
                .call("graph", &[], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([1, 0, false])
    );
}

#[test]
fn captured_fields_keep_pending_addresses_snapshots_and_recoverable_writes() {
    let source = untyped().compile(COUNTER).unwrap();
    let caller = untyped()
        .compile(
            r#"
def run(m)
  old=m::ITEMS
  m::ITEMS[0] = 2
  m::DATA[:values][0] += 3
  m::Nested::VALUES[0] = 4
  m.extra=m.pending
  begin
    m.reject
  rescue
    nil
  end
  begin
    m::ITEMS[0] /= 0
  rescue
    nil
  end
  [old,m::ITEMS,m::DATA[:values],m::Nested::VALUES,m.extra,m]
end
def read(m)
  [m::ITEMS,m::DATA[:values],m::Nested::VALUES,m.extra]
end
"#,
        )
        .unwrap();
    let result = caller
        .call(
            "run",
            &[captured(&source, "Counter")],
            CallOptions::default(),
        )
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        json(&Value::array(values[..5].to_vec())),
        serde_json::json!([[1], [2], [5], [4], [15, 8]])
    );
    let retained = values[5].clone();
    drop(result);
    let read = caller
        .call("read", &[retained], CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&read.value),
        serde_json::json!([[2], [5], [4], [15, 8]])
    );
}

#[test]
fn protected_dispatch_and_nominal_types_distinguish_captured_classes() {
    let source = untyped()
        .compile(
            r#"
class C
  protected def hidden; 7; end
  protected def value=(n); @value=n; end
  protected def +(n); 11; end
  def peer(other); other.hidden; end
  def set_peer(other); other.value=3; end
  def add_peer(other); other+1; end
  def typed_peer(other: C); 13; end
end
"#,
        )
        .unwrap();
    let a = captured(&source, "C");
    let b = captured(&source, "C");
    for (method, expected, kind) in [
        ("peer", 7, ErrorKind::Name),
        ("set_peer", 3, ErrorKind::Name),
        ("add_peer", 11, ErrorKind::Name),
        ("typed_peer", 13, ErrorKind::Type),
    ] {
        let caller = untyped()
            .compile(&format!(
                "def run(a,b); x=a.new; y=b.new; x.{method}(y); end"
            ))
            .unwrap();
        let error = caller
            .call("run", &[a.clone(), b.clone()], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, kind, "{method}: {error}");
        let result = caller
            .call("run", &[a.clone(), a.clone()], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{method}");
    }
}

#[test]
fn invalid_later_arguments_stop_before_captured_initializers() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let observed = initialized.clone();
    let mut engine = untyped();
    engine.register("initialize", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(0))
    });
    let source = engine
        .compile(&COUNTER.replace("COUNT = 0", "COUNT = initialize()"))
        .unwrap();
    let incoming = captured(&source, "Counter");
    let caller = untyped().compile("def run(a,b); nil; end").unwrap();
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
    let source = untyped().compile(COUNTER).unwrap();
    let caller = untyped()
        .compile("def run(m); n=10; a=m.invoke{|x| x+n}; b=m.invoke{break 7}; [a,b,m.current]; end")
        .unwrap();
    let result = caller
        .call(
            "run",
            &[captured(&source, "Counter")],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([10, 7, 2]));
}

#[test]
fn rejected_and_unreturned_scopes_are_only_initialized_when_later_accepted() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let observed = initialized.clone();
    let mut producer = untyped();
    producer.register("initialize", move |_, _| {
        Ok(Value::int(
            observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
        ))
    });
    let source = producer
        .compile(&COUNTER.replace("COUNT = 0", "COUNT = initialize()"))
        .unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let never_returned = captured(&source, "Counter");
    let deep = (0..crate::budget::MAX_VALUE_DEPTH + 1)
        .fold(Value::nil(), |value, _| Value::array(vec![value]));
    let rejected = Value::array(vec![a.clone(), deep]);
    let unreturned = b.clone();
    let count = initialized.clone();
    let mut consumer = untyped();
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
def run
  begin
    rejected()
  rescue LimitError
    nil
  end
  retain()
  [count(),second().current,count(),first().current,count()]
end
"#,
        )
        .unwrap();
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([0, 1, 1, 2, 2]));
    assert_eq!(initialized.load(Ordering::SeqCst), 2);
}

#[test]
fn same_code_scopes_initialize_in_argument_and_graph_discovery_order() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let observed = initialized.clone();
    let mut producer = untyped();
    producer.register("initialize", move |_, _| {
        Ok(Value::int(
            observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
        ))
    });
    let source = producer
        .compile(&COUNTER.replace("COUNT = 0", "COUNT = initialize()"))
        .unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let graph = Value::array(vec![
        b.clone(),
        Value::hash(vec![(b"a".to_vec(), a.clone())]),
        b.clone(),
    ]);
    let mut consumer = untyped();
    consumer.register("graph", move |_, _| Ok(graph.clone()));
    let caller = consumer
        .compile(
            r#"
def positional(a,b,c); [a.current,b.current,c.current]; end
def keywords(a:,b:); [a.current,b.current]; end
def run; v=graph(); [v[0].current,v[1][:a].current,v[2].current]; end
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
    assert_eq!(json(&result.value), serde_json::json!([1, 2, 1]));
    initialized.store(0, Ordering::SeqCst);
    let result = caller
        .call_with_keywords(
            "keywords",
            &[],
            &[("b".into(), b), ("a".into(), a)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([2, 1]));
    initialized.store(0, Ordering::SeqCst);
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!([1, 2, 1]));
}

#[test]
fn captured_initializers_obey_receiving_budgets_cancellation_and_cleanup() {
    let active = Arc::new(AtomicBool::new(false));
    let flag = active.clone();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let mut producer = untyped();
    producer.register("initialize", move |_, _| {
        if flag.load(Ordering::SeqCst) {
            token.cancel();
        }
        Ok(Value::nil())
    });
    let source = producer
        .compile("module M\n initialize()\n VALUES=(1..1024).map{|n| n}\nend")
        .unwrap();
    let value = captured(&source, "M");
    let caller = untyped().compile("def run(m); m.VALUES.size; end").unwrap();
    let baseline = caller
        .call("run", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(baseline.value.as_int(), Some(1024));
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
    assert_eq!(exact.value.as_int(), Some(1024));
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
    let mut producer = untyped();
    producer.register("initialize", move |_, _| {
        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(Error::new(ErrorKind::Runtime, "scope failed"));
        }
        Ok(Value::int(7))
    });
    let source = producer
        .compile(&COUNTER.replace("COUNT = 0", "COUNT = initialize()"))
        .unwrap();
    let a = captured(&source, "Counter");
    let b = captured(&source, "Counter");
    let mut consumer = untyped();
    consumer.register("first", move |_, _| Ok(a.clone()));
    consumer.register("second", move |_, _| Ok(b.clone()));
    let caller = consumer
        .compile(
            r#"
def run
  begin
    first()
  rescue => error
    failed=error.message
  end
  other=second().current
  begin
    again=first().current
  rescue => error
    again=error.message
  end
  [failed,other,again]
end
def retry_scope; first().current; end
"#,
        )
        .unwrap();
    let result = caller.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!(["scope failed", 7, 7])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let result = caller
        .call("retry_scope", &[], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}
