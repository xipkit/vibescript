use std::collections::BTreeMap;
use vibescript::{CallOptions, Engine, ErrorKind, Value};

fn run(source: &str, globals: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let options = CallOptions {
        globals,
        ..CallOptions::default()
    };
    Engine::new()
        .compile(source)
        .unwrap()
        .run_bindings(options)
        .unwrap()
        .1
}

fn ints(bindings: &BTreeMap<String, Value>) -> Vec<(&str, Option<i64>)> {
    bindings
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_int()))
        .collect()
}

#[test]
fn top_level_locals_survive_the_run() {
    let bindings = run(
        "first, *rest, last = [1, 2, 3, 4]\n\
         total = 0\n\
         for n in rest\n  total += n\nend\n\
         i = 0\nwhile i < 3\n  i += 1\nend\n\
         unused = nil if false\n\
         [1].each do |inner|\n  seen = inner\nend\n\
         if total == 5\n  kind = :five\nend\n\
         kind",
        BTreeMap::new(),
    );
    let names: Vec<_> = bindings.keys().map(String::as_str).collect();
    // A local assigned only on a branch that never ran is bound to nil, as a
    // later read in the same script would see it.
    assert_eq!(
        names,
        ["first", "i", "kind", "last", "n", "rest", "total", "unused"]
    );
    assert_eq!(bindings["unused"].type_name(), "nil");
    assert_eq!(bindings["first"].as_int(), Some(1));
    assert_eq!(bindings["last"].as_int(), Some(4));
    assert_eq!(bindings["total"].as_int(), Some(5));
    assert_eq!(bindings["i"].as_int(), Some(3));
    assert_eq!(bindings["kind"].as_bytes(), Some(&b"five"[..]));
    let rest: Vec<_> = bindings["rest"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_int)
        .collect();
    assert_eq!(rest, [Some(2), Some(3)]);
}

#[test]
fn outcome_matches_an_ordinary_run() {
    let source = "values = [3, 1, 2].sort\nvalues.sum";
    let script = Engine::new().compile(source).unwrap();
    let plain = script.run(CallOptions::default()).unwrap();
    let (outcome, bindings) = script.run_bindings(CallOptions::default()).unwrap();
    assert_eq!(outcome.value.as_int(), plain.value.as_int());
    assert_eq!(outcome.value.as_int(), Some(6));
    assert_eq!(bindings["values"].as_array().unwrap().len(), 3);
}

#[test]
fn globals_come_back_with_the_values_the_run_left() {
    let items = Value::array(vec![Value::int(1)]);
    let globals = BTreeMap::from([
        ("count".to_owned(), Value::int(1)),
        ("items".to_owned(), items.clone()),
        ("untouched".to_owned(), Value::int(9)),
        ("read".to_owned(), Value::int(4)),
    ]);
    let bindings = run(
        "count += 1\nitems.push(2)\nlocal = read * 2\n[1].each { |x| count += x }",
        globals,
    );
    assert_eq!(
        ints(&bindings),
        [
            ("count", Some(3)),
            ("items", None),
            ("local", Some(8)),
            ("read", Some(4)),
            ("untouched", Some(9)),
        ]
    );
    let updated: Vec<_> = bindings["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_int)
        .collect();
    assert_eq!(updated, [Some(1), Some(2)]);
    // The host's original stays unchanged.
    assert_eq!(items.as_array().unwrap().len(), 1);
}

#[test]
fn bindings_continue_a_session_across_scripts() {
    let engine = Engine::new();
    let mut session = BTreeMap::new();
    for source in [
        "class Counter\n  def initialize\n    @n = 0\n  end\n  def bump\n    @n += 1\n  end\nend\n\
         counter = Counter.new",
        "counter.bump\ncounter.bump",
        "seen = counter.bump",
    ] {
        let options = CallOptions {
            globals: session,
            ..CallOptions::default()
        };
        session = engine
            .compile(source)
            .unwrap()
            .run_bindings(options)
            .unwrap()
            .1;
    }
    assert_eq!(session["seen"].as_int(), Some(3));
    assert_eq!(session["counter"].type_name(), "instance");
    assert_eq!(session["Counter"].type_name(), "class");
}

#[test]
fn declared_types_keep_their_identity_in_later_scripts() {
    let engine = Engine::new();
    let first = "class Point
  def initialize(x)
    @x = x
  end
  def x
    @x
  end
end
                 module Shapes
  SIDES = 4
end
                 enum Level
  Low
  High
end
                 def helper
  1
end
                 origin = Point.new(3)
level = Level::High";
    let (_, bindings) = engine
        .compile(first)
        .unwrap()
        .run_bindings(CallOptions::default())
        .unwrap();
    let kinds: Vec<_> = bindings
        .iter()
        .map(|(name, value)| (name.as_str(), value.type_name()))
        .collect();
    // Functions are not values, so they are not bindings.
    assert_eq!(
        kinds,
        [
            ("Level", "enum"),
            ("Point", "class"),
            ("Shapes", "class"),
            ("level", "enum value"),
            ("origin", "instance"),
        ]
    );
    let options = CallOptions {
        globals: bindings,
        ..CallOptions::default()
    };
    let later = "def measure(p: Point) -> int
  p.x + Shapes::SIDES
end
                 [origin.is_a?(Point), level == Level::High, measure(origin), measure(Point.new(1))]";
    let (outcome, bindings) = engine
        .compile(later)
        .unwrap()
        .run_bindings(options)
        .unwrap();
    assert_eq!(outcome.value.to_string(), "[true, true, 7, 5]");
    assert!(!bindings.contains_key("measure"));
}

#[test]
fn a_supplied_global_shadows_a_declaration_of_the_same_name() {
    let globals = BTreeMap::from([("Point".to_owned(), Value::int(7))]);
    let bindings = run(
        "class Point
end
kind = Point",
        globals,
    );
    assert_eq!(ints(&bindings), [("Point", Some(7)), ("kind", Some(7))]);
}

#[test]
fn failures_report_no_bindings() {
    let error = Engine::new()
        .compile("x = 1\nmissing_name")
        .unwrap()
        .run_bindings(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
    let frame = &error.diagnostic.as_ref().unwrap().frames[0];
    assert_eq!(frame.position.line, 2);
}

#[test]
fn bindings_respect_the_call_limits() {
    let mut options = CallOptions::default();
    options.limits.steps = Some(50);
    let error = Engine::new()
        .compile("i = 0\nwhile true\n  i += 1\nend")
        .unwrap()
        .run_bindings(options)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn a_top_level_return_still_reports_its_locals() {
    let (outcome, bindings) = Engine::new()
        .compile("x = 5\nreturn x * 2 if x > 1\ny = 3")
        .unwrap()
        .run_bindings(CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(10));
    assert_eq!(ints(&bindings), [("x", Some(5))]);
}
