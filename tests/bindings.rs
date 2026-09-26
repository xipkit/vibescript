use std::collections::BTreeMap;
use vibescript::{CallOptions, Engine, ErrorKind, Value};

fn run(source: &str, globals: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    run_with(&Engine::new(), source, globals)
}

fn run_with(
    engine: &Engine,
    source: &str,
    globals: BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let options = CallOptions {
        globals,
        ..CallOptions::default()
    };
    engine
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
         unused: int? = nil if false\n\
         [1].each { |inner|\n  seen = inner\n}\n\
         kind: symbol? = nil\n\
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
    let mut engine = Engine::new();
    for (name, ty) in [
        ("count", "int"),
        ("items", "array<int>"),
        ("untouched", "int"),
        ("read", "int"),
    ] {
        engine.declare_global(name, ty).unwrap();
    }
    let bindings = run_with(
        &engine,
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

/// An engine for a session's scripts: each binds the earlier scripts'
/// variables from the declared global `session`, as `vibes repl` does.
fn session_engine() -> Engine {
    let mut engine = Engine::new();
    engine
        .declare_global("session", "hash<string, any>")
        .unwrap();
    engine
}

/// Runs `source` with `bindings` as the session's variables and `classes`
/// as globals that shadow the declarations `source` carries.
fn continue_session(
    engine: &Engine,
    source: &str,
    bindings: &BTreeMap<String, Value>,
    classes: &[&str],
) -> (Value, BTreeMap<String, Value>) {
    let mut engine = engine.clone();
    for &name in classes {
        engine
            .declare_capability(&vibescript::Capability::from_value(
                name,
                bindings[name].clone(),
            ))
            .unwrap();
    }
    let mut globals: BTreeMap<String, Value> = classes
        .iter()
        .map(|&name| (name.to_owned(), bindings[name].clone()))
        .collect();
    let variables = bindings
        .iter()
        .filter(|(name, _)| !classes.contains(&name.as_str()))
        .map(|(name, value)| (name.as_bytes().to_vec(), value.clone()))
        .collect();
    globals.insert("session".to_owned(), Value::object(variables));
    let (outcome, mut bindings) = engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run_bindings(CallOptions {
            globals,
            ..CallOptions::default()
        })
        .unwrap();
    bindings.remove("session");
    (outcome.value, bindings)
}

#[test]
fn bindings_continue_a_session_across_scripts() {
    // A later script carries the class as source, so the checker knows it,
    // while the class value the first run left shadows the declaration, so
    // the instance keeps its class; the instance comes back typed through a
    // checked cast.
    const COUNTER: &str =
        "class Counter\n  @n: int = 0\n  def bump -> int\n    @n += 1\n  end\nend\n";
    let engine = session_engine();
    let mut session = BTreeMap::new();
    for source in [
        format!("{COUNTER}counter = Counter.new"),
        format!(
            "{COUNTER}counter = session.fetch(\"counter\").as(Counter)\ncounter.bump\ncounter.bump"
        ),
        format!("{COUNTER}counter = session.fetch(\"counter\").as(Counter)\nseen = counter.bump"),
    ] {
        let classes: &[&str] = if session.is_empty() {
            &[]
        } else {
            &["Counter"]
        };
        session = continue_session(&engine, &source, &session, classes).1;
    }
    assert_eq!(session["seen"].as_int(), Some(3));
    assert_eq!(session["counter"].type_name(), "instance");
    assert_eq!(session["Counter"].type_name(), "class");
}

#[test]
fn declared_types_keep_their_identity_in_later_scripts() {
    const DECLARATIONS: &str = "class Point
  getter x: int
  def initialize(@x: int)
  end
end
module Shapes
  SIDES = 4
end
enum Level
  Low
  High
end
";
    let engine = session_engine();
    let first = format!(
        "{DECLARATIONS}def helper -> int\n  1\nend\norigin = Point.new(3)\nlevel = Level::High"
    );
    let (_, bindings) = continue_session(&engine, &first, &BTreeMap::new(), &[]);
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
    let later = format!(
        "{DECLARATIONS}def measure(p: Point) -> int
  p.x + Shapes::SIDES
end
origin = session.fetch(\"origin\").as(Point)
level = session.fetch(\"level\").as(Level)
[origin.is_type?(:Point), level == Level::High, measure(origin), measure(Point.new(1))]"
    );
    let (value, bindings) =
        continue_session(&engine, &later, &bindings, &["Level", "Point", "Shapes"]);
    assert_eq!(value.to_string(), "[true, true, 7, 5]");
    assert!(!bindings.contains_key("measure"));
}

#[test]
fn a_supplied_global_shadows_a_declaration_of_the_same_name() {
    let globals = BTreeMap::from([("Point".to_owned(), Value::int(7))]);
    let mut engine = Engine::new();
    engine.declare_global("Point", "int").unwrap();
    let bindings = run_with(
        &engine,
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
        .compile("x = 1\n1 // 0")
        .unwrap()
        .run_bindings(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
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

#[test]
fn retained_type_globals_require_matching_declarations_and_identity() {
    for source in ["class C; def n -> int; 1; end; end", "enum C; A; B; end"] {
        let (_, bindings) = Engine::new()
            .compile(source)
            .unwrap()
            .run_bindings(CallOptions::default())
            .unwrap();
        let retained = bindings["C"].clone();
        let mut engine = Engine::new();
        engine
            .declare_capability(&vibescript::Capability::from_value("C", retained.clone()))
            .unwrap();
        let script = engine.compile(source).unwrap();
        script
            .run(CallOptions {
                globals: [("C".into(), retained)].into(),
                ..CallOptions::default()
            })
            .unwrap();
        assert!(engine.compile("class C; end").is_err());
        let (_, other) = Engine::new()
            .compile(source)
            .unwrap()
            .run_bindings(CallOptions::default())
            .unwrap();
        let error = script
            .run(CallOptions {
                globals: [("C".into(), other["C"].clone())].into(),
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(error.message.contains("identity"));
    }
}

#[test]
fn retained_classes_keep_the_types_of_their_original_dependencies() {
    for (before, after, class, expression) in [
        (
            "class D; def f -> int; 1; end; end;",
            "class D; def g -> string; 'new'; end; end;",
            "class C; def self.get -> D; D.new; end; end;",
            "C.get.g",
        ),
        (
            "type Item = int;",
            "type Item = string;",
            "class C; @x: Item; def initialize(@x: Item); end; def x -> Item; @x; end; end;",
            "C.new('s').x.upcase",
        ),
    ] {
        let source = format!("{before}{class}");
        let (_, bindings) = Engine::new()
            .compile(&source)
            .unwrap()
            .run_bindings(CallOptions::default())
            .unwrap();
        let retained = vibescript::Capability::from_value("C", bindings["C"].clone());
        let mut engine = Engine::new();
        engine.declare_capability(&retained).unwrap();
        engine
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                capabilities: vec![retained],
                ..CallOptions::default()
            })
            .unwrap();
        let error = engine
            .compile(&format!("{after}{class}{expression}"))
            .err()
            .unwrap();
        assert_eq!(error.diagnostics()[0].code.to_string(), "V0101");
    }
}
