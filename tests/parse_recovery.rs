use vibescript::{
    Engine, Error, ErrorKind,
    diagnostic::{Area, Span},
};

fn errors(source: &str, lines: &[usize]) -> Error {
    let error = Engine::new().compile(source).err().expect("syntax fails");
    assert_eq!(error.kind, ErrorKind::Syntax, "{source}: {error}");
    let diagnostics = error.diagnostics();
    assert!(
        diagnostics
            .iter()
            .all(|d| d.code.area() == Some(Area::Syntax))
    );
    assert_eq!(
        diagnostics
            .iter()
            .map(|d| d.span.position(source).line)
            .collect::<Vec<_>>(),
        lines,
        "{source}: {diagnostics:?}"
    );
    error
}

#[test]
fn resumes_at_newlines_and_semicolons() {
    errors("x = )\ny = ]\nz = }\n", &[1, 2, 3]);
    errors("x = ); y = ]; z = }", &[1, 1, 1]);
    errors("x = )\r\ny = ]\r\nz = }\r\n", &[1, 2, 3]);
}

#[test]
fn resumes_inside_functions_and_at_declarations() {
    errors(
        "def one\nx = )\ny = ]\nend\ndef two\nz = }\nend",
        &[2, 3, 6],
    );
    errors(
        "def bad(,\nend\ndef next(,\nend\nclass\nend\nmodule Wrong\n def bad(,\n end\nend\nenum\nend\nx = )",
        &[1, 3, 6, 8, 12, 13],
    );
    errors(
        "x = ) def bad(,\nend\nclass Wrong\nx = ]\nend\nmodule Scope\nx = }\nend\nenum\nend",
        &[1, 1, 4, 7, 10],
    );
}

#[test]
fn preserves_class_and_module_member_boundaries() {
    errors(
        "class A\nproperty :bad\ndef bad(,\nend\ndef good\nx = )\ny = ]\nend\nend\nmodule M\ndef self.one\nx = )\ny = ]\nend\nend",
        &[2, 3, 6, 7, 12, 13],
    );
}

#[test]
fn resumes_at_end_braces_and_branch_boundaries() {
    errors("if true\nx=)\nelse\ny=]\nend\nz=}\n", &[2, 4, 6]);
    errors("[1].each { x=)\ny=]\n}\nz=}\n", &[1, 2, 4]);
    errors("def one; x=) end\ndef two; y=] end\n", &[1, 2]);
    errors("[1].each { x=) }; y=]\n", &[1, 1]);
    errors("begin\nx=)\nrescue\ny=]\nensure\nz=}\nend\n", &[2, 4, 6]);
}

#[test]
fn discards_malformed_delimiters_without_closer_cascades() {
    errors("a = [1, )]\nb = (2, ]\nc = {x: )}\nd = }\n", &[1, 2, 3, 4]);
    errors("x=(1\ny=)\nz=]\n", &[2, 2, 3]);
    errors("x=[1, )\n]\ny=)\n", &[1, 3]);
    errors("x=[1,\n)\n2\n]\ny=)\n", &[2, 5]);
    errors(")\n]\n}\n", &[1, 2, 3]);
    errors("def one\nx=)\n", &[2]);
    errors("[1].each { x=)\n", &[1]);
}

#[test]
fn reports_each_lexical_region_once() {
    errors("0x_\n0b2\nx=)\n", &[1, 2, 3]);
    errors("x = \"#{)}\"\ny = \"#{]}\"\nz = )\n", &[1, 2, 3]);
    errors("x=1\"0\"\ny=2\"0\"\n", &[1, 2]);
}

#[test]
fn keeps_codes_spans_and_fixes_and_skips_the_type_checker() {
    let source = "unknown_name\nputs { a: 1 }\nputs { b: 2 }\nwrong: int = \"text\"\n";
    let error = errors(source, &[2, 3]);
    for (index, diagnostic) in error.diagnostics().iter().enumerate() {
        assert_eq!(diagnostic.code.to_string(), "V0002");
        let brace = source.match_indices('{').nth(index).unwrap().0;
        assert_eq!(diagnostic.span, Span::new(brace, brace + 8));
        assert_eq!(diagnostic.fixes.len(), 1);
        assert!(
            diagnostic.fixes[0]
                .apply(source)
                .unwrap()
                .contains(if index == 0 {
                    "puts({ a: 1 })"
                } else {
                    "puts({ b: 2 })"
                })
        );
    }
    let checked = Engine::new().type_check(source).err().unwrap();
    assert_eq!(checked.diagnostics(), error.diagnostics());
}

#[test]
fn uses_original_utf8_offsets_after_recovery() {
    let source = "é = )\n日本 = ]\n";
    let error = errors(source, &[1, 2]);
    for (diagnostic, token) in error.diagnostics().iter().zip([')', ']']) {
        let offset = source.find(token).unwrap();
        assert_eq!(diagnostic.span, Span::new(offset, offset + 1));
    }
}

#[test]
fn bounds_adversarial_errors_and_nesting() {
    let source = "x = )\n".repeat(10000);
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.diagnostics().len(), 100);
    let source = format!("{}\nx = )\n", "[".repeat(2000));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.message, "syntax nesting too deep");
    assert_eq!(error.diagnostics().len(), 1);
}

#[test]
fn host_compilation_recovers_within_its_budget() {
    let source = "x = )\nputs { a: 1 }\ny = ]\n";
    let engine = Engine::new();
    let expected = engine.compile(source).err().unwrap();
    let actual = engine
        .compile_with_options(source, &vibescript::CallOptions::default())
        .err()
        .unwrap();
    assert_eq!(actual.diagnostics(), expected.diagnostics());
    let options = vibescript::CallOptions {
        deadline: Some(std::time::Instant::now()),
        ..vibescript::CallOptions::default()
    };
    assert_eq!(
        engine
            .compile_with_options(source, &options)
            .err()
            .unwrap()
            .kind,
        ErrorKind::Deadline
    );
}
