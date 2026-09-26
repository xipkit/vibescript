use crate::{CallOptions, Engine, ErrorClass};

/// Checks the whole file and returns its diagnostic messages.
fn diagnostics(source: &str) -> Vec<String> {
    let script = Engine::legacy_unchecked().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect()
}

/// Runs `run`, or the top-level statements when the script declares no `run`.
fn witness(source: &str) -> Result<String, (Option<ErrorClass>, String)> {
    let script = Engine::legacy_unchecked().compile(source).unwrap();
    let result = if source.contains("def run") {
        script.call("run", &[], CallOptions::default())
    } else {
        script.run(CallOptions::default())
    };
    result
        .map(|outcome| outcome.value.to_string())
        .map_err(|error| (error.class(), error.message))
}

#[test]
fn loop_control_outside_a_loop_is_a_known_error_where_it_is_reached() {
    let breaking = "break used outside of loop";
    let skipping = "next used outside of loop";
    for (source, message) in [
        ("def run\n  break\nend", breaking),
        ("def run\n  next\nend", skipping),
        ("def run\n  break 5\nend", breaking),
        ("def run\n  next 5\nend", skipping),
        ("break", breaking),
        ("next", skipping),
        ("class A\n  next\nend", skipping),
        ("class A\n  break 5\nend", breaking),
        (
            "class S\n  def x=(n)\n    break\n  end\nend\ndef run\n  s = S.new\n  s.x = 1\nend",
            breaking,
        ),
        (
            "class Leaky\n  def +(other)\n    next\n  end\nend\ndef run\n  Leaky.new + 1\nend",
            skipping,
        ),
        (
            "def run\n  begin\n    break\n  ensure\n    1\n  end\nend",
            breaking,
        ),
    ] {
        assert_eq!(diagnostics(source), [message], "{source}");
        assert_eq!(
            witness(source),
            Err((Some(ErrorClass::Runtime), message.into())),
            "{source}"
        );
    }
}

#[test]
fn unreached_loop_control_and_block_transfers_stay_clean() {
    for (source, value) in [
        ("def run\n  if false\n    break\n  end\n  7\nend", "7"),
        ("def run\n  [1, 2].each { |v| break }\n  7\nend", "7"),
        ("def run\n  [1, 2].map { |v| next v * 2 }\nend", "[2, 4]"),
        (
            "def twice\n  yield\n  yield\n  3\nend\ndef run\n  twice { break 7 }\nend",
            "7",
        ),
        (
            "def run\n  while true\n    [1].each do\n      break\n    end\n    break\n  end\n  5\nend",
            "5",
        ),
        (
            "class A\n  X = [1, 2].map { |v| next v * 2 }\nend\ndef run\n  A::X\nend",
            "[2, 4]",
        ),
    ] {
        assert_eq!(diagnostics(source), Vec::<String>::new(), "{source}");
        assert_eq!(witness(source), Ok(value.into()), "{source}");
    }
}

#[test]
fn loop_control_errors_follow_rescue_and_call_boundaries() {
    // Without a looping caller the error belongs to the frame and its own rescue.
    let source = "def run -> string\n  begin\n    break\n  rescue RuntimeError => e\n    e.message\n  end\nend";
    assert_eq!(diagnostics(source), ["break used outside of loop"]);
    assert_eq!(witness(source), Ok("break used outside of loop".into()));

    // A looping caller receives a LocalJumpError that the callee cannot rescue.
    let source = "def helper\n  begin\n    break\n  rescue\n    1\n  end\nend\ndef run -> int\n  for i in [1]\n    begin\n      helper()\n    rescue LocalJumpError\n      return \"jump\"\n    end\n  end\n  7\nend";
    let messages = diagnostics(source);
    assert!(
        messages.contains(&"break used outside of loop".to_string()),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.starts_with("Return value:")),
        "{messages:?}"
    );
    assert_eq!(
        witness(source),
        Err((
            Some(ErrorClass::Runtime),
            "return value for run expected int, got string".into()
        ))
    );
    let source = "def helper\n  next\nend\ndef run\n  [1].map { |v| helper() }\nend";
    assert_eq!(diagnostics(source), ["next used outside of loop"]);
    assert_eq!(
        witness(source),
        Err((
            Some(ErrorClass::LocalJump),
            "next cannot cross call boundary".into()
        ))
    );
}
