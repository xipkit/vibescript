//! Walks every builtin member name on each receiver kind across call shapes and
//! holds the checker to what the runtime does with the same expression.

use std::collections::BTreeMap;
use vibescript::{CallOptions, CheckReport, Engine, ErrorKind, Script, Value};

const PRELUDE: &str = "enum Status\n  Draft\n  Published\nend\n\
    def failure\n  begin\n    raise \"bad\"\n  rescue => error\n    error\n  end\nend\n";

/// Receivers whose facts are literals, including the enum kinds, match data
/// and error objects that `tooling::member_names` does not list.
const LITERALS: &[&str] = &[
    "\"abc\"",
    ":abc",
    "[1, 2]",
    "{ a: 1 }",
    "3",
    "2.5",
    "money(\"1.00 USD\")",
    "2.hours",
    "Time.at(0)",
    "(1..3)",
    "nil",
    "true",
    "/a/",
    "Status",
    "Status::Draft",
    "\"abc\".match(\"b\")",
    "failure()",
];

/// Typed parameters whose facts admit every value of the kind, with the
/// witness the runtime receives.
fn parameters() -> Vec<(&'static str, Value)> {
    vec![
        ("string", Value::bytes("abc")),
        ("symbol", Value::symbol("abc")),
        ("array", Value::array(vec![Value::int(1), Value::int(2)])),
        ("hash", Value::hash(vec![(b"a".to_vec(), Value::int(1))])),
        ("hash", Value::object(vec![(b"a".to_vec(), Value::int(1))])),
        ("int", Value::int(3)),
        ("float", Value::float(2.5)),
        ("money", Value::money(100, "USD").unwrap()),
        ("duration", Value::duration(7200)),
        ("time", Value::time(0, 0).unwrap()),
        ("range", Value::range(Some(1), Some(3), false)),
        ("bool", Value::boolean(true)),
    ]
}

const SHAPES: &[&str] = &[
    "",
    "()",
    "(1)",
    " { |v| v }",
    "(1, 2)",
    "(\"a\")",
    "([1])",
    "(a: 1)",
    "(*[1])",
    "(1) { |v| v }",
    "(:a, 1)",
    "({ a: :b })",
    "(0, 1)",
    "(nil)",
    "(1.5)",
    "(-1)",
    "(0..1)",
    "(:a)",
    "(\"a\", \"b\")",
    "(1, \"a\")",
    "([1], [2])",
    "(1, 2, 3)",
    "(1, a: 1)",
    "(**{ a: 1 })",
    "(Time.at(0))",
    "(2.hours)",
];

/// Names outside every member table, which each receiver must refuse.
const FOREIGN: &[&str] = &["each_with_object", "then", "zzz"];

/// A disagreement between the checker and the runtime for one call.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Mismatch {
    /// The checker did not analyze the call.
    Incomplete,
    /// A known error, although the runtime succeeded.
    ErrorButRuns,
    /// A clean call that failed at runtime without the checker keeping a
    /// possible failure, so its rescue path looked unreachable.
    CleanButFailsWithoutRescue,
    /// The result fact excludes the kind of value the runtime returned.
    ResultExcludesRuntimeValue,
}

struct Receiver {
    parameter: Option<&'static str>,
    witness: Vec<Value>,
    expression: &'static str,
}

