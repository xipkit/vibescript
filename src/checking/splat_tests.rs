use crate::{CallOptions, CheckReport, Engine, HostMethod, Signature, SignatureParam, Value};

/// Callees of every parameter shape, in the order the call sites below use them.
const PRELUDE: &str = "\
enum Status
  Draft
end
class Box
end
class Point
  def initialize(x, y)
    @x = x
  end

  def pair(a, b = 2)
    [a, b]
  end
end
def rest(*args)
  args
end
def fixed(a, b)
  [a, b]
end
def optional(a, b = 2)
  [a, b]
end
def named(a:, b: 1)
  [a, b]
end
def kw(**opts)
  opts
end
def mixed(a, *more, key: 0, **opts)
  [a, more, key, opts]
end
";

/// A value of each kind a splat can meet, with an annotation admitting it where one exists.
const KINDS: [(&str, Option<&str>); 25] = [
    ("1", Some("int")),
    ("10 ** 30", None),
    ("1.5", Some("float")),
    ("\"s\"", Some("string")),
    (":s", Some("symbol")),
    ("nil", Some("nil")),
    ("true", Some("bool")),
    ("[]", Some("array<int>")),
    ("[1]", Some("array<int>")),
    ("[1, 2]", Some("array<int>")),
    ("[1, 2, 3]", Some("array<any>")),
    ("{}", Some("hash<string, int>")),
    ("{ a: 1 }", Some("hash<string, int>")),
    ("{ a: 1, b: 2 }", Some("{ a: int, b: int }")),
    ("{ key: 1 }", Some("{ key: int }")),
    ("1..3", Some("range")),
    ("money(\"1.00 USD\")", Some("money")),
    ("5.seconds", Some("duration")),
    ("Time.now", Some("time")),
    ("Status", None),
    ("Status::Draft", Some("Status")),
    ("Box.new", Some("Box")),
    ("Box", None),
    ("/ab/", None),
    ("JSON", None),
];

/// Call sites that splat `v` into script functions, methods, constructors, hosts and natives.
const CALLS: [&str; 32] = [
    "rest(*v)",
    "fixed(*v)",
    "fixed(1, *v)",
    "fixed(1, 2, 3, *v)",
    "optional(*v)",
    "named(*v)",
    "mixed(*v)",
    "mixed(1, *v)",
    "fixed(*v, 1)",
    "rest(*v, *v)",
    "kw(**v)",
    "named(**v)",
    "named(a: 5, **v)",
    "named(**v, a: 5)",
    "rest(**v)",
    "fixed(**v)",
    "fixed(1, **v)",
    "optional(1, **v)",
    "mixed(1, **v)",
    "rest(*v, **v)",
    "Point.new(*v)",
    "Point.new(1, 2).pair(*v)",
    "Point.new(1, 2).pair(**v)",
    "Box.new(*v)",
    "host(*v)",
    "host(7, **v)",
    "loose(*v)",
    "[0].push(*v)",
    "[0].first(*v)",
    "to_int(*v)",
    "to_int(\"1\", **v)",
    "JSON.stringify(1, **v)",
];

