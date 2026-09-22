use vibescript::{CallOptions, CheckReport, Engine, Script};

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
        .any(|d| d.message.starts_with("Return value:"))
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
