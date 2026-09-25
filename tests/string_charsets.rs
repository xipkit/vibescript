mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn ranges_intersections_escapes_and_translation_order_follow_the_language() {
    let result = Engine::new()
        .compile(
            r#"
      ["banana".count("an"), "hello".count("lo","o"), "abc^".count("^"),
       "banana".delete("an"), "abc123".delete("^0-9"), "abc-".delete("a\\-c"),
       "abc123".tr("^0-9","XYZ"), "a".tr("aa","XY"), "b".tr("a-cb","WXYZ"),
       "abc".tr("a-c","x"), "abc".tr("b",""), "bookkeeper".squeeze,
       "hello   world".squeeze(" "), "aaabbbccc".squeeze("a-c","^b"),
       "abc".tr("a-c","^ab"), "abc\\".count("\\")]
    "#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    for (value, expected) in values[..3].iter().zip([5, 1, 1]) {
        assert_eq!(value.as_int(), Some(expected));
    }
    for (value, expected) in values[3..15].iter().zip([
        "b",
        "123",
        "b",
        "ZZZ123",
        "Y",
        "Z",
        "xxx",
        "ac",
        "bokeper",
        "hello world",
        "abbbc",
        "^ab",
    ]) {
        assert_eq!(value.as_bytes(), Some(expected.as_bytes()));
    }
    assert_eq!(values[15].as_int(), Some(1));
}

#[test]
fn invalid_bytes_stay_distinct_from_valid_replacement_characters() {
    let input = Value::bytes(b"\xff\xff\xef\xbf\xbd\xef\xbf\xbd\xfe\xfe");
    let result = Engine::new()
        .compile(
            r#"def run(input: string) -> array<int | string>
      [input.count("\xff"),input.count("�"),input.delete("\xff"),
       input.squeeze,input.squeeze("�"),input.tr("\xff","é"),input.tr("�","\xff"),input]
      end"#,
        )
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(values[0].as_int(), Some(2));
    assert_eq!(values[1].as_int(), Some(2));
    for (value, expected) in values[2..].iter().zip([
        b"\xef\xbf\xbd\xef\xbf\xbd\xfe\xfe".as_slice(),
        b"\xff\xef\xbf\xbd\xfe",
        b"\xff\xff\xef\xbf\xbd\xfe\xfe",
        b"\xc3\xa9\xc3\xa9\xef\xbf\xbd\xef\xbf\xbd\xfe\xfe",
        b"\xff\xff\xff\xff\xfe\xfe",
        input.as_bytes().unwrap(),
    ]) {
        assert_eq!(value.as_bytes(), Some(expected));
    }
    let script = Engine::new()
        .compile("def run(s: string,a: string,b: string) -> string?\ns.tr!(a,b)\nend")
        .unwrap();
    let result = script
        .call(
            "run",
            &[
                Value::bytes("�"),
                Value::bytes("a�"),
                Value::bytes("\u{d7ff}-\u{e000}"),
            ],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.type_name(), "nil");
    let result = script
        .call(
            "run",
            &[
                Value::bytes("abc"),
                Value::bytes("a-c"),
                Value::bytes("\u{d7ff}-\u{e000}"),
            ],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some("\u{d7ff}��".as_bytes()));
}

