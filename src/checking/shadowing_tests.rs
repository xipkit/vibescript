use crate::{CallOptions, Engine};

/// Checks the whole file, then runs `run`, or the top-level statements without one.
fn witness(source: &str) -> (Vec<String>, Result<String, String>) {
    let script = Engine::new().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    let result = if source.contains("def run") {
        script.call("run", &[], CallOptions::default())
    } else {
        script.run(CallOptions::default())
    };
    let result = result
        .map(|outcome| outcome.value.to_string())
        .map_err(|error| error.message);
    (diagnostics, result)
}

fn clean(source: &str, value: &str) {
    let (diagnostics, result) = witness(source);
    assert!(diagnostics.is_empty(), "{source}: {diagnostics:?}");
    assert_eq!(result, Ok(value.into()), "{source}");
}

fn rejected(source: &str, diagnostic: &str, error: &str) {
    let (diagnostics, result) = witness(source);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains(diagnostic)),
        "{source}: {diagnostics:?}"
    );
    let message = result.expect_err(source);
    assert!(message.contains(error), "{source}: {message}");
}

#[test]
fn assignments_call_the_function_their_binding_shadows() {
    let helper = "def helper\n  1\nend\ndef helper_arg(n)\n  n + 10\nend\n";
    for (body, value) in [
        ("helper = helper()\n  helper", "1"),
        ("helper = [helper, helper()]\n  helper[1]", "1"),
        ("helper_arg = helper_arg 1\n  helper_arg", "11"),
        ("helper = 1\n  helper += helper()\n  helper", "2"),
        (
            "helper_arg = 1\n  helper_arg += helper_arg 2\n  helper_arg",
            "13",
        ),
        ("helper ||= helper()\n  helper", "1"),
        ("helper_arg ||= helper_arg 1\n  helper_arg", "11"),
        ("helper = 5\n  helper &&= helper()\n  helper", "1"),
        ("helper = (helper() rescue 0)\n  helper", "1"),
    ] {
        clean(&format!("{helper}def run -> int\n  {body}\nend"), value);
    }
    // The plain read in the same value still sees the binding, which is nil.
    rejected(
        &format!("{helper}def run -> int\n  helper = [helper, helper()]\n  helper[0]\nend"),
        "Return value",
        "got nil",
    );
    rejected(
        &format!("{helper}def run -> string\n  helper = helper()\n  helper\nend"),
        "Return value",
        "expected string, got int",
    );
}

#[test]
fn only_the_assigned_value_skips_the_binding() {
    let helper = "def helper\n  1\nend\n";
    for body in [
        "helper = 5\n  helper()",
        "helper = helper()\n  helper()",
        "other = helper()\n  helper = 5\n  other = helper()",
    ] {
        rejected(
            &format!("{helper}def run\n  {body}\nend"),
            "value is not callable",
            "non-callable",
        );
    }
}

#[test]
fn methods_and_blocks_skip_bindings_that_an_enclosing_assignment_fills() {
    clean(
        "class Box\n  property total: int\n  def initialize\n    count = count()\n    @total = count\n  end\n  def count\n    7\n  end\nend\ndef run -> int\n  Box.new.total\nend",
        "7",
    );
    clean(
        "class User\n  property b: int\n  def touch\n    @b = 1\n  end\n  def initialize\n    touch = nil\n    touch = touch()\n  end\nend\ndef run -> int\n  User.new.b\nend",
        "1",
    );
    let helper = "def helper\n  1\nend\n";
    for (body, value) in [
        (
            "helper = 0\n  [1].each do\n    helper = helper()\n  end\n  helper",
            "1",
        ),
        ("helper = [1].map do\n    helper()\n  end\n  helper[0]", "1"),
        (
            "helper = [[1].map { helper() }, 2]\n  helper[0][0] + helper[1]",
            "3",
        ),
        (
            "values = [1].map do |helper|\n    helper = helper()\n  end\n  values[0]",
            "1",
        ),
    ] {
        clean(&format!("{helper}def run -> int\n  {body}\nend"), value);
    }
    // A block parameter's own assignment reaches the enclosing binding next.
    rejected(
        &format!(
            "{helper}def run\n  helper = 5\n  [1].map do |helper|\n    helper = helper()\n  end\nend"
        ),
        "value is not callable",
        "non-callable",
    );
    rejected(
        &format!("{helper}def run\n  helper = 5\n  [1].map do\n    helper()\n  end\nend"),
        "value is not callable",
        "non-callable",
    );
}

#[test]
fn namespace_bodies_skip_the_declaring_binding_they_assign() {
    clean(
        "def helper\n  6\nend\nhelper = 5\nclass A\n  helper = helper()\nend\nhelper",
        "6",
    );
    rejected(
        "def helper\n  1\nend\nhelper = 5\nclass A\n  X = helper()\nend\nA::X",
        "value is not callable",
        "non-callable",
    );
}
