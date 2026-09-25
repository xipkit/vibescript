mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn trimming_chomping_and_chopping_preserve_their_distinct_byte_rules() {
    for (method, raw, expected) in [
        (
            "strip",
            b"\0\t \xc2\xa0x\xc2\xa0 \r\0".as_slice(),
            b"\xc2\xa0x\xc2\xa0".as_slice(),
        ),
        ("squish", "\u{3000}a\u{a0}\t b\n".as_bytes(), b"a b"),
        ("squish", b" \0\x1c\xff\t x\xc2\xa0", b"\0\x1c\xff x"),
        ("chomp", b"a\r\n\r\n", b"a\r\n"),
        ("chomp(nil)", b"a\r\n", b"a\r\n"),
        ("chomp(\"\")", b"a\r\n\r\n\r", b"a"),
        ("chomp(\"\\n\")", b"a\r\n", b"a\r"),
        ("chop", b"a\r\n", b"a"),
        ("chop", "a🙂".as_bytes(), b"a"),
        ("chop", b"a\xf0\x9f\x99", b"a\xf0\x9f"),
        ("chop", b"a\xc3\xa9\x80", b"a\xc3\xa9"),
        ("chop", b"", b""),
        ("delete_prefix(\"\\xc3\")", "éx".as_bytes(), b"\xa9x"),
        ("delete_suffix(\"\\xa9\")", "xé".as_bytes(), b"x\xc3"),
        ("reverse!", b"a\xffb", b"b\xef\xbf\xbda"),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: string) -> string?\ninput.{method}\nend"
            ))
            .unwrap();
        let result = script
            .call("run", &[Value::bytes(raw)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected), "{method}");
    }
}

#[test]
fn bang_results_preserve_aliases_and_unchanged_calls_return_nil() {
    for (call, source, expected) in [
        ("strip!", " x ", "x"),
        ("lstrip!", " x ", "x "),
        ("rstrip!", " x ", " x"),
        ("squish!", " x \t y ", "x y"),
        ("chomp!", "x\r\n", "x"),
        ("chop!", "x界", "x"),
        ("delete_prefix!(\"x\")", "xy", "y"),
        ("delete_suffix!(\"y\")", "xy", "x"),
        ("reverse!", "x界", "界x"),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: string) -> array<string?>\na=[input];h={{k:input}};r=input.{call};[input,a[0],h[\"k\"],r]\nend"
            ))
            .unwrap();
        let input = Value::bytes(source);
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        for value in &values[..3] {
            assert_eq!(value.as_bytes(), Some(source.as_bytes()), "{call}");
        }
        assert_eq!(values[3].as_bytes(), Some(expected.as_bytes()), "{call}");
        assert_eq!(input.as_bytes(), Some(source.as_bytes()));
        let empty = script
            .call("run", &[Value::bytes("")], CallOptions::default())
            .unwrap();
        assert_eq!(empty.value.as_array().unwrap()[3].type_name(), "nil");
    }
}

