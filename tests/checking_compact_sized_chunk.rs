mod common;

use vibescript::{CallOptions, ErrorKind, Value};

fn check(source: &str, bad_return: bool) -> vibescript::Script {
    let script = common::gradual_engine()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    assert_eq!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.starts_with("Return value:")),
        bad_return,
        "{source}: {report:?}"
    );
    script
}

#[test]
fn fixed_shapes_preserve_element_order_and_lengths() {
    for (expression, expected) in [
        ("[nil,1,nil,2].compact.last", 2),
        ("[nil,nil].compact.length", 0),
        ("[false,nil].compact.length", 1),
        ("[1,2,3].chunk(2).length", 2),
        ("[1,2,3].chunk(2).last.first", 3),
        ("[1,2,3].chunk(99).first.length", 3),
        ("[].chunk(1).length", 0),
        ("[1,2,3].send(:chunk,2).length", 2),
        ("[1,2,3].chunk(2, ignored: 1).length", 2),
    ] {
        let source = format!("def run -> int; {expression}; end");
        let script = check(&source, false);
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(expected),
            "{source}"
        );
    }
}

#[test]
fn compact_keeps_the_empty_path_for_nullable_and_unknown_items() {
    for ty in ["any", "int | nil"] {
        let source =
            format!("def run(x: {ty}) -> int; if [x].compact.empty?; 'bad'; else; 0; end; end");
        let script = check(&source, true);
        assert_eq!(
            script
                .call("run", &[Value::nil()], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
    }
    let source = "def run(xs: array<int | nil>) -> array<int>; xs.compact; end";
    let script = check(source, false);
    let value = script
        .call(
            "run",
            &[Value::array(vec![Value::nil(), Value::int(2)])],
            CallOptions::default(),
        )
        .unwrap()
        .value;
    assert_eq!(value.as_array().unwrap().len(), 1);
}

#[test]
fn bad_sizes_and_signatures_stop_the_normal_path() {
    for expression in [
        "[1].chunk()",
        "[1].chunk(1,2)",
        "[1].chunk(0)",
        "[].chunk(-1)",
        "[1].chunk(1.5)",
        "[1].chunk(nil)",
        "[1].chunk('2')",
        "[1].compact(2)",
        "[1].compact(k: 1)",
    ] {
        let source = format!("def run -> int; {expression}; 'bad'; end");
        let script = check(&source, false);
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("does not accept receiver")),
            "{source}: {report:?}"
        );
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn uncertain_sizes_preserve_success_and_rescue_paths() {
    for source in [
        "def run(n: int | string) -> int; [1].chunk(n); 'bad'; end",
        "def run(n: int | string) -> int; begin; [1].chunk(n); 0; rescue; 'bad'; end; end",
        "def run(n: int) -> int; begin; [1].chunk(n); 0; rescue; 'bad'; end; end",
    ] {
        check(source, true);
    }
    let source = "def run(xs: array<int>, n: int) -> array<array<int>>; xs.chunk(n); end";
    let script = check(source, false);
    let value = script
        .call(
            "run",
            &[
                Value::array(vec![Value::int(1), Value::int(2), Value::int(3)]),
                Value::int(2),
            ],
            CallOptions::default(),
        )
        .unwrap()
        .value;
    assert_eq!(value.as_array().unwrap().len(), 2);
    let source = "def run(xs: array<string>) -> array<array<int>>; xs.chunk(2); end";
    check(source, true);
}
