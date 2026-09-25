//! Every type an annotation can name is a type literal where a call takes a
//! type: the argument of `value.as(T)` and of `JSON.parse_as(text, T)`.

use vibescript::{CallOptions, Engine, ErrorKind, Value};

const DECLARATIONS: &str = "enum Status\n  Draft\nend\nclass Box\nend\ntype Pair = [int, string]\ndef identity(value: any) -> any\n  value\nend\n";

fn run(body: &str) -> vibescript::Result<Value> {
    let source = format!("{DECLARATIONS}def run -> any\n  {body}\nend\n");
    let script = Engine::new().compile(&source)?;
    Ok(script.call("run", &[], CallOptions::default())?.value)
}

#[test]
fn every_annotation_type_casts_a_value_of_that_type() {
    for (value, ty) in [
        ("1", "any"),
        ("1", "int"),
        ("1.5", "float"),
        ("1", "number"),
        ("\"a\"", "string"),
        (":a", "symbol"),
        ("true", "bool"),
        ("nil", "nil"),
        ("5.seconds", "duration"),
        ("Time.now", "time"),
        ("money_cents(100, \"USD\")", "money"),
        ("(1..2)", "range"),
        ("/a/", "regex"),
        ("\"ab\".match(/a/)", "match_data"),
        ("identity(int)", "type<int>"),
        ("[1]", "array<int>"),
        ("{ a: 1 }", "hash<string, int>"),
        ("1", "comparable"),
        ("nil", "int?"),
        ("\"a\"", "int | string"),
        ("{ a: 1 }", "{ a: int }"),
        ("[1, \"a\"]", "[int, string]"),
        ("Status::Draft", "Status"),
        ("Box.new", "Box"),
        ("[1, \"a\"]", "Pair"),
    ] {
        let body = format!("value = {value}\n  value.as({ty}) == value");
        let result = run(&body).unwrap_or_else(|error| panic!("{ty}: {error}"));
        assert_eq!(result.to_string(), "true", "{ty}");
    }
    let error =
        run("begin\n    raise \"boom\"\n  rescue => e\n    e.as(error).message\n  end").unwrap();
    assert_eq!(error.as_bytes(), Some(b"boom".as_slice()));
}

#[test]
fn a_cast_to_another_type_fails() {
    for (value, ty) in [
        ("1", "match_data"),
        ("\"a\"", "regex"),
        ("1", "error"),
        ("1", "money"),
        ("[1, 2]", "[int, string]"),
        ("1", "nil"),
        ("1", "type<int>"),
    ] {
        let error = run(&format!("{value}.as({ty})")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{ty}: {error}");
    }
}

#[test]
fn json_parse_as_takes_every_annotation_type() {
    let pair = run("JSON.parse_as(\"[1, \\\"a\\\"]\", [int, string])").unwrap();
    assert_eq!(pair.to_string(), "[1, a]");
    let error = run("JSON.parse_as(\"1\", money)").unwrap_err();
    assert!(
        error
            .message
            .contains("JSON.parse_as value expected money, got int"),
        "{error}"
    );
    let error = run("JSON.parse_as(\"1\", match_data)").unwrap_err();
    assert!(error.message.contains("expected match_data"), "{error}");
}

#[test]
fn locals_named_like_types_stay_values() {
    let rescued =
        run("begin\n    raise \"boom\"\n  rescue => error\n    identity(error).message\n  end")
            .unwrap();
    assert_eq!(rescued.as_bytes(), Some(b"boom".as_slice()));
    let local = run("money = 4\n  identity(money)").unwrap();
    assert_eq!(local.as_int(), Some(4));
    let array = run("a = 1\n  b = 2\n  identity([a, b])").unwrap();
    assert_eq!(array.to_string(), "[1, 2]");
    // Outside a call that takes types, brackets make an array, whose type
    // names are not values, and a builtin function is not a value.
    let error = run("identity([int, string])").unwrap_err();
    assert!(error.message.contains("undefined variable int"), "{error}");
    let error = run("identity(money)").unwrap_err();
    assert!(
        error
            .message
            .contains("money is a method and cannot be used as a value"),
        "{error}"
    );
}