fn engine() -> Engine {
    let mut engine = Engine::new();
    let host = HostMethod::new("host", |_, _, _| Ok(Value::bytes(b"ok")))
        .with_signature(Signature {
            params: vec![
                SignatureParam {
                    name: "value".into(),
                    ty: "int".into(),
                    optional: false,
                },
                SignatureParam {
                    name: "extra".into(),
                    ty: "string".into(),
                    optional: true,
                },
            ],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap();
    engine.register_method("host", host);
    engine.register_method(
        "loose",
        HostMethod::new("loose", |_, args, _| Ok(Value::int(args.len() as i64))),
    );
    engine
}

fn check(source: &str) -> CheckReport {
    engine()
        .compile(source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .check_function("run", &CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"))
}

fn messages(report: &CheckReport) -> Vec<&str> {
    report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

fn splat_message(message: &str) -> bool {
    message.starts_with("Positional splat must be an array")
        || message.starts_with("Keyword splat must be a hash")
}

#[test]
fn splats_of_every_kind_agree_with_execution() {
    for call in CALLS {
        let mut always = true;
        for (kind, annotation) in KINDS {
            let source = format!("{PRELUDE}def run\n  v = ({kind})\n  {call}\nend\n");
            let script = engine()
                .compile(&source)
                .unwrap_or_else(|e| panic!("{source}: {e}"));
            let failure = script
                .call("run", &[], CallOptions::default())
                .err()
                .map(|error| error.to_string());
            always &= failure.is_some();
            let report = script
                .check_call("run", &[], &CallOptions::default())
                .unwrap();
            assert!(report.incomplete.is_empty(), "{kind} {call}: {report:?}");
            assert_eq!(
                !report.diagnostics.is_empty(),
                failure.is_some(),
                "{kind} {call}: {failure:?} {:?}",
                messages(&report)
            );
            assert_eq!(
                messages(&report)
                    .iter()
                    .any(|message| splat_message(message)),
                failure
                    .as_deref()
                    .is_some_and(|message| message.contains("splat argument must be")),
                "{kind} {call}: {failure:?} {:?}",
                messages(&report)
            );
            // Each declared domain holds one kind, so its splat verdict is this value's. Element
            // types stay strict for every count that binds, so only arity is compared here.
            if let Some(annotation) = annotation {
                let report = check(&format!(
                    "{PRELUDE}def run(v: {annotation})\n  {call}\nend\n"
                ));
                assert!(
                    report.incomplete.is_empty(),
                    "{annotation} {call}: {report:?}"
                );
                assert_eq!(
                    messages(&report)
                        .iter()
                        .any(|message| splat_message(message)),
                    failure
                        .as_deref()
                        .is_some_and(|message| message.contains("splat argument must be")),
                    "{annotation} {call}: {failure:?} {:?}",
                    messages(&report)
                );
                assert!(
                    failure.is_some()
                        || messages(&report)
                            .iter()
                            .all(|message| message.contains(": expected ")),
                    "{annotation} {call}: {:?}",
                    messages(&report)
                );
            }
        }
        // A gradual value spreads into gradual arguments unless every value fails.
        let report = check(&format!("{PRELUDE}def run(v)\n  {call}\nend\n"));
        assert!(report.incomplete.is_empty(), "{call}: {report:?}");
        assert!(
            report.diagnostics.is_empty() || always,
            "{call}: {:?}",
            messages(&report)
        );
    }
}

#[test]
fn uncertain_counts_fail_only_when_every_count_fails() {
    for (source, expected) in [
        (
            "def f(*xs); xs; end; def run(xs: array<int>); f(*xs); end",
            &[][..],
        ),
        ("def f(a, b); a; end; def run(x); f(*x); end", &[]),
        (
            "def f(a, b); a; end; def run(x); f(1, 2, 3, *x); end",
            &["\"f\": too many positional arguments"],
        ),
        (
            "def f(a:); a; end; def run(x); f(*x); end",
            &["\"f\": missing argument \"a\""],
        ),
        (
            "def f(a, b = 2); a; end; def run(flag: bool); f(*(flag ? [1] : [1, 2, 3])); end",
            &[],
        ),
        // Arrays of zero or three values never fit two parameters.
        (
            "def f(a, b); a; end; def run(flag: bool); f(*(flag ? [] : [1, 2, 3])); end",
            &[
                "\"f\": missing argument \"a\"",
                "\"f\": missing argument \"b\"",
            ],
        ),
        (
            "def f(a, b); a; end; def run(xs: array<int>, ys: array<int>); f(*xs, 1, *ys); end",
            &[],
        ),
        // Arrays of known lengths keep each value in its own position.
        (
            "def f(a: int, b: string = \"x\"); a; end; def run(flag: bool); f(*(flag ? [1] : [1, \"s\"])); end",
            &[],
        ),
        (
            "def f(a, b: 0); a; end; def run(p: bool, q: bool); f(*(p ? [] : [1]), *(q ? [2] : [])); end",
            &[],
        ),
        (
            "def f(a, b, c); a; end; def run(p: bool, q: bool); f(*(p ? [] : [1]), *(q ? [2] : [])); end",
            &[
                "\"f\": missing argument \"a\"",
                "\"f\": missing argument \"b\"",
                "\"f\": missing argument \"c\"",
            ],
        ),
        // Every count that binds keeps its element types strict.
        (
            "def f(a: int); a; end; def run(x: array<string>); f(*x); end",
            &["\"f\": argument \"a\": expected int, got string"],
        ),
        (
            "def f(a: int, b: string = \"x\"); a; end; def run(x: array<int>); f(*x); end",
            &["\"f\": argument \"b\": expected string, got int"],
        ),
        (
            "def f(*a) -> array<int>; a; end; def run(xs: array<int>); f(*xs); end",
            &[],
        ),
        (
            "def f(a, *r) -> array<int>; r; end; def run(xs: array<int>); f(*xs, \"tail\"); end",
            &["Return value: expected array<int>, got [] | [string] | array<int | string>"],
        ),
        (
            "def f(*a); a; end; def run(v: array<int>?); f(*v); end",
            &["Positional splat must be an array; got nil | array<int>"],
        ),
    ] {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(messages(&report), expected, "{source}");
    }
}

#[test]
fn hash_splats_bind_optional_and_unknown_keywords() {
    for (source, expected) in [
        (
            "def f(**xs); xs; end; def run(xs: hash<string, int>); f(**xs); end",
            &[][..],
        ),
        ("def f(a:, b: 1); a; end; def run(x); f(**x); end", &[]),
        (
            "def f(a:, b: 1); a; end; def run(flag: bool); f(**(flag ? {a: 1} : {})); end",
            &[],
        ),
        (
            "def f(a:); a; end; def run(flag: bool); f(**(flag ? {b: 1} : {})); end",
            &["\"f\": missing argument \"a\""],
        ),
        // An unknown key can replace an earlier keyword but not a later one.
        (
            "def f(a: int); a; end; def run(h: hash<string, string>); f(a: 1, **h); end",
            &["\"f\": argument \"a\": expected int, got string | int"],
        ),
        (
            "def f(a: int); a; end; def run(h: hash<string, string>); f(**h, a: 1); end",
            &[],
        ),
        (
            "def f(**o) -> hash<string, int>; o; end; def run(h: hash<string, string>); f(**h); end",
            &["Return value: expected hash<string, int>, got {} | hash<string, string>"],
        ),
        // Keywords become an options hash only when there are some.
        (
            "def f(a, b); b; end; def run(h: hash<string, int>); f(1, **h); end",
            &[],
        ),
        (
            "def f(a); a; end; def run(h: hash<string, int>); f(1, 2, **h); end",
            &["\"f\": too many positional arguments"],
        ),
        (
            "def f(a, b:); a; end; def run(h); f(1, 2, **h); end",
            &[
                "\"f\": missing argument \"b\"",
                "\"f\": too many positional arguments",
            ],
        ),
    ] {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(messages(&report), expected, "{source}");
    }
}

#[test]
fn splats_narrow_the_local_they_expand() {
    for (source, expected) in [
        // The rescue sees only the hash that failed to expand.
        (
            "def rest(*a); a; end; def run(flag: bool); v = flag ? [1] : {a: 1}; begin; rest(*v); v.push(2); rescue; v.keys; end; end",
            &["Positional splat must be an array; got [int] | {\"a\": int}"][..],
        ),
        // No value is both an array and a hash, so the call never returns.
        (
            "def takes(v: int); v; end; def run(flag: bool); items = []; args = flag ? [1] : {x: 1}; items.fill(*args, **args); takes(\"unreachable\"); end",
            &[
                "Keyword splat must be a hash; got [int]",
                "Positional splat must be an array; got [int] | {\"x\": int}",
            ],
        ),
    ] {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(messages(&report), expected, "{source}");
    }
}

#[test]
fn natives_take_uncertain_splats_gradually() {
    for source in [
        "def run(keys); h = {}; h.fetch_values(*keys) { |k| [k, k] }.length; end",
        "def run(s); Regexp.union(*s); end",
        "def run(h: hash<string, int>, args: array<any>, opts); h.store(*args, **opts); h; end",
        "def run(items: array<int>, args); items.fill(*args); items.insert(*args); items.push(*args); items; end",
        "def run(items: array<int>, flag: bool); options = flag ? {} : { extra: 2 }; items.push(1, **options); end",
        "class R; def first(v); v; end; end; def run(names: array<symbol>); R.new.send(*names, \"x\"); end",
        "def run(xs: array<int>); puts(*xs); format(\"%d\", *xs); [1].send(:push, *xs); end",
    ] {
        let report = check(source);
        assert!(report.is_clean(), "{source}: {report:?}");
    }
}

#[test]
fn host_signatures_check_every_splatted_count() {
    for (source, expected) in [
        ("def run(xs: array<string>); host(1, *xs); end", &[][..]),
        (
            "def run(xs: array<int>); host(*xs); end",
            &["\"host\": argument 2: expected string, got int"],
        ),
        (
            "def run(xs: array<int>); host(1, 2, 3, *xs); end",
            &["\"host\": wrong number of arguments"],
        ),
        ("def run(xs: array<any>); loose(*xs); end", &[]),
        (
            "def run(flag: bool); host(*(flag ? [1] : [1, \"s\"])); end",
            &[],
        ),
    ] {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(messages(&report), expected, "{source}");
    }
}
