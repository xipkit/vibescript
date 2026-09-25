mod common;

use vibescript::{CallOptions, CheckReport, Engine, ErrorClass, Script, Value};

fn compile(source: &str) -> Script {
    common::gradual_engine()
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
        .any(|d| d.message.starts_with("Return value:"))
}

fn text(script: &Script, args: &[Value]) -> String {
    let value = script
        .call("run", args, CallOptions::default())
        .unwrap()
        .value;
    String::from_utf8(value.as_bytes().unwrap().to_vec()).unwrap()
}

#[test]
fn templates_render_nested_paths_scalars_and_enum_members() {
    for (source, args, expected) in [
        (
            "def run -> string; \"Player {{user.name}} scored {{user.score}}\".template({ user: { name: \"Alex\", score: 42 } }); end",
            vec![],
            "Player Alex scored 42",
        ),
        (
            "def run -> string; \"Hello {{missing}}\".template({ name: \"Alex\" }); end",
            vec![],
            "Hello {{missing}}",
        ),
        (
            "enum Status\n  Draft\nend\ndef run -> string; draft = Status::Draft; \"status={{value}}\".template({ value: draft }); end",
            vec![],
            "status=draft",
        ),
        (
            "def run(id) -> string; \"Order {{id}}\".template({ id: id }); end",
            vec![Value::int(7)],
            "Order 7",
        ),
        (
            "def run(id: string) -> string; \"{{a}}/{{ b }}\".template({ a: id, b: nil }, strict: true); end",
            vec![Value::bytes("x")],
            "x/",
        ),
        (
            "def run -> string; \"plain\".template({}, strict: true); end",
            vec![],
            "plain",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(text(&script, &args), expected, "{source}");
    }
}

#[test]
fn template_contradictions_are_diagnosed_and_fail_at_runtime() {
    for expression in [
        "\"{{x}}\".template(1)",
        "\"{{x}}\".template(nil)",
        "\"{{x}}\".template({}, {})",
        "\"{{x}}\".template({ x: [1] })",
        "\"{{x.y}}\".template({ x: { y: { z: 1 } } })",
        "\"{{list}} {{x}}\".template({ list: [1] }, strict: true)",
        "\"{{x}}\".template({ x: 1 }, strict: 1)",
        "\"{{x}}\".template({ x: 1 }, other: true)",
        "\"a\".include?(1)",
        "\"a\".include?(nil)",
        "\"a\".include?(\"a\", \"b\")",
        "\"a\".concat(:b)",
        "\"a\".concat(\"b\", 1)",
    ] {
        let source = format!("def run; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("does not accept")),
            "{source}: {report:?}"
        );
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

/// `strict: true` documents an error for a missing placeholder, so a known
/// miss is an ordinary runtime failure: it has no string result, rescue handles
/// it, and a possible miss keeps both outcomes.
#[test]
fn strict_template_misses_are_runtime_errors() {
    for (source, args, expected) in [
        (
            "def run -> int; \"{{x}}\".template({}, strict: true); end",
            vec![],
            None,
        ),
        (
            "def run -> int; \"{{x.y}} {{list}}\".template({ x: {}, list: [1] }, strict: true); end",
            vec![],
            None,
        ),
        (
            "def run -> string; \"{{x}}\".template({}, strict: true) rescue \"none\"; end",
            vec![],
            Some("none"),
        ),
        (
            "def run(flag: bool) -> string; context = flag ? { x: 1 } : {}; \"{{x}}\".template(context, strict: true); end",
            vec![Value::boolean(true)],
            Some("1"),
        ),
        (
            "def run(flag: bool) -> string; context = flag ? { x: 1 } : {}; \"{{x}}\".template(context, strict: true); end",
            vec![Value::boolean(false)],
            None,
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        match expected {
            Some(expected) => assert_eq!(text(&script, &args), expected, "{source}"),
            None => {
                let error = script
                    .call("run", &args, CallOptions::default())
                    .unwrap_err();
                assert_eq!(error.class(), Some(ErrorClass::Runtime), "{source}");
                assert!(
                    error
                        .message
                        .starts_with("string.template missing placeholder x"),
                    "{source}: {error}"
                );
            }
        }
    }
    // The failure path reaches the rescue clause and its string result.
    let source =
        "def run -> int; begin; \"{{x}}\".template({}, strict: true); 0; rescue; 'bad'; end; end";
    let script = compile(source);
    assert!(returns_bad_type(&check(&script, source)), "{source}");
}

#[test]
fn gradual_template_inputs_keep_their_failure_paths() {
    for source in [
        "def run(context) -> int; begin; \"{{x}}\".template(context); 0; rescue; 'bad'; end; end",
        "def run(text: string) -> int; begin; text.template({ x: [1] }); 0; rescue; 'bad'; end; end",
        "def run(strict: bool) -> int; begin; \"{{x}}\".template({}, strict: strict); 0; rescue; 'bad'; end; end",
        "def run(h: hash) -> int; begin; \"{{x}}\".template(h); 0; rescue; 'bad'; end; end",
    ] {
        let script = compile(source);
        assert!(returns_bad_type(&check(&script, source)), "{source}");
    }
    for source in [
        "def run -> int; begin; \"{{x}} {{y}}\".template({ x: 1 }); 0; rescue; 'bad'; end; end",
        "def run(x: int) -> int; begin; \"{{x}}\".template({ x: x }, strict: true); 0; rescue; 'bad'; end; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    }
    let source = "def run(context) -> string; \"{{x}}\".template(context); end";
    let script = compile(source);
    let error = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "string.template context must be hash");
}

#[test]
fn include_and_concat_follow_the_string_contracts() {
    for (source, args, expected) in [
        (
            "def run(id: string) -> bool; id.include?(\"-\"); end",
            vec![Value::bytes("a-b")],
            "true",
        ),
        (
            "def run(id: string) -> bool; id.include?(:b); end",
            vec![Value::bytes("a-b")],
            "true",
        ),
        (
            "def run(id: string) -> bool; id.include?(\"\"); end",
            vec![Value::bytes("")],
            "true",
        ),
        (
            "def run(id: string) -> string; id.concat(\"llo\", \"!\"); end",
            vec![Value::bytes("he")],
            "hello!",
        ),
        (
            "def run(id: string) -> string; id.concat; end",
            vec![Value::bytes("he")],
            "he",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let value = script
            .call("run", &args, CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), expected, "{source}");
    }
    // A literal empty needle is always found, so the false branch is unreachable.
    let source = "def run(id: string) -> int; id.include?(\"\") ? 1 : 'bad'; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
}

#[test]
fn interpolating_gradual_values_keeps_unknown_conversion_effects() {
    let source = "
module Tally
  @@count = 0
  def self.count
    @@count
  end
  def self.bump
    @@count += 1
  end
end

class Loud
  def to_s
    Tally.bump
    'loud'
  end
end

class Broken
  def to_s
    raise 'no text'
  end
end

def label(value) -> string
  \"Order #{value} is on its way.\"
end

def counted(value) -> int
  \"#{value}\"
  Tally.count == 0 ? 0 : 'changed'
end

def rescued(value) -> int
  begin
    \"#{value}\"
    0
  rescue
    'failed'
  end
end

def run
  [label(7), label(Loud.new)]
end

def loud_count
  counted(Loud.new)
end

def broken_text
  rescued(Broken.new)
end
";
    let script = compile(source);
    let label = script
        .check_function("label", &CallOptions::default())
        .unwrap();
    assert!(label.is_clean(), "{label:?}");
    for name in ["counted", "rescued"] {
        let report = script
            .check_function(name, &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{name}: {report:?}");
        assert!(returns_bad_type(&report), "{name}: {report:?}");
    }
    let loud = script
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(
        loud.to_string(),
        "[Order 7 is on its way., Order loud is on its way.]"
    );
    // A source `to_s` really changes shared state and really raises.
    for witness in ["loud_count", "broken_text"] {
        let error = script
            .call(witness, &[], CallOptions::default())
            .unwrap_err();
        assert!(error.message.ends_with("got string"), "{witness}: {error}");
    }
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
}

#[test]
fn format_and_output_convert_gradual_values_without_incomplete_paths() {
    for source in [
        "def run(value) -> string; format(\"%s!\", value); end",
        "def run(value) -> string; \"#{value % 3}#{value}\"; end",
        "def run(left, right) -> string; \"#{left ** 3 + right ** 3}\"; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    }
}
