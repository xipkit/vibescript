use crate::{CallOptions, CheckReport, Engine, HostMethod, Signature, SignatureParam, Value};

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
