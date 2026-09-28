mod common;

use vibescript::{CallOptions, Engine, Value};

#[test]
fn symbol_conversions_preserve_raw_bytes_and_distinct_value_kinds() {
    for bytes in [b"name".as_slice(), b"", b"raw\x00\xff\x80"] {
        for (method, result) in [("to_s", "string"), ("to_sym", "symbol")] {
            let source = format!("def run(value: symbol) -> {result}; value.{method}; end");
            let script = Engine::new().compile(&source).unwrap();
            let output = script
                .call("run", &[Value::symbol(bytes)], CallOptions::default())
                .unwrap();
            assert_eq!(output.value.as_bytes(), Some(bytes), "{source}");
            assert_eq!(output.value.type_name(), result, "{source}");
        }
    }
    let result = Engine::new()
        .compile(
            "def run -> array<bool | string | symbol>; a=:name; b=a.to_s; b=b.concat(\"!\"); [a,b,a.to_sym==a,a.to_s==a]; end",
        )
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[name, name!, true, false]");
}

#[test]
fn scalar_conversions_refuse_arguments_keywords_and_blocks_before_entry() {
    let mut engine = vibescript::Engine::new();
    engine.register("entered", |_, _| panic!("entered ran"));
    for (receiver, methods) in [
        ("nil", &["to_s"][..]),
        ("true", &["to_s"][..]),
        ("1..3", &["to_s"][..]),
        (":name", &["to_s", "to_sym"][..]),
    ] {
        for &method in methods {
            for (arguments, expected) in [
                ("(7)", &["V0301", "V0305"][..]),
                ("(extra:7)", &["V0302", "V0305"]),
                ("", &["V0305"]),
            ] {
                let source = format!("({receiver}).{method}{arguments} {{entered()}}");
                let error = engine.compile(&source).err().unwrap();
                assert_eq!(common::codes(&error), expected, "{source}");
            }
        }
    }
}

#[test]
fn scalar_aliases_and_dispatch_by_name_are_removed() {
    let source = "[:name.id2name,:name.to_sym,:name.respond_to?(:id2name),:name.respond_to?(:to_sym),:name.id2name.is_type?(:string),:name.to_sym.is_type?(:symbol),[1,:odd?].reduce(:respond_to?),[nil,:nil].reduce(:is_type?)]";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(
        common::codes(&error),
        ["V0401", "V0405", "V0405", "V0401", "V0401", "V0401"]
    );
    let names: Vec<&str> = error
        .diagnostics()
        .iter()
        .map(|d| &source[d.span.start..d.span.end])
        .collect();
    assert_eq!(
        names,
        [
            "id2name",
            "respond_to?",
            "respond_to?",
            "id2name",
            "reduce",
            "reduce"
        ]
    );
}
