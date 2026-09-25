//! Dispatch by name, `send` and `public_send`, is removed (ADR-008), so
//! every forwarding form these tests once ran is refused at compile time,
//! and the runtime forwarding they exercised is unreachable from a program
//! that type checks.

mod common;

use vibescript::{CallOptions, Engine, ErrorKind, Value};

#[test]
fn dispatch_by_name_is_refused_on_values_receivers_and_through_forwarding() {
    for (source, count, at) in [
        // Universal helpers on values of every kind.
        ("[nil.send(:nil?), true.public_send(:nil?)]", 2, "send"),
        ("1.send(:seconds)", 1, "send"),
        ("nil&.send(:nil?)", 1, "send"),
        // Blocks and forwarding chains.
        ("[1,2].send(:map) { |x| x + 3 }", 1, "send"),
        ("[1,2].send(:public_send, :map) { |x| x + 3 }", 1, "send"),
        // Visibility.
        (
            "class C\n  private def hidden -> int\n    3\n  end\nend\nC.new.send(:hidden)",
            1,
            "send(",
        ),
        // Names known only at run time.
        (
            "def run(name: string) -> any\n  {}.send(name)\nend",
            1,
            "send",
        ),
        (
            "def run(names: array<symbol>) -> any\n  [1].send(*names, 1)\nend",
            1,
            "send",
        ),
        // Arguments that mutate the receiver, and protected receivers.
        ("a = [1, 2]\na.send(:include?, a.pop)", 1, "send"),
        ("m = \"ab\".match(/(a)(b)/)\nm&.send(:clear)", 1, "send"),
    ] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), vec!["V0405"; count], "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{source}"
        );
    }
}

#[test]
fn receiver_read_snapshots_enforce_exact_budgets_and_release_their_storage() {
    let script = Engine::new()
        .compile("def run(items: array<int>) -> bool\nitems.include?(items.pop.as(int))\nend")
        .unwrap();
    let args = [Value::array((0..1024).map(Value::int).collect())];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.to_string(), "true");
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert_eq!(args[0].as_array().unwrap().len(), 1024);
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.call("run", &args, options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(
        script.call("run", &args, options.clone()).unwrap_err().kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("run", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}
