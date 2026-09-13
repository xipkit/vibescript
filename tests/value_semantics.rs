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
        let body = case["body"].as_str().unwrap();
        let name = case["name"].as_str().unwrap();
        assert_eq!(evaluate(body), case["expected"], "{name}");
        let first_end = body.find([';', '\n']).unwrap();
        let root = body.split_once('=').unwrap().0;
        let with_alias = format!(
            "{};unused_snapshot={root};{}",
            &body[..first_end],
            &body[first_end + 1..]
        );
        assert_eq!(evaluate(&with_alias), case["expected"], "{name} with alias");
    }
}

#[test]
fn passing_and_evaluating_collections_cannot_change_earlier_values() {
    for body in [
        "a=[1];x=a+a.push(2);[x,a]",
        "a=[1];left=a;x=left+a.push(2);[x,a]",
        "def combine(left,right)\nleft+right\nend\na=[1];x=combine(a,a.push(2));[x,a]",
    ] {
        assert_eq!(evaluate(body), serde_json::json!([[1, 1, 2], [1, 2]]));
    }
    for alias in ["", "copy=later;"] {
        let body = format!(
            "h={{a:1}};later={{a:3,b:4}};{alias}r=h.merge({{a:2}},later){{|k,o,n|later.a=9;o+n}};[later,r]"
        );
        assert_eq!(
            evaluate(&body),
            serde_json::json!([{"a":9,"b":4},{"a":6,"b":4}])
        );
    }
}
