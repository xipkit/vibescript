//! A computed call calls the value of an expression, such as a callee a
//! rescue chooses. Static types call only functions (ADR-007), so each form
//! these tests once ran is refused at compile time, where the diagnostic
//! names the expression that was called.

mod common;

use vibescript::{Engine, ErrorKind};

/// Asserts that `source` is refused with `codes`, the first at `at`.
#[track_caller]
fn refused(source: &str, codes: &[&str], at: &str) {
    let mut engine = common::static_engine();
    for host in ["record", "observe", "stop", "fallback"] {
        engine.register(host, |_, _| panic!("a host ran"));
    }
    engine.register_with_keywords("sms", |_, _, _| panic!("a host ran"));
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), codes, "{source}");
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find(at).unwrap(),
        "{source}"
    );
}

#[test]
fn computed_targets_support_nested_calls_keywords_splats_and_blocks() {
    refused(
        "def f(x: int = 42) -> int\nx\nend\n(f rescue f)(8)",
        &["V0310"],
        "rescue",
    );
    refused(
        "def f(x: int = 42) -> int\nx\nend\n(missing rescue f)((unknown rescue f)(8))",
        &["V0201", "V0201"],
        "missing",
    );
    refused(
        "def f(x: int = 2, &block: int -> int) -> int\nyield(x)\nend\n(f rescue f)(3) {|x| x+1}",
        &["V0304", "V0310", "V0304"],
        "f rescue",
    );
}

#[test]
fn begin_expressions_are_called_after_selection_and_cleanup() {
    refused(
        "events: array<int> = [];x=(begin\nevents.push(1);JSON::parse\nend)(begin\nevents.push(2);\"[8]\"\nend);[x,events]",
        &["V0106", "V0301"],
        "begin",
    );
}

#[test]
fn missing_namespace_members_are_catchable_at_lookup() {
    refused("JSON.nope rescue 7", &["V0203"], "nope");
    refused("JSON::nope rescue 7", &["V0203"], "nope");
    refused(
        "def good(x: int = 42) -> int\nx\nend\nbegin\n(JSON.nope rescue good)(8)\nrescue RuntimeError\n99\nend",
        &["V0203"],
        "nope",
    );
}

#[test]
fn selection_rescue_finishes_before_arguments_and_callee_body() {
    refused(
        "def select -> int\nrecord(1);raise \"lookup\"\nend\ndef argument -> int\nrecord(2);7\nend\n\
         def callee(x: int) -> int\nrecord(3);x\nend\nbegin\n(select rescue callee)(argument)\nrescue\nrecord(4)\nend",
        &["V0310", "V0301"],
        "rescue callee",
    );
}

#[test]
fn ordinary_expressions_still_reject_function_values() {
    for (expression, codes, at) in [
        ("[f][0]()", &["V0310", "V0301"][..], "[f]"),
        ("(true ? f : f)()", &["V0310", "V0301", "V0301"], "true"),
        ("(false || f)()", &["V0310", "V0301", "V0105"], "false"),
        ("(begin\nf\nend)()", &["V0310", "V0301"], "begin"),
        ("{cb:f}.cb()", &["V0301", "V0203"], "f}"),
    ] {
        refused(
            &format!("def f(x: int) -> int\nx\nend\n{expression}"),
            codes,
            at,
        );
    }
}

#[test]
fn selected_receivers_and_builtins_survive_argument_side_effects() {
    refused(
        "class C\ngetter value: int\ndef initialize(x: int)\n@value=x\nend\ndef add(x: int) -> int\n@value+x\nend\nend\n\
         c=C.new(4);x=(c.add rescue c.add)(begin\nc=C.new(7);8\nend);[x,c.value]",
        &["V0301", "V0310", "V0301"],
        "add rescue",
    );
}

#[test]
fn failed_computed_calls_release_argument_and_receiver_storage() {
    refused(
        "def factory -> string\n\"x\"*8192\nend\ndef fail -> int\nobserve();raise \"argument\"\nend\n\
         def run(n: int) -> int\ni=0;while i<n\nbegin\n(factory rescue factory)(fail)\nrescue\n0\nend;i+=1\nend;42\nend",
        &["V0310"],
        "rescue factory",
    );
}

#[test]
fn exhaustion_and_cancellation_in_selection_or_arguments_cannot_be_rescued() {
    for (expression, at) in [
        ("(stop() rescue fallback)()", "rescue"),
        ("(fallback() rescue fallback())(stop())", "rescue"),
    ] {
        refused(
            &format!("begin\n{expression}\nrescue\nfallback()\nensure\nfallback()\nend"),
            &["V0106"],
            at,
        );
    }
}

#[test]
fn protected_match_data_and_errors_reject_wrapped_mutators() {
    refused(
        "def fallback -> int\n42\nend\nm=\"a\".match(\"a\");(m.clear rescue fallback)()",
        &["V0107", "V0203"],
        "clear",
    );
}

#[test]
fn selected_host_capabilities_keep_keyword_contracts_and_step_limits() {
    refused(
        "(sms rescue sms)(\"destination\", body:\"hello\")",
        &["V0106"],
        "rescue",
    );
}

#[test]
fn computed_call_nesting_reaches_the_parser_guard() {
    let source = format!("JSON::parse{}", "()".repeat(1100));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    let source = format!("{}f{}()", "(missing rescue ".repeat(1100), ")".repeat(1100));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    let error = Engine::new().compile("nil&.f()(1).field=2").err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}
