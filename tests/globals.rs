mod common;

use vibescript::{CallOptions, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

/// Runs `source`, whose top level binds the capitalized global `Store`,
/// with the host global `input`, declared as `ty`.
fn run(source: &str, ty: &str, input: &Value) -> vibescript::Result<vibescript::Outcome> {
    let mut engine = vibescript::Engine::new();
    engine.declare_global("input", ty).unwrap();
    engine.compile(source)?.run(CallOptions {
        globals: [("input".to_owned(), input.clone())].into(),
        ..CallOptions::default()
    })
}

/// A function cannot assign a capitalized name, so the programs bind the
/// global `Store` at top level, beside the host global `input`.
#[test]
fn global_mutations_preserve_local_bindings_and_pending_writes() {
    for (body, expected) in [
        (
            "Store.push(input.push(3).length)",
            serde_json::json!([[1, 2], [1, 3]]),
        ),
        (
            "Store.push(Store.push(2).length)",
            serde_json::json!([[1, 2, 2], [1]]),
        ),
        ("Store[0]=9", serde_json::json!([[9], [1]])),
        ("Store[0]=Store.fetch(0)+4", serde_json::json!([[5], [1]])),
        ("Store.push(2)", serde_json::json!([[1, 2], [1]])),
        ("Store.fill(4)", serde_json::json!([[4], [1]])),
        (
            "begin;Store.insert(-9,2);rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
        (
            "begin;Store[9]=2;rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
        (
            "begin;Store.delete_if {|i| raise \"stop\"};rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
    ] {
        let source = format!("Store=input\n{body}\n[Store,input]");
        let input = Value::array(vec![Value::int(1)]);
        let output =
            run(&source, "array<int>", &input).unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(json(&output.value), expected, "{body}");
        assert_eq!(json(&input), serde_json::json!([1]), "{body}");
    }
}

#[test]
fn replacing_a_global_detaches_an_earlier_mutation_target() {
    for (body, expected) in [
        ("Store.push(begin;Store=[9];2;end)", serde_json::json!([9])),
        // Plain indexed assignment selects its target after the right-hand side.
        ("Store[0]=begin;Store=[9];2;end", serde_json::json!([2])),
        (
            "Store.push(begin;Store,other=[9],2;other;end)",
            serde_json::json!([9]),
        ),
        // A block rebinds the global while the argument runs.
        (
            "Store.push(loop{Store=[9];break 2})",
            serde_json::json!([9]),
        ),
        ("Store.push(begin;Store=[1];2;end)", serde_json::json!([1])),
        (
            "Store.push(begin;Store=Store;2;end)",
            serde_json::json!([1, 2]),
        ),
    ] {
        let source = format!("Store=input\n{body}\n[Store,input]");
        let output = run(&source, "array<int>", &Value::array(vec![Value::int(1)]))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(
            json(&output.value),
            serde_json::json!([expected, [1]]),
            "{body}"
        );
    }
    // Compound assignment selects its target before the right-hand side. An
    // array element may be missing, so a record's field shows it.
    let input = Value::hash(vec![(b"a".to_vec(), Value::int(1))]);
    let output = run(
        "Store=input\nStore[\"a\"]+=begin;Store={a: 9};2;end\n[Store,input]",
        "{ a: int }",
        &input,
    )
    .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([{"a": 9}, {"a": 1}]));
    assert_eq!(json(&input), serde_json::json!({"a": 1}));
    // `&&=` and `||=` test their target, which must be a bool.
    for operator in ["&&=", "||="] {
        let source = format!(
            "def run(input: array<int>) -> array<array<int>>\nMath=input\n\
             Math.push(begin;Math {operator} [9];2;end)\n[Math,input]\nend"
        );
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
        assert_eq!(
            common::codes(&error),
            ["V0102", "V0102", "V0104"],
            "{operator}"
        );
        assert_eq!(
            error.diagnostics()[1].span.start,
            source.find(&format!("Math {operator}")).unwrap(),
            "{operator}"
        );
    }
}

/// A capitalized name assigned in one function is a runtime global, but the
/// static checker reads it in another function as the builtin namespace of
/// that name, so such a program does not compile.
#[test]
fn namespace_global_writes_are_builtin_namespace_reads_to_the_checker() {
    let source = r#"
module Reader
 def self.change -> array<int>
  Math[0].push(2)
  Math[0].fill {|i| i+7}
 end
end
def failing
 Reader.change
 raise "stop"
end
def run(input: array<array<int>>) -> array<array<array<int>>>
 Math=input
 begin
  failing
 rescue
  saved=Math
 ensure
  Math[0].push(9)
 end
 [input,saved,Math]
end
def discard(input: array<array<int>>)
 run(input)
 nil
end
def read -> float
 Math.PI
end
"#;
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error)[..2], ["V0112", "V0112"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("Math[0].push(2)").unwrap()
    );
}

/// Annotations name types, never a runtime global's current binding.
#[test]
fn type_annotations_do_not_name_the_current_global_binding() {
    let source = r#"
class A
end
class B
end
def typed(value:Math)->Math
 value
end
def run(first: bool) -> array<bool>
 if first
  Math=A
 else
  Math=B
 end
 value=Math.new
 accepted=typed(value).class==Math
 rejected=begin
  typed(7)
  false
 rescue
  true
 end
 [accepted,rejected]
end
"#;
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error)[0], "V0116");
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("Math)->").unwrap()
    );
}