impl Receiver {
    fn compile(&self, body: &str, returns: Option<&str>) -> Script {
        let parameter = self
            .parameter
            .map_or(String::new(), |ty| format!("(x: {ty})"));
        let returns = returns.map_or(String::new(), |ty| format!(" -> {ty}"));
        let source = format!("{PRELUDE}def run{parameter}{returns}\n{body}\nend\n");
        Engine::new()
            .compile(&source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
    }

    /// Classifies one member call; `None` when checker and runtime agree.
    fn classify(&self, call: &str) -> Option<Mismatch> {
        let expression = format!("{}{call}", self.expression);
        let script = self.compile(&format!("  {expression}"), None);
        let report = check(&script);
        if !report.incomplete.is_empty() {
            return Some(Mismatch::Incomplete);
        }
        let known = !report.diagnostics.is_empty();
        match script.call("run", &self.witness, CallOptions::default()) {
            Ok(_) if known => Some(Mismatch::ErrorButRuns),
            Ok(outcome) => {
                let ty = match outcome.value.type_name() {
                    "object" => "hash",
                    ty @ ("int" | "float" | "string" | "bool" | "nil" | "duration" | "time"
                    | "money" | "range" | "symbol" | "array" | "hash") => ty,
                    _ => return None,
                };
                // The branch reports its undefined call only when the result
                // fact admits the runtime value's kind.
                let body = format!(
                    "  result = {expression}\n  if result.is_type?(:{ty})\n    undefined_call()\n  end"
                );
                let report = check(&self.compile(&body, None));
                (!report.incomplete.is_empty() || report.diagnostics.is_empty())
                    .then_some(Mismatch::ResultExcludesRuntimeValue)
            }
            // Exhausted quotas are execution limits rather than member failures.
            Err(error) if known || matches!(error.kind, ErrorKind::Steps | ErrorKind::Memory) => {
                None
            }
            Err(_) => {
                let body =
                    format!("  begin\n    {expression}\n    1\n  rescue\n    \"rescued\"\n  end");
                let report = check(&self.compile(&body, Some("int")));
                (!report.incomplete.is_empty() || !returns_bad_type(&report))
                    .then_some(Mismatch::CleanButFailsWithoutRescue)
            }
        }
    }
}

fn check(script: &Script) -> CheckReport {
    script
        .check_function("run", &CallOptions::default())
        .unwrap()
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.starts_with("Return value:"))
}

fn receivers() -> Vec<Receiver> {
    let mut receivers: Vec<Receiver> = LITERALS
        .iter()
        .map(|&expression| Receiver {
            parameter: None,
            witness: Vec::new(),
            expression,
        })
        .collect();
    for (ty, witness) in parameters() {
        receivers.push(Receiver {
            parameter: Some(ty),
            witness: vec![witness],
            expression: "x",
        });
    }
    receivers
}

#[test]
fn builtin_members_agree_with_the_runtime_in_every_call_shape() {
    let mut names: Vec<&str> = vibescript::tooling::member_names()
        .into_iter()
        .flat_map(|(_, names)| names)
        .chain(FOREIGN.iter().copied())
        .collect();
    names.sort_unstable();
    names.dedup();
    let count = receivers().len();
    // Each receiver's calls are independent, so they are classified in parallel.
    let found: Vec<Vec<(Mismatch, String)>> = std::thread::scope(|scope| {
        let names = &names;
        let workers: Vec<_> = (0..count)
            .map(|index| {
                scope.spawn(move || {
                    let receiver = receivers().swap_remove(index);
                    let mut found = Vec::new();
                    for name in names {
                        for shape in SHAPES {
                            let call = format!(".{name}{shape}");
                            if let Some(mismatch) = receiver.classify(&call) {
                                let parameter = receiver
                                    .parameter
                                    .map_or(String::new(), |ty| format!("x: {ty} => "));
                                let expression =
                                    format!("{parameter}{}{call}", receiver.expression);
                                found.push((mismatch, expression));
                            }
                        }
                    }
                    found
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    let mut mismatches: BTreeMap<Mismatch, Vec<String>> = BTreeMap::new();
    for (mismatch, expression) in found.into_iter().flatten() {
        mismatches.entry(mismatch).or_default().push(expression);
    }
    let mut summary = String::new();
    for (mismatch, expressions) in &mismatches {
        summary += &format!("{mismatch:?}: {}\n", expressions.len());
        for expression in expressions {
            summary += &format!("  {expression}\n");
        }
    }
    let cases = count * names.len() * SHAPES.len();
    assert!(mismatches.is_empty(), "{cases} cases\n{summary}");
}
