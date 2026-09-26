mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorKind, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn invalid_call_members_stop_before_arguments_and_blocks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    let prefix = "class P\nprivate\ndef call(value: int = 7) -> int\nvalue\nend\nend\n";
    let mut checked = vibescript::Engine::new();
    checked.register("mark", |_, _| panic!("mark ran"));
    // A receiver cannot reach the private `call` (V0208), so these calls
    // are refused before their arguments could run.
    for suffix in [
        ".call(mark(1).as(int))",
        "&.call(mark(1).as(int))",
        ".call(*[mark(1).as(int)])",
    ] {
        let source = format!("{prefix}\nP.new{suffix}");
        let error = checked.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0208"], "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.rfind("call(").unwrap(),
            "{source}"
        );
    }
    // Every other receiver has no `call` member, and `P#call` takes no
    // keywords or block, so the rest are refused before anything runs.
    for (receiver, code) in [
        ("{}", "V0203"),
        ("[]", "V0203"),
        ("7", "V0203"),
        ("Math[\"sqrt\"]", "V0112"),
        ("Regexp[\"last_match\"]", "V0106"),
    ] {
        for suffix in [
            ".call(mark(1))",
            "&.call(mark(1))",
            ".call(*[mark(1)])",
            ".call(flag:mark(1)){mark(2)}",
            ".call{mark(1)}",
        ] {
            let source = format!("{prefix}\n{receiver}{suffix}");
            let error = checked.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), [code], "{source}");
            assert!(error.diagnostics()[0].span.start > prefix.len(), "{source}");
        }
    }
    // `P#call` is also private, which a receiver cannot reach.
    for (suffix, codes) in [
        (
            ".call(flag:mark(1)){mark(2)}",
            &["V0208", "V0302", "V0305"][..],
        ),
        (".call{mark(1)}", &["V0208", "V0305"]),
    ] {
        let source = format!("{prefix}\nP.new{suffix}");
        let error = checked.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{source}");
    }
    let output = engine
        .compile("nil&.call(mark(1))")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(json(&output.value).is_null());
    assert!(events.lock().unwrap().is_empty());
    // A hash field is not a member, so it cannot be called either.
    let error = checked.compile("{call:7}.call(mark(1))").err().unwrap();
    assert_eq!(common::codes(&error), ["V0203"]);
    assert_eq!(error.diagnostics()[0].span.start, 9);
}

#[test]
fn valid_call_methods_preserve_binding_blocks_and_control_flow() {
    // A block's break returns its value from `call`, so the result type
    // admits it.
    let script = Engine::new()
        .compile(
            r#"
class C
 def call(value: int = 7, *, flag: int = 0, &block?: int -> int) -> array<int> | int
  out=[value,flag]
  if block_given?
   out.push(yield(value))
  end
  out
 end
end
class D
 def call(options: hash<string, int> = {}) -> hash<string, int>
  options
 end
end
def escape -> int
 C.new.call{return 13}
 99
end
def run -> array<array<int> | int | hash<string, int>>
 c=C.new
 [c.call(2,flag:3){|v|v+1},c.call{break 12},escape,c.call{|v|next v+1},D.new.call({flag:3})]
end
"#,
        )
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[2,3,3],12,13,[7,0,8],{"flag":3}])
    );
    // Keywords do not collect into a hash parameter, and dispatch by name
    // is removed.
    let declaration = "class D\ndef call(options: hash<string, int> = {}) -> hash<string, int>\noptions\nend\nend\n";
    for (call, code, at) in [
        ("D.new.call(flag:3)", "V0302", "flag"),
        ("D.new.send(:call,flag:3)", "V0405", "send"),
    ] {
        let source = format!("{declaration}{call}");
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{call}");
        assert_eq!(error.diagnostics()[0].span.start, source.rfind(at).unwrap());
    }
}

#[test]
fn call_targets_are_selected_before_arguments_mutate_callable_fields() {
    // A builtin function is not a value and a hash field is not a member,
    // so a callable field is refused before anything runs.
    let source = "h={call:Math::sqrt};value=h.call(h.clear.length);[value,h]";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0416", "V0301", "V0203"]);
    let spans: Vec<usize> = error.diagnostics().iter().map(|d| d.span.start).collect();
    assert_eq!(
        spans,
        [
            source.find("::").unwrap(),
            source.find("sqrt").unwrap(),
            source.find("call(").unwrap()
        ]
    );
}

#[test]
fn rejected_call_arguments_release_storage_and_cancellation_still_wins() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    // A call to a private `call` is refused (V0208) before anything runs,
    // so no rejected call is left to hold storage.
    let source = "class P\nprivate\ndef call(value: int = 7) -> int\nvalue\nend\nend\ndef reject\nbegin\nP.new.call(*[mark(1).as(int)])\nrescue RuntimeError\nnil\nend\nend\ndef run(n: int)\nfor i in 1..n\nreject\nend\nnil\nend";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0208"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("call(*").unwrap()
    );
    let cancelled = engine.compile("class C\ndef call(value: any = nil, &block: () -> any) -> any\nyield\nend\nend\nbegin\nC.new.call(cancel()){mark(2)}\nrescue RuntimeError\nmark(3)\nensure\nmark(4)\nend").unwrap().run(CallOptions::default()).unwrap_err();
    assert_eq!(cancelled.kind, ErrorKind::Cancelled);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn unknown_call_members_use_the_reference_wording() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("language-errors.json")).unwrap();
    let mut checked = 0;
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !name.starts_with("call_member_unknown_") {
            continue;
        }
        let source = case["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "def run(input: any) -> any\n{}\nend",
                    case["body"].as_str().unwrap()
                )
            });
        let Some(engine) = common::fixture_engine(case.get("static_error"), &source, name) else {
            checked += 1;
            continue;
        };
        let error = engine
            .compile(&source)
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, case["go_error"].as_str().unwrap(), "{name}");
        checked += 1;
    }
    assert_eq!(checked, 15);
}

#[test]
fn bare_names_receiving_call_follow_the_reference_rules() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("language-errors.json")).unwrap();
    let mut checked = 0;
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        // Cases moved from the language corpus succeed without static types.
        if !name.starts_with("call_receiver_") || case.get("go_error").is_none() {
            continue;
        }
        let source = case["source"].as_str().unwrap();
        let Some(engine) = common::fixture_engine(case.get("static_error"), source, name) else {
            checked += 1;
            continue;
        };
        let error = engine
            .compile(source)
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, case["go_error"].as_str().unwrap(), "{name}");
        checked += 1;
    }
    assert_eq!(checked, 17);
    // A function's result has no `call` member.
    let source = "def helper -> int\n1\nend\nx = 1\n(helper.call)()";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0203"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("call").unwrap()
    );
}
