mod common;

use vibescript::{CallOptions, Engine, Limits, Value};

#[test]
fn full_unicode_modes_follow_independently_known_mappings() {
    for (method, source, expected) in [
        ("upcase", "Straße ﬁle", "STRASSE FILE"),
        ("downcase", "İ ΟΔΟΣ", "i\u{307} οδοσ"),
        ("capitalize", "ǆENAN", "ǅenan"),
        ("capitalize", "ßHELLO", "Sshello"),
        ("capitalize", "界ABC", "界abc"),
        ("swapcase", "ⒶⓐⅠⅰǅStraße", "ⓐⒶⅰⅠǆsTRASSE"),
        ("downcase(:fold)", "Straße Σςσ Kſ", "strasse σσσ ks"),
        ("upcase(:ascii)", "Straße Σς", "STRAßE Σς"),
        ("capitalize(:ascii)", "éABC", "éabc"),
        ("upcase", "ΐ", "Ι\u{308}\u{301}"),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: string) -> string\ninput.{method}\nend"
            ))
            .unwrap();
        let input = Value::bytes(source);
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(
            result.value.as_bytes(),
            Some(expected.as_bytes()),
            "{method}: {source}"
        );
        assert_eq!(input.as_bytes(), Some(source.as_bytes()));
    }
}

#[test]
fn invalid_bytes_select_ascii_mapping_for_the_entire_receiver() {
    let script = Engine::new().compile("def run(input: string) -> array<string>\n[input.upcase,input.downcase,input.capitalize,input.swapcase,input.downcase(:fold),input]\nend").unwrap();
    let source = b"\xc3\x89A\xffz";
    let result = script
        .call("run", &[Value::bytes(source)], CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    for (value, expected) in values.iter().zip([
        b"\xc3\x89A\xffZ".as_slice(),
        b"\xc3\x89a\xffz",
        b"\xc3\x89a\xffz",
        b"\xc3\x89a\xffZ",
        b"\xc3\x89a\xffz",
        source,
    ]) {
        assert_eq!(value.as_bytes(), Some(expected));
    }
}

#[test]
fn bang_methods_leave_receivers_and_aliases_unchanged() {
    let script = Engine::new().compile("def run(input: string) -> array<string?>\na=[input];h={key:input};v=input.upcase!;[input,a[0],h[\"key\"],v,v&.upcase!]\nend").unwrap();
    let input = Value::bytes("Straße");
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    for value in &values[..3] {
        assert_eq!(value.as_bytes(), Some("Straße".as_bytes()));
    }
    assert_eq!(values[3].as_bytes(), Some(b"STRASSE".as_slice()));
    assert_eq!(values[4].type_name(), "nil");
    assert_eq!(input.as_bytes(), Some("Straße".as_bytes()));
}

#[test]
fn comparisons_use_ascii_ordering_and_unicode_simple_folding() {
    for (method, left, right, expected) in [
        ("casecmp", "[", "A", Value::int(-1)),
        ("casecmp", "é", "É", Value::int(1)),
        ("casecmp?", "Σ", "ς", Value::boolean(true)),
        ("casecmp?", "kſ", "KS", Value::boolean(true)),
        ("casecmp?", "Straße", "STRASSE", Value::boolean(false)),
        ("casecmp?", "İ", "i", Value::boolean(false)),
        ("casecmp?", "ı", "I", Value::boolean(false)),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def run(a: string,b: string) -> {}\na.{method}(b)\nend",
                if method == "casecmp" { "int" } else { "bool" }
            ))
            .unwrap();
        let result = script
            .call(
                "run",
                &[Value::bytes(left), Value::bytes(right)],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(
            result.value.to_string(),
            expected.to_string(),
            "{method}: {left} {right}"
        );
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
    let script = Engine::new()
        .compile("def run(a: string,b: string) -> bool\na.casecmp?(b)\nend")
        .unwrap();
    let left = "ſΣK".repeat(32768);
    let right = "sςk".repeat(32768);
    let capacity = left.capacity() + right.capacity();
    let result = script
        .call(
            "run",
            &[Value::bytes(left), Value::bytes(right)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.type_name(), "bool");
    assert_eq!(result.value.to_string(), "true");
    assert_eq!(result.stats.retained_memory_bytes, 0);
    assert!(result.stats.peak_memory_bytes < capacity + 12000);
}

#[test]
fn output_storage_has_an_independent_lifetime_and_repeated_transforms_release_old_values() {
    let script = Engine::new()
        .compile("def run(input: string) -> string\noutput=input.upcase;input=\"\";output\nend")
        .unwrap();
    let text = "ΐ".repeat(8192);
    let input = Value::bytes(text.clone());
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let expected = "Ι\u{308}\u{301}".repeat(8192);
    assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
    assert!(result.stats.retained_memory_bytes >= expected.len());
    assert!(result.stats.retained_memory_bytes < expected.len() + 256);
    assert!(result.stats.peak_memory_bytes < text.len() + expected.len() + 12000);
    assert_eq!(input.as_bytes(), Some(text.as_bytes()));
    let import = Engine::new()
        .compile("def run(input: string) -> string\ninput\nend")
        .unwrap();
    let imported = import
        .call(
            "run",
            std::slice::from_ref(&result.value),
            CallOptions::default(),
        )
        .unwrap();
    assert!(imported.stats.retained_memory_bytes >= expected.len());
    drop(result);
    assert_eq!(imported.value.as_bytes(), Some(expected.as_bytes()));
    let repeat = Engine::new()
        .compile("def run(input: string) -> string\nfor i in 1..128\ninput=input.swapcase\nend\ninput\nend")
        .unwrap();
    let result = repeat
        .call(
            "run",
            &[Value::bytes("aB".repeat(4096))],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(65536),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some("aB".repeat(4096).as_bytes()));
    assert!(result.stats.retained_memory_bytes < 10000);
}

#[test]
fn keywords_blocks_and_bad_modes_are_refused_before_host_effects() {
    let mut engine = vibescript::Engine::new();
    engine.register("touch", |_, _| panic!("touch ran"));
    for method in [
        "upcase",
        "downcase",
        "capitalize",
        "swapcase",
        "upcase!",
        "downcase!",
        "capitalize!",
        "swapcase!",
    ] {
        for (call, code) in [
            ("(ignored:touch())", "V0302"),
            ("{touch()}", "V0305"),
            ("(nil)", "V0101"),
            ("(\"ascii\")", "V0101"),
            ("(:bad)", "V0101"),
            ("(:ascii,:ascii)", "V0301"),
        ] {
            let source = format!("def run(input: string)\ninput.{method}{call};touch()\nend");
            let error = engine.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), [code], "{source}");
        }
    }
}
