//! `is_type?` is the one type predicate. The removed `is_a?`, `kind_of?`,
//! `instance_of?`, `respond_to?`, symbolic `reduce(:name)`, builtins read
//! as values and namespaces written like hashes are reported by the
//! surface tests and the builtin tests.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn core_predicates_use_strict_types_and_exact_class_identity() {
    assert_eq!(
        result(
            "class C\nend\nmodule M\nend\nc=C.new;[c.is_type?(:C),c.is_type?(:M),C.is_type?(:C),1.is_type?(:C),1.is_type?(:int),1.is_type?(:float),\"1\".is_type?(:int),9223372036854775808.is_type?(:int),JSON.is_type?(:hash),nil.is_type?(:\"int?\")]"
        ),
        serde_json::json!([
            true, false, false, false, true, false, false, true, true, true
        ])
    );
}

#[test]
fn named_atoms_resolve_in_the_active_lexical_scope() {
    assert_eq!(
        result(
            "class C\nend\nclass D\nend\nenum E\nA\nend\nv=C.new;[v.is_type?(:C),nil.is_type?(:\"C?\"),[1].map {nil.is_type?(:\"C?\")},E::A.is_type?(:E),E.is_type?(:E),:a.is_type?(:E)]"
        ),
        serde_json::json!([true, true, [true], true, false, false])
    );
    assert_eq!(
        result(
            "class User\nend\n[User.new.is_type?(:USER),nil.is_type?(:\"USER?\"),nil.is_type?(:\"User?\"),nil.is_type?(:\"Missing?\")]"
        ),
        serde_json::json!([false, false, true, false])
    );
    // A type atom is a symbol.
    let source = "nil.is_type?(\"int?\")";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(error.diagnostics()[0].span.start, 13);
}

#[test]
fn invalid_predicates_are_refused_before_blocks() {
    let mut engine = vibescript::Engine::new();
    engine.register("entered", |_, _| panic!("entered ran"));
    for receiver in ["1", "C.new", "C", "{x:int}"] {
        for (suffix, expected) in [
            (
                "(x:1) {entered()}",
                vec![("V0301", "is_type?"), ("V0302", "x:"), ("V0305", "{")],
            ),
            ("(1) {entered()}", vec![("V0101", "1)"), ("V0305", "{")]),
            ("", vec![("V0301", "is_type?")]),
        ] {
            let source = format!("class C\nend\n{receiver}.is_type?{suffix}");
            let error = engine.compile(&source).err().unwrap();
            let found: Vec<(String, usize)> = error
                .diagnostics()
                .iter()
                .map(|d| (d.code.to_string(), d.span.start))
                .collect();
            let call = source.find(".is_type?").unwrap();
            let expected: Vec<(String, usize)> = expected
                .into_iter()
                .map(|(code, text)| (code.to_owned(), call + source[call..].find(text).unwrap()))
                .collect();
            assert_eq!(found, expected, "{source}");
        }
    }
}

#[test]
fn raw_type_atoms_have_bounded_errors_and_preserve_byte_quoting() {
    let script = Engine::new()
        .compile("def run(atom: symbol) -> bool\nnil.is_type?(atom)\nend")
        .unwrap();
    for (bytes, message) in [
        (
            vec![0xff],
            "is_type? supports type atoms only, got \"\\xff\"",
        ),
        (
            b"int\0".to_vec(),
            "is_type? supports type atoms only, got \"int\\x00\"",
        ),
        (
            b"int\n".to_vec(),
            "is_type? supports type atoms only, got \"int\\n\"",
        ),
        (
            vec![b'a'; 257],
            "is_type? supports type atoms only, got 257 bytes",
        ),
        (
            vec![b'.'; 1 << 20],
            "is_type? supports type atoms only, got 1048576 bytes",
        ),
    ] {
        let error = script
            .call("run", &[Value::symbol(bytes)], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, message);
    }
    assert_eq!(
        script
            .call(
                "run",
                &[Value::symbol("A".repeat(256).into_bytes())],
                CallOptions::default()
            )
            .unwrap()
            .value
            .to_string(),
        "false"
    );
    let error = script
        .call(
            "run",
            &[Value::symbol(b"JSON.Missing?".to_vec())],
            CallOptions::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.message,
        "unknown type atom \"JSON.Missing?\" in is_type?"
    );
}

#[test]
fn cancellation_in_a_method_called_by_a_reduction_prevents_later_effects() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    engine.register("after", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script=engine.compile("class C\ndef apply(x: int) -> int\nstop();after()\nx\nend\nend\nbegin\n[1].reduce(0) { |sum, x| C.new.apply(x) };after()\nrescue RuntimeError | LimitError\nafter()\nensure\nafter()\nend").unwrap();
    let error = script
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
