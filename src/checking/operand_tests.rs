use super::{
    entry::{self, Call},
    facts::{Atom, Node},
    normalization_tests::observed,
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine, Value, value::Kind};

/// Declares the types the operands use. `Ops` defines its own operators, so a
/// gradual receiver can always succeed through source dispatch.
const PRELUDE: &str = "enum Status
  Draft
  Published
end

class Box
end

class Ops
  def +(o); 1; end
  def -(o); 1; end
  def *(o); 1; end
  def /(o); 1; end
  def %(o); 1; end
  def **(o); 1; end
  def ==(o); true; end
  def <(o); true; end
  def <=(o); true; end
  def >(o); true; end
  def >=(o); true; end
  def <=>(o); 0; end
  def &(o); 1; end
  def [](i); 1; end
end
";

/// One expression for each kind of value the native operators distinguish.
const OPERANDS: [&str; 23] = [
    "1",
    "1.5",
    "(2 ** 80)",
    "\"s\"",
    ":s",
    "nil",
    "true",
    "[1]",
    "[]",
    "{ a: 1 }",
    "{}",
    "(1..2)",
    "money(\"1.00 USD\")",
    "2.hours",
    "Time.at(0)",
    "Status::Draft",
    "Status",
    "Box.new",
    "Box",
    "/a/",
    "\"a\".match(/a/)",
    "JSON",
    "Ops.new",
];

/// Checks one concrete expression against its execution and returns whether
/// the runtime failed.
///
/// Analysis must finish. A diagnostic requires a runtime failure; a success
/// requires a clean report whose result contains the actual value; any other
/// failure must appear among the possible errors of the call.
#[track_caller]
fn concrete(expression: &str) -> bool {
    let source = format!("{PRELUDE}\ndef run\n  {expression}\nend\n");
    let script = Engine::new()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{expression}: {error}"));
    let options = CallOptions::default();
    let actual = script.call("run", &[], options.clone());
    let mut ctx = CallContext::new(options.clone());
    let mut checked = entry::check(
        &mut ctx,
        Call {
            script: &script,
            name: "run",
            arguments: &[],
            keywords: &[],
            options: &options,
        },
    )
    .unwrap();
    assert!(
        checked.analysis.incomplete.data.is_empty(),
        "{expression}: {checked:?}"
    );
    let issues = !checked.analysis.issues.data.is_empty();
    let failed = match actual {
        Ok(outcome) => {
            assert!(!issues, "{expression} = {}: {checked:?}", outcome.value);
            if literal(&outcome.value) {
                let value = observed(
                    &mut ctx,
                    &mut checked.facts,
                    &script.inner.code.program,
                    &outcome.value,
                );
                let returns = checked.analysis.returns;
                assert_ne!(
                    checked.facts.relation(&mut ctx, value, returns).unwrap(),
                    Relation::Rejected,
                    "{expression} = {}: {checked:?}",
                    outcome.value
                );
            }
            false
        }
        Err(error) => {
            let class = error.class().unwrap();
            assert!(
                issues || checked.analysis.throws & (1 << class as u8) != 0,
                "{expression}: {error}: {checked:?}"
            );
            true
        }
    };
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{expression}");
    failed
}

/// Returns the integer or nil that analysis infers for a concrete expression,
/// or `None` when the inferred result is not a single such value.
fn exact(expression: &str) -> Option<Option<i64>> {
    let source = format!("{PRELUDE}\ndef run\n  {expression}\nend\n");
    let script = Engine::new().compile(&source).unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(options.clone());
    let checked = entry::check(
        &mut ctx,
        Call {
            script: &script,
            name: "run",
            arguments: &[],
            keywords: &[],
            options: &options,
        },
    )
    .unwrap();
    let value = match checked.facts.node(checked.analysis.returns) {
        Node::Integer(n) => Some(Some(*n)),
        Node::Atom(Atom::Nil) => Some(None),
        _ => None,
    };
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{expression}");
    value
}

/// Whether a value has a literal fact. Source objects and types do not; their
/// identities are covered by the namespace tests.
fn literal(value: &Value) -> bool {
    match &value.0 {
        Kind::Instance(_) | Kind::Namespace(_) | Kind::Shape(_) => false,
        Kind::Array(items) => items.buffer.data.iter().all(literal),
        Kind::Hash(hash) => hash.buffer.data.iter().all(|(_, value)| literal(value)),
        _ => true,
    }
}

/// Checks an expression over an unannotated parameter `x` and returns whether
/// analysis reported a known contradiction.
#[track_caller]
fn gradual(expression: &str) -> bool {
    let source = format!("{PRELUDE}\ndef run(x)\n  {expression}\nend\n");
    let script = Engine::new()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{expression}: {error}"));
    let options = CallOptions::default();
    let mut ctx = CallContext::new(options.clone());
    let checked = entry::check_function(&mut ctx, &script, "run", &options).unwrap();
    assert!(
        checked.analysis.incomplete.data.is_empty(),
        "{expression}: {checked:?}"
    );
    let issues = !checked.analysis.issues.data.is_empty();
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{expression}");
    issues
}

