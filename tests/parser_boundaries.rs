use vibescript::{CallOptions, Engine, ErrorKind};

#[test]
fn standalone_begin_rejects_statement_modifiers() {
    for body in [
        "begin;1;end",
        "begin;raise 'x';rescue;1;end",
        "begin;1;ensure;2;end",
    ] {
        for modifier in ["if true", "unless false", "while false", "until true"] {
            let source = format!("{body} {modifier}");
            let error = Engine::new().compile(&source).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
            assert!(error.to_string().contains("modifier"), "{source}: {error}");
        }
    }
}

#[test]
fn begin_values_preserve_expression_modifiers() {
    for expression in [
        "(begin;i+=1;end)",
        "value=begin;i+=1;end",
        "begin;i+=1;end.to_s",
    ] {
        for (modifier, expected) in [
            ("if true", 6),
            ("unless false", 6),
            ("while i<3", 5),
            ("until i>3", 5),
        ] {
            let source = format!("i=5;{expression} {modifier};i");
            let result = Engine::new()
                .compile(&source)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .run(CallOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(result.value.as_int(), Some(expected), "{source}");
        }
    }
    for (source, expected) in [
        ("(begin;raise 'x';rescue;3;end) if true", 3),
        ("(begin;1;ensure;2;end) unless false", 1),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{source}");
    }
}