#[test]
fn padding_counts_characters_and_partition_searches_raw_bytes() {
    for (call, source, expected) in [
        ("center(8,\"ab界\")", "é", "ab界éab界a"),
        ("ljust(5,\"🙂界\")", "é", "é🙂界🙂界"),
        ("rjust(4,\"ab\")", "é", "abaé"),
        ("center(-2)", "é", "é"),
    ] {
        let result = Engine::new()
            .compile(&format!(
                "def run(input: string) -> string\ninput.{call}\nend"
            ))
            .unwrap()
            .call("run", &[Value::bytes(source)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()), "{call}");
    }
    // A float width is refused before anything runs.
    let error = common::static_engine()
        .compile("def run(input: string) -> string\ninput.ljust(5.9,\"🙂界\")\nend")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    let script = Engine::new().compile("def run(input: string) -> array<string | array<string>>\n[input.center(6,\"\\xffé\"),input.partition(\"\\xa9\"),input.rpartition(\"\\xa9\")]\nend").unwrap();
    let result = script
        .call("run", &[Value::bytes("éé")], CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(
        values[0].as_bytes(),
        Some(b"\xff\xc3\xa9\xc3\xa9\xc3\xa9\xff\xc3\xa9".as_slice())
    );
    for (value, expected) in values[1..].iter().zip([
        [b"\xc3".as_slice(), b"\xa9", b"\xc3\xa9"],
        [b"\xc3\xa9\xc3".as_slice(), b"\xa9", b""],
    ]) {
        for (part, bytes) in value.as_array().unwrap().iter().zip(expected) {
            assert_eq!(part.as_bytes(), Some(bytes));
        }
    }
}

#[test]
fn small_results_do_not_retain_large_inputs_or_search_scratch() {
    for (call, input, expected) in [
        (
            "strip",
            format!("{}x{}", " ".repeat(65536), "\0".repeat(65536)),
            "x",
        ),
        ("lstrip", format!("{}x", " ".repeat(65536)), "x"),
        ("rstrip", format!("x{}", "\t".repeat(65536)), "x"),
        (
            "squish",
            format!("{}x{}", "\u{3000}".repeat(16384), "\n".repeat(65536)),
            "x",
        ),
        ("chomp(\"\")", format!("x{}", "\r\n".repeat(32768)), "x"),
        (
            "partition(\"x\")[1]",
            format!("{}x{}", "a".repeat(65536), "b".repeat(65536)),
            "x",
        ),
        (
            "rpartition(\"x\")[1]",
            format!("{}x{}", "a".repeat(65536), "b".repeat(65536)),
            "x",
        ),
        (
            "partition(\"a\"*8192)[0]",
            format!("x{}", "a".repeat(8192)),
            "x",
        ),
        (
            "delete_prefix(\"a\"*65536)",
            format!("{}x", "a".repeat(65536)),
            "x",
        ),
        (
            "delete_suffix(\"a\"*65536)",
            format!("x{}", "a".repeat(65536)),
            "x",
        ),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: string) -> string\noutput=input.{call};input=\"\";output\nend"
            ))
            .unwrap();
        let result = script
            .call("run", &[Value::bytes(input)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()), "{call}");
        assert!(
            result.stats.retained_memory_bytes < 256,
            "{call}: {:?}",
            result.stats
        );
        let imported = Engine::new()
            .compile("def run(input: string) -> string\ninput\nend")
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&result.value),
                CallOptions::default(),
            )
            .unwrap();
        assert!(imported.stats.retained_memory_bytes < 256);
        drop(result);
        assert_eq!(imported.value.as_bytes(), Some(expected.as_bytes()));
    }
    let script = Engine::new()
        .compile(
            "def run(input: string) -> string\nfor i in 1..512\ninput=input.chop\nend\ninput\nend",
        )
        .unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes("a".repeat(8192))],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(32768),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_bytes().unwrap().len(), 7680);
    assert!(result.stats.retained_memory_bytes < 8192);
}

#[test]
fn call_contracts_stop_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("touch", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(1))
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::nil())
    });
    for (call, kind) in [
        ("cancel().as(string?)", ErrorKind::Cancelled),
        ("exhaust().as(string?)", ErrorKind::Steps),
    ] {
        effects.store(0, Ordering::Relaxed);
        let script = engine
            .compile(&format!("\"x\".chomp({call});touch()"))
            .unwrap();
        assert_eq!(script.run(CallOptions::default()).unwrap_err().kind, kind);
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    effects.store(0, Ordering::Relaxed);
    let script = engine.compile("\"x\".center(0,\"\");touch()").unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Argument
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    // Unknown keywords, blocks and arguments of the wrong type or count
    // are refused before anything runs.
    let mut checked = common::static_engine();
    checked.register("touch", |_, _| panic!("touch ran"));
    let refused = |source: &str, expected: &[&str]| {
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), expected, "{source}");
    };
    for (method, args) in [
        ("strip", ""),
        ("lstrip!", ""),
        ("rstrip", ""),
        ("squish!", ""),
        ("chomp", ""),
        ("chop!", ""),
        ("delete_prefix!", "\"x\","),
        ("delete_suffix", "\"x\","),
        ("reverse!", ""),
    ] {
        refused(
            &format!("\" x x \".{method}({args}unused:touch()){{touch()}}"),
            &["V0302", "V0305"],
        );
    }
    for (method, args) in [
        ("center", "9"),
        ("ljust", "9"),
        ("rjust", "9"),
        ("partition", "\"x\""),
        ("rpartition", "\"x\""),
    ] {
        refused(
            &format!("\"x\".{method}({args},unused:touch()){{touch()}};touch()"),
            &["V0302", "V0305"],
        );
        refused(&format!("\"x\".{method}({args}){{touch()}}"), &["V0305"]);
    }
    for (call, code) in [
        ("strip(1)", "V0301"),
        ("partition(:x)", "V0101"),
        ("ljust(0,nil)", "V0101"),
        ("rjust(1e30)", "V0101"),
    ] {
        refused(&format!("\"x\".{call};touch()"), &[code]);
    }
}
