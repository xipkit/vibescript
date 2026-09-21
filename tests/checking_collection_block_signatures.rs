use vibescript::{
    CallOptions, Capability, CheckReport, Engine, ErrorKind, HostMethod, Script, Signature,
    SignatureParam, Value,
};

fn compile(source: &str) -> Script {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn check(script: &Script, source: &str) -> CheckReport {
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("Return value: expected int"))
}

fn rejects_member(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("does not accept receiver"))
}

#[test]
fn always_rejected_block_calls_end_the_path_before_the_return() {
    for (source, message) in [
        (
            "def run -> int; [1].to_s { |x| x }; 'bad'; end",
            "array.to_s does not take a block",
        ),
        (
            "def run -> int; [1].send(:string) { |x| x }; 'bad'; end",
            "array.string does not take a block",
        ),
        (
            "def run -> int; [1,2].reverse { |x| x }; 'bad'; end",
            "array.reverse does not accept a block",
        ),
        (
            "def run -> int; [1,nil].compact { |x| x }; 'bad'; end",
            "array.compact does not accept a block",
        ),
        (
            "def run -> int; [1,2,3].chunk(2) { |x| x }; 'bad'; end",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "def run -> int; [1].clear { |x| x }; 'bad'; end",
            "array.clear does not accept a block",
        ),
        (
            "def run -> int; {a: 1}.clear { |x| x }; 'bad'; end",
            "hash.clear does not accept a block",
        ),
        (
            "def run -> int; a = [1]; a.clear { |x| x }; 'bad'; end",
            "array.clear does not accept a block",
        ),
        (
            "def run -> int; h = {a: 1}; h.clear { |x| x }; 'bad'; end",
            "hash.clear does not accept a block",
        ),
        (
            "def run -> int; [1].send(:reverse) { |x| x }; 'bad'; end",
            "array.reverse does not accept a block",
        ),
        (
            "def run -> int; a = [1]; a.send(:clear) { |x| x }; 'bad'; end",
            "array.clear does not accept a block",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(rejects_member(&report), "{source}: {report:?}");
        // The call never returns, so the bad tail is unreachable.
        assert!(!returns_bad_type(&report), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}: {error}");
        assert_eq!(error.message, message, "{source}");
    }
}

#[test]
fn rejected_block_calls_keep_rescues_reachable_without_mutating() {
    // The rescue arm is the only way to the bad return.
    for source in [
        "def run -> int; begin; [1,2].reverse { |x| x }; 0; rescue; 'bad'; end; end",
        "def run -> int; begin; [1,2,3].chunk(2) { |x| x }; 0; rescue; 'bad'; end; end",
        "def run -> int; begin; [1].send(:compact) { |x| x }; 0; rescue; 'bad'; end; end",
        "def run -> int; a = [1]; begin; a.clear { |x| x }; 0; rescue; 'bad'; end; end",
        "def run -> int; h = {a: 1}; begin; h.clear { |x| x }; 0; rescue; 'bad'; end; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(returns_bad_type(&report), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
        assert!(
            error.message.contains("return value for run expected int"),
            "{source}: {error}"
        );
    }
    // The receiver is not cleared on the rescue path, so the emptiness guard
    // proves the bad arm unreachable.
    for (source, expected) in [
        (
            "def run -> int; a = [1, 2]; begin; a.clear { |x| x }; rescue; if a.empty?; 'bad'; else; a.length; end; end; end",
            2,
        ),
        (
            "def run -> int; h = {a: 1}; begin; h.clear { |x| x }; rescue; if h.empty?; 'bad'; else; h.size; end; end; end",
            1,
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(rejects_member(&report), "{source}: {report:?}");
        assert!(!returns_bad_type(&report), "{source}: {report:?}");
        assert_eq!(report.diagnostics.len(), 1, "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}

#[test]
fn blockless_and_iterating_forms_stay_clean() {
    for (source, expected) in [
        ("def run -> int; [1,2,3].reverse.first; end", 3),
        ("def run -> int; [1,nil].compact.length; end", 1),
        ("def run -> int; [1,2,3].chunk(2).length; end", 2),
        ("def run -> int; a = [1]; a.clear; a.length; end", 0),
        ("def run -> int; h = {a: 1}; h.clear; h.size; end", 0),
        (
            "def run -> int; a = [3, 1]; a.delete_if { |x| x > 2 }; a.length; end",
            1,
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}

fn host_options() -> CallOptions {
    let member = |name: &'static str| {
        HostMethod::new_with_block(name, |call, args, _| call.call_block(args))
            .with_signature(Signature {
                params: vec![SignatureParam {
                    name: "n".into(),
                    ty: "int".into(),
                    optional: false,
                }],
                result: "int".into(),
                accepts_block: true,
            })
            .unwrap()
            .value()
    };
    CallOptions {
        capabilities: vec![Capability::from_value(
            "host",
            Value::object(vec![
                (b"clear".to_vec(), member("host.clear")),
                (b"reverse".to_vec(), member("host.reverse")),
            ]),
        )],
        ..CallOptions::default()
    }
}

#[test]
fn callable_object_members_with_collection_names_are_not_rejected() {
    for body in [
        "host.clear(3) { |n| n + 1 }",
        "host.reverse(3) { |n| n + 1 }",
        "local = host; local.clear(3) { |n| n + 1 }",
    ] {
        let source = format!("def run -> int\n{body}\nend");
        let script = compile(&source);
        let report = script
            .check_function("run", &host_options())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert!(!rejects_member(&report), "{source}: {report:?}");
        assert!(report.is_clean(), "{source}: {report:?}");
        let outcome = script
            .call("run", &[], host_options())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(outcome.value.as_int(), Some(4), "{source}");
    }
}