/// Compares every pair of operand kinds for `op`, then requires a gradual
/// operand to be rejected only when every concrete kind in its place fails.
fn binary(op: &str) -> usize {
    let mut failed = [[false; OPERANDS.len()]; OPERANDS.len()];
    for (l, left) in OPERANDS.iter().enumerate() {
        for (r, right) in OPERANDS.iter().enumerate() {
            failed[l][r] = concrete(&format!("{left} {op} {right}"));
        }
    }
    for (r, right) in OPERANDS.iter().enumerate() {
        let expression = format!("x {op} {right}");
        if gradual(&expression) {
            assert!(failed.iter().all(|row| row[r]), "{expression}");
        }
    }
    for (l, left) in OPERANDS.iter().enumerate() {
        let expression = format!("{left} {op} x");
        if gradual(&expression) {
            assert!(failed[l].iter().all(|&failed| failed), "{expression}");
        }
    }
    OPERANDS.len() * (OPERANDS.len() + 2)
}

#[test]
fn arithmetic_operators_agree_with_execution_for_every_operand_kind() {
    let mut count = 0;
    for op in ["+", "-", "*", "/", "//", "%", "**"] {
        count += binary(op);
    }
    assert_eq!(count, 4025);
}

#[test]
fn comparison_operators_agree_with_execution_for_every_operand_kind() {
    let mut count = 0;
    for op in ["==", "!=", "<", "<=", ">", ">=", "<=>", "==="] {
        count += binary(op);
    }
    assert_eq!(count, 4600);
}

#[test]
fn collection_logical_and_match_operators_agree_with_execution_for_every_operand_kind() {
    let mut count = 0;
    for op in ["<<", "&", "=~", "!~", "&&", "||"] {
        count += binary(op);
    }
    assert_eq!(count, 3450);
}

#[test]
fn unary_operators_agree_with_execution_for_every_operand_kind() {
    for op in ["-", "+", "!"] {
        let failed: Vec<_> = OPERANDS
            .iter()
            .map(|value| concrete(&format!("{op}({value})")))
            .collect();
        let expression = format!("{op}x");
        if gradual(&expression) {
            assert!(failed.iter().all(|&failed| failed), "{expression}");
        }
    }
}

#[test]
fn index_reads_agree_with_execution_for_every_operand_kind() {
    let mut failed = [[false; OPERANDS.len()]; OPERANDS.len()];
    for (r, receiver) in OPERANDS.iter().enumerate() {
        for (i, index) in OPERANDS.iter().enumerate() {
            failed[r][i] = concrete(&format!("({receiver})[{index}]"));
        }
    }
    for (i, index) in OPERANDS.iter().enumerate() {
        let expression = format!("x[{index}]");
        if gradual(&expression) {
            assert!(failed.iter().all(|row| row[i]), "{expression}");
        }
    }
    for (r, receiver) in OPERANDS.iter().enumerate() {
        let expression = format!("({receiver})[x]");
        if gradual(&expression) {
            assert!(failed[r].iter().all(|&failed| failed), "{expression}");
        }
    }
}

#[test]
fn symbol_reductions_agree_with_execution_for_every_operand_kind() {
    // Reductions apply native operators directly, without source dispatch.
    for op in ["+", "-", "*", "/", "%", "**", "<<", "&"] {
        for left in OPERANDS {
            for right in OPERANDS {
                concrete(&format!("[{left}, {right}].reduce(:{op})"));
            }
        }
    }
    for left in OPERANDS {
        concrete(&format!("[{left}].sum"));
        for right in OPERANDS {
            concrete(&format!("[{left}, {right}].sum"));
        }
    }
}

#[test]
fn spaceship_orders_arrays_by_element_and_leaves_other_pairs_nil() {
    for (expression, expected) in [
        ("[1, 2] <=> [1, 3]", Some(-1)),
        ("[1, 2] <=> [1]", Some(1)),
        ("[] <=> [1]", Some(-1)),
        ("[] <=> []", Some(0)),
        ("[\"a\", \"b\"] <=> [\"a\", \"c\"]", Some(-1)),
        ("[[1, [2]]] <=> [[1, [3]]]", Some(-1)),
        ("[:north, 300] <=> [:south, 200]", Some(-1)),
        ("[true] <=> [false]", Some(1)),
        ("[nil] <=> [nil]", Some(0)),
        ("[1, 2] <=> [1, \"a\"]", None),
        ("[1] <=> 1", None),
        ("1 <=> [1]", None),
        ("{ a: 1 } <=> { a: 1 }", None),
        ("true <=> [1]", None),
        ("Box.new <=> Box.new", None),
        ("Box <=> Box", None),
    ] {
        concrete(expression);
        assert_eq!(exact(expression), Some(expected), "{expression}");
    }
    // Equal element facts may describe one aliased array, which the runtime
    // orders as equal before comparing elements, and float facts do not
    // record NaN; analysis keeps both results for these.
    for expression in [
        "[{ a: 1 }] <=> [{ a: 1 }]",
        "nan = 0.0 / 0.0\n  [1, nan] <=> [1, 2.0]",
    ] {
        concrete(expression);
    }
}