#[test]
fn transforms_preserve_aliases_and_argument_side_effects() {
    for (method, args, expected) in [
        ("delete", "\"an\"", "b"),
        ("delete!", "\"an\"", "b"),
        ("tr", "\"an\",\"AN\"", "bANANA"),
        ("tr!", "\"an\",\"AN\"", "bANANA"),
        ("squeeze", "", "banana"),
        ("squeeze!", "", "nil"),
    ] {
        let call = if args.is_empty() {
            method.to_owned()
        } else {
            format!("{method}({args})")
        };
        let script = Engine::new().compile(&format!("def run(input: string) -> array<string?>\ns=input;h={{a:[s]}};r=h[\"a\"].fetch(0).{call};[input,s,h[\"a\"].fetch(0),r]\nend")).unwrap();
        let input = Value::bytes("banana");
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        for value in &values[..3] {
            assert_eq!(value.as_bytes(), input.as_bytes());
        }
        if expected == "nil" {
            assert_eq!(values[3].type_name(), "nil");
        } else {
            assert_eq!(values[3].as_bytes(), Some(expected.as_bytes()));
        }
    }
    for call in [
        "delete(a.shift.as(string))",
        "delete!(a.shift.as(string))",
        "tr(a.shift.as(string),\"\")",
    ] {
        let result = Engine::new()
            .compile(&format!("a=[\"aba\"];r=a.fetch(0).{call};[a,r]"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        assert!(values[0].as_array().unwrap().is_empty());
        assert_eq!(values[1].as_bytes(), Some(b"".as_slice()));
    }
}

#[test]
fn returned_storage_outlives_inputs_and_repeated_transforms_reclaim_old_values() {
    for (call, source, expected) in [
        (
            "delete(\"a\")",
            format!("{}x", "a".repeat(32768)),
            "x".to_owned(),
        ),
        (
            "squeeze",
            format!("{}x", "a".repeat(32768)),
            "ax".to_owned(),
        ),
        ("tr(\"a\",\"🙂\")", "a".repeat(8192), "🙂".repeat(8192)),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: string) -> string\noutput=input.{call};input=\"\";output\nend"
            ))
            .unwrap();
        let result = script
            .call("run", &[Value::bytes(source)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
        assert!(result.stats.retained_memory_bytes >= expected.len());
        assert!(result.stats.retained_memory_bytes < expected.len() + 256);
        let imported = Engine::new()
            .compile("def run(input: string) -> string\ninput\nend")
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&result.value),
                CallOptions::default(),
            )
            .unwrap();
        assert!(imported.stats.retained_memory_bytes < expected.len() + 256);
        drop(result);
        assert_eq!(imported.value.as_bytes(), Some(expected.as_bytes()));
    }
    let script = Engine::new()
        .compile("def run(input: string) -> string\nfor i in 1..32\ninput=input.tr(\"aA\",\"Aa\")\nend\ninput\nend")
        .unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes("a".repeat(1024))],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(16384),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some("a".repeat(1024).as_bytes()));
    assert!(result.stats.retained_memory_bytes < 1280);
}

#[test]
fn large_sets_consume_work_and_bad_calls_stop_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("touch", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(1))
    });
    // Blocks and unknown keywords are refused before anything runs.
    let mut checked = common::static_engine();
    checked.register("touch", |_, _| panic!("touch ran"));
    for (method, args) in [
        ("count", "\"a\""),
        ("delete", "\"a\""),
        ("delete!", "\"a\""),
        ("tr", "\"a\",\"b\""),
        ("tr!", "\"a\",\"b\""),
        ("squeeze", ""),
        ("squeeze!", ""),
    ] {
        let call = |suffix: &str| match args {
            "" => format!("\"aa\".{method}{suffix}"),
            _ => format!("\"aa\".{method}({args}){suffix}"),
        };
        let keyword = match args {
            "" => format!("\"aa\".{method}(ignored:touch())"),
            _ => format!("\"aa\".{method}({args},ignored:touch())"),
        };
        for (source, code) in [(call("{touch()}"), "V0305"), (keyword, "V0302")] {
            let source = format!("{source};touch()");
            let error = checked.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), [code], "{source}");
        }
    }
    for call in ["delete(nil)", "squeeze(:a)"] {
        let error = checked
            .compile(&format!("\"\".{call};touch()"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), ["V0101"], "{call}");
    }
    for call in ["count(\"z-a\")", "tr(\"a\",\"z-a\")"] {
        effects.store(0, Ordering::Relaxed);
        let script = engine.compile(&format!("\"\".{call};touch()")).unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    let large: String = (0..1024)
        .map(|i| char::from_u32(0x3000 + i).unwrap())
        .collect();
    for call in ["count(set)", "delete(set)", "tr(set,\"x\")", "squeeze(set)"] {
        let script = engine
            .compile(&format!(
                "def run(input: string,set: string)\ninput.{call};touch()\nend"
            ))
            .unwrap();
        effects.store(0, Ordering::Relaxed);
        let options = CallOptions {
            limits: Limits {
                steps: Some(16384),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        script
            .call(
                "run",
                &[Value::bytes("z".repeat(4096)), Value::bytes("q")],
                options.clone(),
            )
            .unwrap();
        assert_eq!(effects.load(Ordering::Relaxed), 1);
        effects.store(0, Ordering::Relaxed);
        let error = script
            .call(
                "run",
                &[Value::bytes("z".repeat(4096)), Value::bytes(large.clone())],
                options,
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{call}");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
}
