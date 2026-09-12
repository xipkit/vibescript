use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, parse_json, stringify_json};

fn run(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}
fn json(value: &Value) -> String {
    String::from_utf8(
        stringify_json(value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes()
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn functions_loops_and_control_flow() {
    let source = "def fib(n)\n if n < 2\n return n\n end\n fib(n - 1) + fib(n - 2)\nend\ni = 0\ns = 0\nwhile i < 10\n i += 1\n if i == 3\n next\n end\n if i == 8\n break\n end\n s += i\nend\n[s, fib(10)]";
    assert_eq!(json(&run(source)), "[25,55]");
}

#[test]
fn precedence_truthiness_and_floor_arithmetic() {
    assert_eq!(
        json(&run(
            "[2 + 3 * 4, -2 ** 2, 2 ** 3 ** 2, -7 / 3, 7 / -3, -7 % 3, 7 % -3, nil || 9, 0 && 4, false && (1 / 0)]"
        )),
        "[14,-4,512,-3,-3,2,-2,9,4,false]"
    );
}

#[test]
fn collections_have_value_semantics() {
    assert_eq!(
        json(&run(
            "a = [1, 2]\nb = a\na.push(3)\na[-1] = 9\na << 4\nh = {name: a, other: b}\nx = h\nh[:name] = 7\n[a,b,h,x]"
        )),
        "[[1,2,9,4],[1,2],{\"name\":7,\"other\":[1,2]},{\"name\":[1,2,9,4],\"other\":[1,2]}]"
    );
    assert_eq!(json(&run("a = [1,2]\na[-1] = 7\na")), "[1,7]");
    assert_eq!(
        json(&run(
            "[{a:1,b:2} == {b:2,a:1}, :a == \"a\", {a:1,a:2}.keys]"
        )),
        "[true,false,[\"a\"]]"
    );
}

#[test]
fn array_updates_preserve_aliases_and_evaluate_arguments_before_writing() {
    for (source, expected) in [
        ("a=[1]\nb=a\na.push(a)\na[0]=9\n[a,b]", "[[9,[1]],[1]]"),
        ("a=[1]\na << a\na", "[1,[1]]"),
        ("a=[1]\na[0]=a\na", "[[1]]"),
        ("a=[1]\na.push(a.length,a[0])\na", "[1,1,1]"),
        ("a=[1]\nb=[a]\na.push(2)\n[b,a]", "[[[1]],[1,2]]"),
        ("a=[1]\nb=a.push(2)\na[0]=9\n[a,b]", "[[9,2],[1,2]]"),
        ("a=[1]\nb=a\na=a+[a.length]\na+=[3]\n[a,b]", "[[1,1,3],[1]]"),
        ("a=[1]\na=a+a\na", "[1,1]"),
        ("a=[1]\nb=a\na=(false || a)+[a[0]]\n[a,b]", "[[1,1],[1]]"),
    ] {
        assert_eq!(json(&run(source)), expected, "{source}");
    }
}

#[test]
fn strings_preserve_bytes_and_count_runes() {
    assert_eq!(
        json(&run(
            "s = \"aé界🙂\\xff\"\n[s.length, s.bytesize, s[1], s[-1], s.upcase(:ascii), s.index(\"界\"), s.rindex(\"é\")]"
        )),
        "[5,11,\"é\",\"\\ufffd\",\"Aé界🙂\\ufffd\",2,1]"
    );
    assert_eq!(
        run("\"a\\xff\".upcase(:ascii)").as_bytes(),
        Some(b"A\xff".as_slice())
    );
    assert_eq!(
        json(&run(
            "[\"  hi  \".strip, \" a  b \".split, \"a,b,,\".split(\",\"), [1,2,3].join(\"-\"), \" -42 \".to_i]"
        )),
        "[\"hi\",[\"a\",\"b\"],[\"a\",\"b\"],\"1-2-3\",-42]"
    );
}

#[test]
fn json_escapes_duplicates_and_depth() {
    let v = parse_json(
        br#"{"x":1,"y":[true,null,"\ud83d\ude42\n"],"x":2}"#,
        CallOptions::default(),
    )
    .unwrap()
    .value;
    assert_eq!(json(&v), "{\"x\":2,\"y\":[true,null,\"🙂\\n\"]}");
    for raw in [
        "[1,]",
        "{\"a\":}",
        "01",
        "1.",
        "1e+",
        "true false",
        "\"\n\"",
        "\"\\q\"",
        "1e9999",
    ] {
        assert!(
            parse_json(raw.as_bytes(), CallOptions::default()).is_err(),
            "{raw}"
        );
    }
    assert_eq!(
        json(&run("JSON.parse(JSON.stringify({text: \"<>&\", n: 7}))")),
        "{\"text\":\"\\u003c\\u003e\\u0026\",\"n\":7}"
    );
}

#[test]
fn host_calls_are_isolated_and_exhaustion_is_latched() {
    let mut engine = Engine::new();
    engine.register("double", |ctx, args| {
        ctx.charge(2)?;
        Ok(Value::int(args[0].as_int().unwrap() * 2))
    });
    let script = engine
        .compile("def run(a)\n a.push(double(21))\n a\nend")
        .unwrap();
    let input = Value::array(vec![Value::int(1)]);
    assert_eq!(
        json(
            &script
                .call("run", std::slice::from_ref(&input), CallOptions::default())
                .unwrap()
                .value
        ),
        "[1,42]"
    );
    assert_eq!(json(&input), "[1]");
    engine.register("ignore", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::int(42))
    });
    assert_eq!(
        engine
            .compile("ignore()")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    engine.register("ignore_memory", |ctx, _| {
        let _ = ctx.bytes(&[1; 4096]);
        Ok(Value::int(42))
    });
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(2048),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        engine
            .compile("ignore_memory()")
            .unwrap()
            .run(options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn syntax_guards_and_overflow_are_errors() {
    for source in [
        "class X\nend",
        "def x(a, a)\n1\nend",
        "1e+",
        "1__2",
        "\"unterminated",
        "break",
        "a = 1\na[0][0] = 2",
    ] {
        assert!(Engine::new().compile(source).is_err(), "{source}");
    }
    let nested = format!("{}1{}", "(".repeat(300), ")".repeat(300));
    assert!(Engine::new().compile(&nested).is_err());
    for source in ["9223372036854775807 + 1", "1 / 0", "2 ** 64"] {
        assert_eq!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Arithmetic
        );
    }
}
