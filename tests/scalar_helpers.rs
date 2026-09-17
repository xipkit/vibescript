use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, Value};

fn forms(receiver: &str, name: &str, arguments: &str) -> [String; 3] {
    let suffix = if arguments.is_empty() {
        String::new()
    } else {
        format!(",{arguments}")
    };
    [
        format!("({receiver}).{name}({arguments})"),
        format!("({receiver}).send(:{name}{suffix})"),
        format!("({receiver}).public_send(:send,:{name}{suffix})"),
    ]
}

#[test]
fn symbol_conversions_preserve_raw_bytes_and_distinct_value_kinds() {
    for bytes in [b"name".as_slice(), b"", b"raw\x00\xff\x80"] {
        for method in ["id2name", "to_s", "string", "to_sym"] {
            for call in forms("value", method, "") {
                let script = Engine::new()
                    .compile(&format!("def run(value:symbol); {call}; end"))
                    .unwrap();
                let result = script
                    .call("run", &[Value::symbol(bytes)], CallOptions::default())
                    .unwrap();
                assert_eq!(result.value.as_bytes(), Some(bytes), "{call}");
                assert_eq!(
                    result.value.type_name(),
                    if method == "to_sym" {
                        "symbol"
                    } else {
                        "string"
                    },
                    "{call}"
                );
            }
        }
    }
    let result = Engine::new()
        .compile(
            "def run; a=:name; b=a.id2name; b=b.concat(\"!\"); [a,b,a.to_sym==a,a.id2name==a]; end",
        )
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[name, name!, true, false]");
}

#[test]
fn scalar_conversions_reject_arguments_keywords_and_blocks_without_entry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for (receiver, methods) in [
        ("nil", &["to_s", "string"][..]),
        ("true", &["to_s", "string"][..]),
        ("1..3", &["to_s", "string"][..]),
        (":name", &["id2name", "to_s", "string", "to_sym"][..]),
    ] {
        for &method in methods {
            for arguments in ["7", "extra:7", ""] {
                for call in forms(receiver, method, arguments) {
                    let source = format!(
                        "def run; begin; {call} {{entered(); return 999}}; rescue RuntimeError; 7; end; end"
                    );
                    let result = engine
                        .compile(&source)
                        .unwrap()
                        .call("run", &[], CallOptions::default())
                        .unwrap();
                    assert_eq!(result.value.as_int(), Some(7), "{source}");
                }
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn scalar_aliases_work_as_reads_and_feed_introspection_without_detachment() {
    let source = "[:name.id2name,:name.to_sym,:name.respond_to?(:id2name),:name.respond_to?(:to_sym),:name.id2name.is_type?(:string),:name.to_sym.is_type?(:symbol),[1,:odd?].reduce(:respond_to?),[nil,:nil].reduce(:is_type?)]";
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        result.value.to_string(),
        "[name, name, true, true, true, true, true, true]"
    );
}
