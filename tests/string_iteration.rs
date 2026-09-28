mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn string_iterators_distinguish_raw_bytes_runes_and_lines() {
    let script = Engine::new().compile("def run(text: string) -> array<array<int | array<int>>>\nchars: array<array<int>> =[];bytes: array<int> =[];points: array<int> =[];lines: array<array<int>> =[]\ntext.each_char{|c:string|chars.push(c.bytes)}\ntext.each_byte{|b:int|bytes.push(b)}\ntext.each_codepoint{|c:int|points.push(c)}\ntext.each_line{|line:string|lines.push(line.bytes)}\n[chars,bytes,points,lines,text.lines.map{|line|line.bytes}]\nend").unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(b"A\xc3\xa9\xff\n\0b")],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            [[65], [195, 169], [239, 191, 189], [10], [0], [98]],
            [65, 195, 169, 255, 10, 0, 98],
            [65, 233, 65533, 10, 0, 98],
            [[65, 195, 169, 255, 10], [0, 98]],
            [[65, 195, 169, 255, 10], [0, 98]]
        ])
    );
}

#[test]
fn iteration_returns_the_original_receiver_and_preserves_bindings() {
    for method in ["each_char", "each_byte", "each_codepoint", "each_line"] {
        let script = Engine::new().compile(&format!("text=\"a\\nb\";alias=text;count=0;result=text.{method}{{|value|text=\"changed\";count+=1;next}};[text,alias,result,count]")).unwrap();
        let output = script.run(CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([
                "changed",
                "a\nb",
                "a\nb",
                if method == "each_line" { 2 } else { 3 }
            ])
        );
    }
}

#[test]
fn missing_blocks_and_argument_errors_are_refused_before_callback_effects() {
    let mut engine = vibescript::Engine::new();
    engine.register("effect", |_, _| panic!("effect ran"));
    let refused = |source: &str, code: &str| {
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    };
    for method in ["each_char", "each_byte", "each_codepoint", "each_line"] {
        refused(&format!("\"\".{method}"), "V0304");
        refused(&format!("\"a\".{method}(1){{effect()}}"), "V0301");
        refused(&format!("\"a\".{method}(chomp:true){{effect()}}"), "V0302");
    }
    for source in [
        "\"a\".each_char{|x:int|effect()}",
        "\"a\".each_byte{|x:string|effect()}",
        "\"a\".each_codepoint{|x:string|effect()}",
        "\"a\".each_line{|x:int|effect()}",
    ] {
        refused(source, "V0101");
    }
    for method in ["lines", "bytes", "chars", "codepoints"] {
        refused(&format!("\"a\\nb\".{method}{{effect()}}"), "V0305");
    }
    refused("\"x\".each_char(effect()){effect()}", "V0301");
}

#[test]
fn early_exits_skip_the_remaining_input_and_retire_pending_calls() {
    let mut input = vec![b'x'; 1 << 20];
    input[0] = b'A';
    input[1] = b'\n';
    let input = Value::bytes(input);
    for (method, expected) in [
        ("each_byte", serde_json::json!(65)),
        ("each_codepoint", serde_json::json!(65)),
        ("each_char", serde_json::json!("A")),
        ("each_line", serde_json::json!("A\n")),
    ] {
        let script = Engine::new().compile(&format!("def sink(a: string,b: string) -> int\n9\nend\ndef run(text: string) -> int | string\nsink(text, text.{method}{{|value|return value}})\nend")).unwrap();
        let output = script
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        steps: Some(1000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(json(&output.value), expected);
        assert!(output.stats.retained_memory_bytes < 1024);
    }
    let script = Engine::new()
        .compile("def run(text: string) -> string\ntext.each_line{|line|break line}\nend")
        .unwrap();
    let output = script
        .call("run", &[input], CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"A\n");
    assert!(output.stats.retained_memory_bytes < 1024);
}

#[test]
fn detached_lines_do_not_retain_a_large_subject() {
    let mut input = vec![b'x'; 1 << 20];
    input[1] = b'\n';
    let input = Value::bytes(input);
    for body in ["text.lines.fetch(0)", "text.each_line{|line|return line}"] {
        let script = Engine::new()
            .compile(&format!("def run(text: string) -> string\n{body}\nend"))
            .unwrap();
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes().unwrap(), b"x\n");
        assert_ne!(
            output.value.as_bytes().unwrap().as_ptr(),
            input.as_bytes().unwrap().as_ptr()
        );
        assert!(output.stats.retained_memory_bytes < 1024);
    }
}

#[test]
fn streaming_keeps_one_unretained_line_and_accounts_for_retained_lines() {
    let width = 65537;
    let mut bytes = vec![b'a'; 2 * width];
    bytes[width - 1] = b'\n';
    bytes[2 * width - 1] = b'\n';
    let input = Value::bytes(bytes);
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(3 * width + 16384),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("def run(text: string)\ntext.each_line{|line|line};nil\nend")
        .unwrap();
    let output = script
        .call("run", std::slice::from_ref(&input), options.clone())
        .unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert!(output.stats.peak_memory_bytes < 3 * width + 16384);

    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("seen", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("def run(text: string) -> array<string>\nkept: array<string> =[];text.each_line{|line|seen();kept.push(line)};kept\nend")
        .unwrap();
    assert_eq!(
        script.call("run", &[input], options).unwrap_err().kind,
        ErrorKind::Memory
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn callback_cancellation_and_recursion_stop_before_later_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for method in ["each_char", "each_byte", "each_codepoint", "each_line"] {
        let source = format!("\"a\\nb\".{method}{{cancel();effect()}};effect()");
        assert_eq!(
            engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
        let source =
            format!("def recurse -> string\n\"x\".{method}{{recurse}}\nend\nrecurse;effect()");
        let options = CallOptions {
            limits: Limits {
                recursion: 16,
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            engine
                .compile(&source)
                .unwrap()
                .run(options)
                .unwrap_err()
                .kind,
            ErrorKind::Recursion
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn long_scans_and_empty_inputs_observe_work_and_deadline_limits() {
    let script = Engine::new()
        .compile("def run(text: string) -> string\ntext.each_line{}\nend")
        .unwrap();
    let error = script
        .call(
            "run",
            &[Value::bytes(vec![b'a'; 1 << 20])],
            CallOptions {
                limits: Limits {
                    steps: Some(1000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    for body in [
        "\"\".lines",
        "\"\".each_byte{}",
        "\"\".each_char{}",
        "\"\".each_codepoint{}",
        "\"\".each_line{}",
    ] {
        let script = Engine::new().compile(body).unwrap();
        let options = CallOptions {
            deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..CallOptions::default()
        };
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Deadline);
    }
}
