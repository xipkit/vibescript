use vibescript::{CallOptions, Engine, ErrorKind};

const DECLARATIONS: &str =
    "def f(*a)\n  a\nend\ndef a\n  [0]\nend\ndef x\n  1\nend\nclass X\nend\n";

// The deepest nesting Go v0.70.0 accepts inside `def run`, for each shape.
// One more level is rejected as too deep. These run on the default test
// thread, so they also show that deep syntax needs no extra native stack.
const FORMS: &[(&str, &str, &str, &str, usize)] = &[
    ("groups", "(", "1", ")", 1021),
    ("arrays", "[", "1", "]", 1021),
    ("hashes", "{a: ", "1", "}", 1021),
    ("unary", "!", "true", "", 1021),
    ("calls", "f(", "1", ")", 1021),
    ("indices", "a[", "0", "]", 1021),
    ("powers", "1 ** ", "1", "", 1021),
    ("ternaries", "true ? 1 : ", "1", "", 1021),
    ("if", "if true\n", "1\n", "end\n", 1021),
    ("unless", "unless false\n", "1\n", "end\n", 1021),
    ("while", "while false\n", "1\n", "end\n", 1021),
    ("for", "for q in [1]\n", "1\n", "end\n", 1021),
    ("ensure", "begin\n", "1\n", "ensure\n1\nend\n", 1021),
    ("rescue", "begin\n", "1\n", "rescue\n1\nend\n", 1021),
    ("case", "case 1\nwhen 1\n", "1\n", "end\n", 1021),
    ("yield", "yield(", "1", ")", 1021),
    ("blocks", "f { ", "it", " }", 340),
    ("do blocks", "f do\n", "it\n", "end\n", 340),
    ("block parameters", "f { |v| ", "v", " }", 340),
    ("sums", "", "x", " + 1", 1021),
    ("members", "", "x", ".x", 1021),
    ("scopes", "", "X", "::X", 1021),
    ("index chains", "", "a", "[0]", 1021),
    ("call chains", "", "f", "()", 1021),
    ("rescue modifiers", "", "x", " rescue 1", 1021),
    ("method calls", "", "x", ".to_s()", 510),
    ("members in a block", "f { x", "", ".x", 1018),
];

fn nested(prefix: &str, inner: &str, suffix: &str, depth: usize) -> String {
    let body = if prefix.is_empty() {
        format!("{inner}{}", suffix.repeat(depth))
    } else if inner.is_empty() {
        format!("{prefix}{} }}", suffix.repeat(depth))
    } else {
        format!("{}{inner}{}", prefix.repeat(depth), suffix.repeat(depth))
    };
    format!("{DECLARATIONS}def run\n{body}\nend\n")
}

fn assert_too_deep(source: &str, name: &str) {
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax, "{name}: {error}");
    assert!(
        error.message.contains("syntax nesting too deep"),
        "{name}: {error}"
    );
}

#[test]
fn every_form_reaches_the_reference_syntax_depth_on_the_default_stack() {
    for &(name, prefix, inner, suffix, depth) in FORMS {
        let script = Engine::new()
            .compile(&nested(prefix, inner, suffix, depth))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        // Some shapes fail at runtime, but none may exhaust the native stack.
        let _ = script.call("run", &[], CallOptions::default());
        let _ = script.check_call("run", &[], &CallOptions::default());
        assert_too_deep(&nested(prefix, inner, suffix, depth + 1), name);
    }
}

#[test]
fn nested_declarations_and_destructuring_reach_the_reference_depth() {
    for (name, prefix, suffix, depth) in [
        ("classes", "class A\n", "end\n", 512),
        ("modules", "module A\n", "end\n", 1023),
    ] {
        let source = |depth: usize| format!("{}{}", prefix.repeat(depth), suffix.repeat(depth));
        Engine::new()
            .compile(&source(depth))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_too_deep(&source(depth + 1), name);
    }
    let destructure = |depth: usize| {
        format!(
            "def run\na, {}b, c{} = [1, [2, 3]]\n[a, b, c]\nend",
            "(".repeat(depth),
            ")".repeat(depth)
        )
    };
    let script = Engine::new().compile(&destructure(1020)).unwrap();
    script.call("run", &[], CallOptions::default()).unwrap();
    assert_too_deep(&destructure(1021), "destructuring");
}

#[test]
fn syntax_depth_counts_across_interpolations() {
    let depth = 1024 * 3 / 4;
    let inner = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    Engine::new().compile(&inner).unwrap();
    let source = format!("{}\"#{{{inner}}}\"{}", "(".repeat(depth), ")".repeat(depth));
    assert_too_deep(&source, "interpolation");
}

#[test]
fn elsif_chains_and_deep_aliases_do_not_nest() {
    for form in [
        "if x == 0\n0\n{elsif}end",
        "y = if x == 0 then 0\n{elsif}end\ny",
    ] {
        let elsif = (1..5000)
            .map(|i| {
                if form.starts_with("if") {
                    format!("elsif x == {i}\n{i}\n")
                } else {
                    format!("elsif x == {i} then {i}\n")
                }
            })
            .collect::<String>();
        let source = format!("def run(x)\n{}\nend", form.replace("{elsif}", &elsif));
        let script = Engine::new().compile(&source).unwrap();
        let result = script
            .call(
                "run",
                &[vibescript::Value::int(4999)],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_int(), Some(4999));
    }
    let body = format!("{}1{}", "[".repeat(1017), "]".repeat(1017));
    let source = format!(
        "class Box\ndef original\n{body}\nend\nalias copied original\nend\ndef run\nBox.new.copied\nend"
    );
    let script = Engine::new().compile(&source).unwrap();
    script.call("run", &[], CallOptions::default()).unwrap();
    let body = format!("{}1{}", "[".repeat(1018), "]".repeat(1018));
    let source = format!("def original\n{body}\nend\nalias copied original\ndef run\ncopied\nend");
    let script = Engine::new().compile(&source).unwrap();
    script.call("run", &[], CallOptions::default()).unwrap();
}
