mod common;

use vibescript::{CallOptions, Engine, stringify_json};

fn evaluate(body: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(body)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn documented_values_remain_stable_with_and_without_an_unused_alias() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../docs/compatibility-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        if case["policy"] != "documented_value_semantics" {
            continue;
        }
        let source = case["source"].as_str().unwrap();
        let with_alias = match case["source_with_alias"].as_str() {
            Some(alias) => alias.to_owned(),
            None => with_unused_alias(source),
        };
        for source in [source, &with_alias] {
            let name = case["name"].as_str().unwrap();
            let result = common::fixture_engine(case.get("static_error"), source, name)
                .compile(source)
                .unwrap()
                .call("run", &[vibescript::Value::nil()], CallOptions::default())
                .unwrap();
            let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
            let actual: serde_json::Value =
                serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
            assert_eq!(actual, case["expected"], "{}", case["name"]);
        }
    }
}

/// Aliases the local that the first statement of `run`'s body assigns, right
/// after that statement, leaving the alias unused.
fn with_unused_alias(source: &str) -> String {
    let (header, body) = source.split_once('\n').unwrap();
    let first_end = body.find([';', '\n']).unwrap();
    let target = body.split_once('=').unwrap().0;
    let root = target.split(':').next().unwrap().trim();
    format!(
        "{header}\n{};unused_snapshot={root};{}",
        &body[..first_end],
        &body[first_end + 1..]
    )
}

#[test]
fn passing_and_evaluating_collections_cannot_change_earlier_values() {
    for body in [
        "a=[1];x=a+a.push(2);[x,a]",
        "a=[1];left=a;x=left+a.push(2);[x,a]",
        "def combine(left: array<int>,right: array<int>) -> array<int>\nleft+right\nend\na=[1];x=combine(a,a.push(2));[x,a]",
    ] {
        assert_eq!(evaluate(body), serde_json::json!([[1, 1, 2], [1, 2]]));
    }
    for alias in ["", "copy=later;"] {
        let body = format!(
            "h={{a:1}};later={{a:3,b:4}};{alias}r=h.merge({{a:2}},later){{|k,o,n|later[\"a\"]=9;o+n}};[later,r]"
        );
        assert_eq!(
            evaluate(&body),
            serde_json::json!([{"a":9,"b":4},{"a":6,"b":4}])
        );
    }
}

#[test]
fn field_writes_leave_earlier_snapshots_unchanged() {
    // Go v0.70.0 writes through the stored collection without isolating it, so
    // there the snapshots taken before the writes change too.
    let body = "class Holder
  @h: { a: int }
  @rows: [int, array<int>]
 def initialize
  @rows=[1,[2]]
  @h={a:1}
 end
 def poke -> array<any>
  saved=[@rows,@rows[1],@h]
  @rows[0]=9
  @rows[1][0]=8
  @h[\"a\"]+=1
  [saved,@rows,@h]
 end
end
Holder.new.poke";
    assert_eq!(
        evaluate(body),
        serde_json::json!([[[1, [2]], [2], {"a": 1}], [9, [8]], {"a": 2}])
    );
}

#[test]
fn bare_field_names_are_not_in_scope() {
    // A field is written through its `@` name; a bare `rows` names nothing.
    let body = "class Holder
  @rows: array<int>
 def initialize
  @rows=[1]
 end
 def poke
  rows[0]=9
 end
end";
    let error = common::static_engine().compile(body).err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        body.find("rows[0]").unwrap()
    );
}
