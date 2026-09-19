use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, CheckReport, CheckedOutcome, Engine, ErrorKind, HostMethod, Limits,
    Position, Signature, Value,
};

fn check(source: &str) -> CheckReport {
    Engine::new()
        .compile(source)
        .unwrap()
        .check_call("run", &[], &CallOptions::default())
        .unwrap()
}

fn equality_branch(setup: &str, expression: &str, expected: bool, options: CallOptions) {
    let (yes, no) = if expected {
        ("7", "'wrong'")
    } else {
        ("'wrong'", "7")
    };
    let source = format!("{setup}; def run -> int; if {expression}; {yes}; else; {no}; end; end");
    let script = Engine::new().compile(&source).unwrap();
    let report = script.check_call("run", &[], &options).unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
    assert_eq!(
        script.call("run", &[], options).unwrap().value.as_int(),
        Some(7),
        "{source}"
    );
}

#[test]
fn structural_equality_selects_typed_branches_for_literals_and_computed_values() {
    for (expression, expected) in [
        ("[] == []", true),
        ("[2,3] == [2,3]", true),
        ("[2,3] != [2,3]", false),
        ("[2] == [2,3]", false),
        ("[{a:[1]}] == [{a:[1.0]}]", true),
        ("[{a:[1]}] != [{a:[1.0]}]", false),
        ("[9007199254740993] == [9007199254740992.0]", false),
        ("[9223372036854775807] == [9223372036854775808.0]", false),
        ("{a:1,b:[2]} == {b:[2.0],\"a\":1.0}", true),
        ("{a:1} == {b:1}", false),
        ("{a:1} == {a:1,b:2}", false),
        ("{a:1} == [1]", false),
        ("nil == []", false),
        ("[1] == false", false),
        ("[:a] == ['a']", false),
        ("[1,2].map {|n| n+1} == [2,3]", true),
        ("(begin; a=[1]; a.push(2); a; end) == [1,2]", true),
        ("(begin; a=[1]; a == a.push(2); end)", false),
        (
            "(begin; a={x:[1]}; a == (begin; a.x.push(2); a; end); end)",
            false,
        ),
        ("[1,1.0].uniq == [1,1.0]", true),
        ("[[1],[1.0]].uniq == [[1]]", true),
    ] {
        equality_branch("", expression, expected, CallOptions::default());
    }
}

#[test]
fn structural_equality_preserves_host_object_kinds_and_nan_semantics() {
    let options = CallOptions {
        globals: [
            ("nan".into(), Value::float(f64::NAN)),
            ("negative_zero".into(), Value::float(-0.0)),
            (
                "object".into(),
                Value::object(vec![(b"a".to_vec(), Value::int(1))]),
            ),
            (
                "other".into(),
                Value::object(vec![(b"a".to_vec(), Value::float(1.0))]),
            ),
        ]
        .into(),
        ..CallOptions::default()
    };
    for (expression, expected) in [
        ("[nan] == [nan]", false),
        ("[nan] != [nan]", true),
        ("{a:nan} == {a:nan}", false),
        ("[negative_zero] == [0.0]", true),
        ("{a:1} == object", false),
        ("object == {a:1}", false),
        ("[object] == [other]", true),
        ("[nan,nan].uniq.length == 1", true),
        ("[[nan],[nan]].uniq.length == 2", true),
    ] {
        equality_branch("", expression, expected, options.clone());
    }
}

#[test]
fn structural_equality_keeps_nested_instances_attached_to_their_identity() {
    let setup = "class Plain; end; class C; property link; def ==(other); true; end; end";
    for (body, expected) in [
        ("Plain.new == []", false),
        ("[] == Plain.new", false),
        ("C == []", false),
        ("[] == C", false),
        ("C.new == []", true),
        ("a=C.new; b=C.new; a == b", true),
        ("a=C.new; b=C.new; [a] == [b]", false),
        ("a=C.new; a.link=a; [a] == [a.link]", true),
        ("a=C.new; b=C.new; {x:a} != {x:b}", true),
        ("a=C.new; [a,a] == [a,a]", true),
    ] {
        equality_branch(
            setup,
            &format!("(begin; {body}; end)"),
            expected,
            CallOptions::default(),
        );
    }
}

#[test]
fn structural_equality_general_inputs_retain_both_results_without_host_effects() {
    for op in ["==", "!="] {
        let source = format!(
            "class C; def {op}(other); 7; end; end; def make; C.new; end; def run(x:any)->int; if (x {op} []).is_type?(:bool); 7; else; 'wrong'; end; end"
        );
        let script = Engine::new().compile(&source).unwrap();
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(
            !report.is_clean() && !report.incomplete.is_empty(),
            "{source}: {report:?}"
        );
        let receiver = script
            .call("make", &[], CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(
            script
                .call("run", &[receiver], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
    }
    for parameters in [
        "a:array<int>,b:array<int>",
        "a:hash<string,int>,b:hash<string,int>",
    ] {
        let source = format!("def run({parameters}) -> bool; a == b; end");
        let script = Engine::new().compile(&source).unwrap();
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let source = format!("def run({parameters}) -> int; if a == b; 7; else; 'wrong'; end; end");
        let report = Engine::new()
            .compile(&source)
            .unwrap()
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(
            report.incomplete.is_empty() && !report.diagnostics.is_empty(),
            "{source}: {report:?}"
        );
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let source = "def run(flag:bool) -> int; a=if flag; [1]; else; [2]; end; if a==[1.0]; tick(); else; 7; end; end";
    let script = engine.compile(source).unwrap();
    assert!(
        script
            .check_function("run", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    for flag in [true, false] {
        assert_eq!(
            script
                .call("run", &[Value::boolean(flag)], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn whole_file_check_includes_unused_declarations_and_preserves_call_scopes() {
    let script = Engine::new()
        .compile("7;def unused(n:string)->int;n;end;class C;private def bad->bool;7;end;end")
        .unwrap();
    assert!(
        script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.diagnostics.len(), 2, "{report:?}");
    assert!(
        report
            .diagnostics
            .windows(2)
            .all(|pair| pair[0].offset < pair[1].offset)
    );
    assert!(report.stats.steps > 0);
    assert!(report.stats.retained_memory_bytes > 0);
    let script = Engine::new()
        .compile("x=7;module M;K=x;def self.value->int;K;end;end")
        .unwrap();
    assert!(script.check(&CallOptions::default()).unwrap().is_clean());
    assert!(
        !script
            .check_function("M.value", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn whole_file_checks_constructor_and_block_domains_without_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let count = effects.clone();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let count = effects.clone();
    engine.set_output_writer(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let script = engine.compile("module M;K=tick();end;class C;property n:int;def initialize(n:int=tick());@n=n;puts @n;end;def value->int;if block_given?;yield;end;@n;end;end;def run;C.new.value;end").unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(effects.load(Ordering::Relaxed), 3);
}

#[test]
fn whole_file_reports_missing_required_files_without_running_them() {
    let report = Engine::new()
        .compile("def unused;require('missing');end")
        .unwrap()
        .check(&CallOptions::default())
        .unwrap();
    assert!(!report.is_clean());
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("module paths not configured")),
        "{report:?}"
    );
}

fn executed(outcome: CheckedOutcome) -> vibescript::Outcome {
    match outcome {
        CheckedOutcome::Executed(outcome) => outcome,
        other => panic!("{other:?}"),
    }
}

#[test]
fn checked_rendering_rejects_bad_conversion_contracts_before_host_effects() {
    for expression in [r##""#{c}""##, "format('%s',c)", "puts(c)"] {
        let effects = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let mut engine = Engine::new();
        engine.register("effect", move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        });
        let count = writes.clone();
        engine.set_output_writer(move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        let script = engine.compile(&format!("class C;def initialize(@value);end;def to_s -> int;effect();@value;end;end;def run(value);c=C.new(value);{expression};'done';end")).unwrap();
        let report = script
            .check_call("run", &[Value::int(7)], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{expression}: {report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        let result = executed(
            script
                .checked_call("run", &[Value::int(7)], CallOptions::default())
                .unwrap(),
        );
        assert_eq!(result.value.as_bytes(), Some(b"done".as_slice()));
        assert_eq!(effects.load(Ordering::Relaxed), 1);
        assert_eq!(
            writes.load(Ordering::Relaxed),
            usize::from(expression == "puts(c)")
        );
        let CheckedOutcome::Rejected(report) = script
            .checked_call("run", &[Value::boolean(false)], CallOptions::default())
            .unwrap()
        else {
            panic!("bad conversion contract executed host effects");
        };
        assert!(report.incomplete.is_empty(), "{expression}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{expression}: {report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 1);
        assert_eq!(
            writes.load(Ordering::Relaxed),
            usize::from(expression == "puts(c)")
        );
    }
}

#[test]
fn checked_forwarding_and_predicates_reject_bad_calls_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("class C;def value(x);effect();x;end;end;def accept(x:int);x;end;def run(x);c=C.new;if c.respond_to?(:value)&&c.is_type?(:C);accept(c.public_send(:send,:value,x));else;raise 'missing';end;end").unwrap();
    let report = script
        .check_call("run", &[Value::int(7)], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let outcome = executed(
        script
            .checked_call("run", &[Value::int(7)], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(7));
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[Value::boolean(false)], CallOptions::default())
        .unwrap()
    else {
        panic!("invalid forwarded result executed host effects");
    };
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let script = engine.compile("class C;def configure(options);effect();options;end;end;def run;effect();C.new.configure(n:7);end").unwrap();
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[], CallOptions::default())
        .unwrap()
    else {
        panic!("strict keywords executed host effects");
    };
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn checked_operators_reject_bad_results_before_host_effects() {
    for (definition, expression) in [
        ("def +(value);effect();value;end", "c+value"),
        ("def [](value);effect();value;end", "c[value]"),
        ("def []=(i,value);effect();99;end", "begin;c[0]=value;end"),
    ] {
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let mut engine = Engine::new();
        engine.register("effect", move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        });
        let script = engine.compile(&format!("class C;{definition};end;def accept(value:int);value;end;def run(value);c=C.new;accept({expression});end")).unwrap();
        let report = script
            .check_call("run", &[Value::int(7)], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
        let outcome = executed(
            script
                .checked_call("run", &[Value::int(7)], CallOptions::default())
                .unwrap(),
        );
        assert_eq!(outcome.value.as_int(), Some(7));
        assert_eq!(effects.load(Ordering::Relaxed), 1);
        let CheckedOutcome::Rejected(report) = script
            .checked_call("run", &[Value::boolean(false)], CallOptions::default())
            .unwrap()
        else {
            panic!("invalid operator result executed host effects");
        };
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(!report.diagnostics.is_empty(), "{report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn checked_constructors_reject_bad_property_writes_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("class C;getter items:array<int>;def initialize;effect();@items=[1];end;def add(value);@items.push(value);end;end;def run(value);c=C.new;c.add(value);c.items;end").unwrap();
    let report = script
        .check_call("run", &[Value::int(2)], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let result = executed(
        script
            .checked_call("run", &[Value::int(2)], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(result.value.to_string(), "[1, 2]");
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[Value::boolean(false)], CallOptions::default())
        .unwrap()
    else {
        panic!("bad field write executed constructor effects")
    };
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|issue| issue.message.contains("Property value") && issue.message.contains("int")),
        "{report:?}"
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn checked_namespace_calls_report_callee_types_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, args| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(args[0].clone())
    });
    let script = engine
        .compile(
            "module M;def self.answer(x:int)->int;effect(x+1);end;end;def run(x);M.answer(x);end",
        )
        .unwrap();
    assert!(
        script
            .check_call("run", &[Value::int(6)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let outcome = executed(
        script
            .checked_call("run", &[Value::int(6)], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(7));
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[Value::boolean(false)], CallOptions::default())
        .unwrap()
    else {
        panic!("invalid namespace call executed")
    };
    assert!(report.incomplete.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("int"))
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn checked_namespace_state_is_initialized_only_during_accepted_execution() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("module M;C=1;effect();def self.answer(x:int)->int;C+x;end;end;def run(x);M.answer(x);end").unwrap();
    let report = script
        .check_call("run", &[Value::int(6)], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let outcome = executed(
        script
            .checked_call("run", &[Value::int(6)], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(7));
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[Value::boolean(false)], CallOptions::default())
        .unwrap()
    else {
        panic!("invalid namespace call executed its initializer")
    };
    assert!(report.incomplete.is_empty());
    assert!(!report.diagnostics.is_empty());
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn public_calls_bind_keywords_defaults_rest_and_concrete_paths() {
    let script = Engine::new()
        .compile("def unused()->int;\"bad\";end;def run(a:int,b:2,**rest);[a,b,rest[:x]];end")
        .unwrap();
    let args = [Value::int(7)];
    let keywords = [
        ("b".into(), Value::int(3)),
        ("b".into(), Value::int(5)),
        ("x".into(), Value::int(9)),
    ];
    let options = CallOptions::default();
    let report = script
        .check_call_with_keywords("run", &args, &keywords, &options)
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.stats.steps > 0);
    assert_eq!(report.stats.retained_memory_bytes, 0);
    let outcome = executed(
        script
            .checked_call_with_keywords("run", &args, &keywords, options)
            .unwrap(),
    );
    assert_eq!(outcome.value.to_string(), "[7, 5, 9]");
    let script = Engine::new()
        .compile("def run(flag)->int;if flag;\"bad\";else;7;end;end")
        .unwrap();
    assert!(
        script
            .check_call("run", &[Value::boolean(false)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert!(
        !script
            .check_call("run", &[Value::boolean(true)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn reports_source_positions_and_known_parameter_and_return_types() {
    let source = "def run(x:int) -> int\n  x\nend";
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_call(
            "run",
            &[Value::bytes(b"bad".to_vec())],
            &CallOptions::default(),
        )
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    let diagnostic = &report.diagnostics[0];
    assert!(
        diagnostic.message.contains("expected int, got string"),
        "{diagnostic}"
    );
    assert_eq!(diagnostic.function, "run");
    assert_eq!(diagnostic.position.line, 1);
    assert!(diagnostic.code_frame.contains("def run"));
    assert!(diagnostic.filename.is_none());
    assert!(diagnostic.to_string().contains(&diagnostic.code_frame));
    let report = check("def run() -> int\n  \"é\"\nend");
    let diagnostic = &report.diagnostics[0];
    assert_eq!(diagnostic.message, "Return value: expected int, got string");
    assert_eq!(diagnostic.position, Position { line: 2, column: 3 });
    assert!(diagnostic.code_frame.contains('é'));
    assert!(report.stats.retained_memory_bytes > 0);
    assert!(report.stats.peak_memory_bytes >= report.stats.retained_memory_bytes);
}

#[test]
fn rejection_precedes_callbacks_defaults_and_initializer_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    for source in [
        "def run(x=effect())->int;effect();\"bad\";end",
        "class C;effect();end;def run->int;effect();\"bad\";end",
    ] {
        let script = engine.compile(source).unwrap();
        let CheckedOutcome::Rejected(report) = script
            .checked_call("run", &[], CallOptions::default())
            .unwrap()
        else {
            panic!("rejected script ran")
        };
        assert!(!report.is_clean());
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    let script = engine.compile("def run;effect();end").unwrap();
    let options = CallOptions {
        capabilities: vec![Capability::new("sms", move |_| {
            panic!("checker invoked factory")
        })],
        ..CallOptions::default()
    };
    let CheckedOutcome::Rejected(report) = script.checked_call("run", &[], options).unwrap() else {
        panic!("factory script ran")
    };
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.incomplete.len(), 1);
    assert!(report.incomplete[0].message.contains("sms"));
    assert!(report.incomplete[0].message.contains("factory"));
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn dynamic_unknowns_are_clean_and_runtime_failures_remain_errors() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("read", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::bytes(b"bad".to_vec()))
    });
    let script = engine.compile("def run()->int;read();end").unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .checked_call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn checked_calls_preserve_value_isolation_and_ignored_block_transfers() {
    let method = HostMethod::new_with_block("visit", |host, _, _| {
        for _ in 0..3 {
            let _ = host.call_block(&[]);
        }
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: true,
    })
    .unwrap();
    let driver = Value::object(vec![(b"visit".to_vec(), method.value())]);
    let input = Value::array(vec![Value::int(1)]);
    for (transfer, expected) in [("break 7", "[7, [1, 2]]"), ("return 9", "9")] {
        let script = Engine::new()
            .compile(&format!(
                "def run(driver,a);n=driver.visit{{a.push(2);{transfer}}};[n,a];end"
            ))
            .unwrap();
        for _ in 0..2 {
            let outcome = executed(
                script
                    .checked_call(
                        "run",
                        &[driver.clone(), input.clone()],
                        CallOptions::default(),
                    )
                    .unwrap(),
            );
            assert_eq!(outcome.value.to_string(), expected);
            assert_eq!(input.as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn report_retention_does_not_keep_code_callbacks_or_arguments_alive() {
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        let _ = &marker;
        Ok(Value::nil())
    });
    let script = engine.compile("def run;1-\"bad\";end").unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(!report.is_clean());
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    drop(script);
    drop(engine);
    assert!(weak.upgrade().is_none());
    assert!(report.diagnostics[0].message.contains("int and string"));
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let method = HostMethod::new("send", move |_, _, _| {
        let _ = &marker;
        panic!("argument callback ran")
    });
    let input = Value::object(vec![(b"send".to_vec(), method.value())]);
    let script = Engine::new().compile("def run(x:int);x;end").unwrap();
    let report = script
        .check_call("run", std::slice::from_ref(&input), &CallOptions::default())
        .unwrap();
    assert!(!report.is_clean());
    drop(input);
    drop(method);
    drop(script);
    assert!(weak.upgrade().is_none());
    assert!(report.diagnostics[0].message.contains("attached method"));
}

#[test]
fn diagnostics_are_sorted_deduplicated_and_stable_across_contexts() {
    let source = "def bad(x);begin;x-\"bad\";rescue;0;end;end\ndef run\n bad(1)\n bad(2)\nend";
    let mut previous = None;
    for _ in 0..8 {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{report:?}");
        let messages: Vec<_> = report
            .diagnostics
            .iter()
            .map(|d| (d.offset, d.function.clone(), d.message.clone()))
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        if let Some(previous) = &previous {
            assert_eq!(&messages, previous);
        }
        previous = Some(messages);
    }
    let source = "def second()->int;\"bad\";end\ndef first()->int;\"bad\";end\ndef run(flag);if flag;first();else;second();end;end";
    let mut engine = Engine::new();
    engine.register("unknown", |_, _| panic!("checker executed"));
    let script = engine
        .compile(&format!("{source}\ndef root;run(unknown());end"))
        .unwrap();
    let report = script
        .check_call("root", &[], &CallOptions::default())
        .unwrap();
    assert_eq!(report.diagnostics.len(), 2, "{report:?}");
    assert_eq!(report.diagnostics[0].function, "second");
    assert_eq!(report.diagnostics[1].function, "first");
    assert!(
        report
            .diagnostics
            .windows(2)
            .all(|pair| pair[0].offset <= pair[1].offset)
    );
}

#[test]
fn guards_cancellation_and_quotas_stop_before_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("def run;effect();end").unwrap();
    for kind in [
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
        ErrorKind::Steps,
        ErrorKind::Memory,
    ] {
        let mut options = CallOptions::default();
        match kind {
            ErrorKind::Cancelled => options.cancellation.cancel(),
            ErrorKind::Deadline => options.deadline = Some(std::time::Instant::now()),
            ErrorKind::Steps => options.limits.steps = Some(0),
            ErrorKind::Memory => options.limits.memory_bytes = Some(0),
            _ => unreachable!(),
        }
        assert_eq!(
            script.check_call("run", &[], &options).unwrap_err().kind,
            kind
        );
        assert_eq!(
            script.checked_call("run", &[], options).unwrap_err().kind,
            kind
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    assert_eq!(
        script
            .checked_call("missing", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
}

#[test]
fn public_reports_obey_exact_and_sampled_work_and_memory_limits() {
    let script = Engine::new()
        .compile("def run(flag);if flag;[1]+\"bad\";else;{}+2;end;end")
        .unwrap();
    let input = [Value::boolean(true)];
    let baseline = script
        .check_call("run", &input, &CallOptions::default())
        .unwrap();
    assert!(!baseline.is_clean());
    let stats = baseline.stats;
    drop(baseline);
    for (memory, steps, expected) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script
                .check_call("run", &input, &options)
                .err()
                .map(|e| e.kind),
            expected
        );
    }
    for sample in 0..16 {
        for memory in [false, true] {
            let mut options = CallOptions::default();
            let kind = if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 16);
                ErrorKind::Memory
            } else {
                options.limits.steps = Some(stats.steps * sample as u64 / 16);
                ErrorKind::Steps
            };
            assert_eq!(
                script.check_call("run", &input, &options).unwrap_err().kind,
                kind
            );
        }
    }
}

#[test]
fn lazy_globals_and_strict_validation_keep_their_entry_order() {
    let script = Engine::new().compile("def run;7;end").unwrap();
    let options = CallOptions {
        globals: [("unused".into(), Value::bytes(vec![b'x'; 128 * 1024]))].into(),
        limits: Limits {
            memory_bytes: Some(48 * 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert!(script.check_call("run", &[], &options).unwrap().is_clean());
    assert_eq!(
        executed(script.checked_call("run", &[], options).unwrap())
            .value
            .as_int(),
        Some(7)
    );
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(x);x;end").unwrap();
    let options = CallOptions {
        globals: [(
            "unused".into(),
            HostMethod::new("send", |_, _, _| panic!("called")).value(),
        )]
        .into(),
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .check_call("missing", &[], &options)
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    assert_eq!(
        script.check_call("run", &[], &options).unwrap_err().kind,
        ErrorKind::Runtime
    );
}

#[test]
fn descriptors_remain_attached_and_old_grants_are_rejected() {
    let method = HostMethod::new("send", |_, _, _| panic!("invalid grant called"));
    let producer = Engine::new().compile("def run(x);x;end").unwrap();
    let fresh = Value::object(vec![(b"send".to_vec(), method.value())]);
    let old = producer
        .call("run", &[fresh], CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("def run(sms);sms.send();end")
        .unwrap();
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[old], CallOptions::default())
        .unwrap()
    else {
        panic!("old grant approved")
    };
    assert!(report.diagnostics[0].message.contains("earlier invocation"));
    let CheckedOutcome::Rejected(report) = producer
        .checked_call("run", &[method.value()], CallOptions::default())
        .unwrap()
    else {
        panic!("detached method approved")
    };
    assert!(report.diagnostics[0].message.contains("Attached methods"));
}

#[test]
fn diagnostic_text_preserves_raw_keys_and_distinct_enum_declarations() {
    let script = Engine::new().compile("def run(x:int);x;end").unwrap();
    let input = Value::object(vec![(vec![b'\n', 0xff, 0], Value::int(1))]);
    let report = script
        .check_call("run", &[input], &CallOptions::default())
        .unwrap();
    let text = &report.diagnostics[0].message;
    assert!(
        text.contains("\\n") && text.contains("\\xff") && text.contains("\\0"),
        "{text}"
    );
    assert!(!text.contains('\n') && !text.contains('\0'));
    let other = Engine::new()
        .compile("enum State;Ready;end;State::Ready")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("enum State;Ready;end;def run(x:State);x;end")
        .unwrap();
    let report = script
        .check_call("run", &[other], &CallOptions::default())
        .unwrap();
    assert!(
        report.diagnostics[0]
            .message
            .contains("different declaration"),
        "{report:?}"
    );
}

#[test]
fn call_failures_and_unresolved_contracts_have_actionable_messages() {
    for (source, needle) in [
        ("def run(x);x;end", "missing argument"),
        ("def run;missing();end", "undefined callable"),
        ("def run;to_int();end", "wrong number"),
        ("def run;/(/;end", "regular expression"),
        ("def run;yield;end", "No block"),
        ("def run(x:Missing=7);x;end", "Unknown type"),
        ("def run;\"abc\".center(5,extra:7);end", "center"),
        ("def run;x=1;x=[];x;end", "Reassignment"),
    ] {
        let report = check(source);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains(needle)),
            "{source}: {report:?}"
        );
    }
    for source in ["def run;(-1).chr;end", "def run;1.foo;end"] {
        let report = check(source);
        assert!(!report.is_clean());
        assert!(report.diagnostics.is_empty());
        assert!(!report.incomplete.is_empty());
    }
}

#[test]
fn general_scope_checks_defaults_and_all_declared_parameter_values() {
    for (source, argument) in [
        ("def run(x:int=false)->int;x;end", Value::int(7)),
        (
            "def run(x:bool)->int;if x;7;else;false;end;end",
            Value::boolean(true),
        ),
        (
            "def run(x:int)->int;if x>0;run(false);else;7;end;end",
            Value::int(0),
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions::default();
        assert!(
            script
                .check_call("run", std::slice::from_ref(&argument), &options)
                .unwrap()
                .is_clean()
        );
        assert_eq!(
            script
                .call("run", &[argument], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        let report = script.check_function("run", &options).unwrap();
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(
            report
                .diagnostics
                .iter()
                .all(|d| d.function == "run" && !d.code_frame.is_empty())
        );
    }
    let script = Engine::new()
        .compile("false+true;def unused->int;false;end;def run(x:int)->int;x+1;end")
        .unwrap();
    assert!(
        script
            .check_function("run", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    let script = Engine::new().compile("def run;yield;end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("No block"))
    );
}

#[test]
fn general_scope_resolves_initialized_types_and_variadic_collection_domains() {
    let script = Engine::new().compile("enum State;Ready;Done;end;module M;Math.store(:Status,State);end;def run(*items:array<Math.Status>,**extra:hash<symbol,Math.Status>)->array<array<State>>;[items,extra.values];end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    let result = script
        .call_with_keywords(
            "run",
            &[Value::symbol(b"ready".to_vec())],
            &[("next".into(), Value::symbol(b"done".to_vec()))],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.to_string(), "[[State::Ready], [State::Done]]");
    for source in [
        "def run(**extra)->array<string>;extra.keys;end",
        "def run(**extra:hash<symbol,int>?)->array<string>;extra.keys;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script
            .call_with_keywords(
                "run",
                &[],
                &[("next".into(), Value::int(7))],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.to_string(), "[next]");
    }
}

#[test]
fn general_scope_preserves_lazy_globals_strict_validation_and_factory_isolation() {
    let script = Engine::new()
        .compile("def run(x:int)->int;x+1;end")
        .unwrap();
    let options = CallOptions {
        globals: [("unused".into(), Value::bytes(vec![b'x'; 128 * 1024]))].into(),
        limits: Limits {
            memory_bytes: Some(48 * 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert!(script.check_function("run", &options).unwrap().is_clean());
    let options = CallOptions {
        capabilities: vec![Capability::new("sms", |_| {
            panic!("checker invoked factory")
        })],
        ..CallOptions::default()
    };
    let report = script.check_function("run", &options).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.incomplete.len(), 1);
    assert!(report.incomplete[0].message.contains("sms"));
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(x);x;end").unwrap();
    let options = CallOptions {
        globals: [(
            "unused".into(),
            HostMethod::new("send", |_, _, _| panic!("called")).value(),
        )]
        .into(),
        ..CallOptions::default()
    };
    assert_eq!(
        script.check_function("missing", &options).unwrap_err().kind,
        ErrorKind::Name
    );
    assert_eq!(
        script.check_function("run", &options).unwrap_err().kind,
        ErrorKind::Runtime
    );
}

#[test]
fn general_reports_obey_quotas_cancellation_and_deadlines_without_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let script = engine.compile("module M;effect();end;def run(flag:bool,x:int=effect())->int;if flag;false;else;effect();end;end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty());
    assert!(report.incomplete.is_empty());
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in [0, 1, 8, 15, 16] {
            let options = CallOptions {
                limits: Limits {
                    steps: (kind == ErrorKind::Steps).then_some(report.stats.steps * sample / 16),
                    memory_bytes: (kind == ErrorKind::Memory)
                        .then_some(report.stats.peak_memory_bytes * sample as usize / 16),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            let result = script.check_function("run", &options);
            if sample == 16 {
                assert_eq!(result.unwrap().diagnostics.len(), report.diagnostics.len());
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
            }
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut options = CallOptions::default();
        if kind == ErrorKind::Cancelled {
            options.cancellation.cancel();
        } else {
            options.deadline = Some(std::time::Instant::now());
        }
        assert_eq!(
            script.check_function("run", &options).unwrap_err().kind,
            kind
        );
    }
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn general_method_reports_select_declarations_and_preserve_constructor_rules() {
    let script = Engine::new().compile("class C;def initialize(@n:int)->int;'bad';end;private def read->int;'bad';end;def self.read->int;7;end;end;module M;module N;def self.answer->int;7;end;end;end;def unused->int;false;end").unwrap();
    for name in ["C.new", "C.read", "M::N.answer"] {
        let report = script
            .check_function(name, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{name}: {report:?}");
    }
    for (name, function) in [("C#initialize", "initialize"), ("C#read", "read")] {
        let report = script
            .check_function(name, &CallOptions::default())
            .unwrap();
        assert_eq!(report.diagnostics.len(), 1, "{name}: {report:?}");
        assert!(report.incomplete.is_empty(), "{name}: {report:?}");
        let diagnostic = &report.diagnostics[0];
        assert_eq!(diagnostic.function, function);
        assert!(diagnostic.message.contains("expected int, got string"));
        assert_eq!(diagnostic.position.line, 1);
        assert!(diagnostic.position.column > 1);
        assert!(diagnostic.code_frame.contains("'bad'"));
    }
    assert_eq!(
        script
            .check_function("C#missing", &CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    assert_eq!(
        script
            .check_call("C#read", &[], &CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
}

#[test]
fn general_method_checks_never_execute_constructors_initializers_or_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let method = HostMethod::new("tick", move |_, _, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap();
    let mut engine = Engine::new();
    engine.register_method("tick", method);
    let count = effects.clone();
    engine.set_output_writer(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let script = engine.compile("class C;K=tick();property n:int;def initialize;@n=tick();puts @n;end;def run(n:int=tick())->int;@n=n;puts @n;@n;end;def self.value->int;tick();end;end;def witness;C.new.run;end").unwrap();
    for name in ["C.new", "C#run", "C.value"] {
        let report = script
            .check_function(name, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{name}: {report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    let outcome = script.call("witness", &[], CallOptions::default()).unwrap();
    assert_eq!(outcome.value.as_int(), Some(7));
    assert_eq!(effects.load(Ordering::Relaxed), 5);
}

#[test]
fn checked_top_level_preserves_source_order_and_tracks_ambient_captures() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile("M.value+1;module M;effect();C=7;def self.value;C;end;end;def run;M.value+1;end")
        .unwrap();
    let CheckedOutcome::Rejected(report) = script
        .checked_call("__main__", &[], CallOptions::default())
        .unwrap()
    else {
        panic!("bad top-level call executed");
    };
    assert!(!report.diagnostics.is_empty());
    assert!(report.incomplete.is_empty());
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let outcome = executed(
        script
            .checked_call("run", &[], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(8));
    assert_eq!(effects.load(Ordering::Relaxed), 1);
    let script = engine
        .compile("module M;effect();C=7;def self.value;C;end;end;M.value+1")
        .unwrap();
    let outcome = executed(
        script
            .checked_call("__main__", &[], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(8));
    assert_eq!(effects.load(Ordering::Relaxed), 2);
    let script = engine
        .compile("n=7;module M;effect();C=n;end;M::C")
        .unwrap();
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 2);
    let outcome = executed(
        script
            .checked_call("__main__", &[], CallOptions::default())
            .unwrap(),
    );
    assert_eq!(outcome.value.as_int(), Some(7));
    assert_eq!(effects.load(Ordering::Relaxed), 3);
    let script = engine
        .compile("n=false;module M;effect();C=n+1;end;M::C")
        .unwrap();
    let CheckedOutcome::Rejected(report) = script
        .checked_call("__main__", &[], CallOptions::default())
        .unwrap()
    else {
        panic!("invalid ambient arithmetic executed");
    };
    assert!(!report.diagnostics.is_empty());
    assert!(report.incomplete.is_empty());
    assert_eq!(effects.load(Ordering::Relaxed), 3);
}

#[test]
fn opaque_left_equality_does_not_assume_native_boolean_results() {
    let options = CallOptions::default();
    // An opaque receiver may dispatch a source ==/!= returning any type, so the
    // checker must not prove the branch that returns a string unreachable.
    for source in [
        "class Plain;end;class C;def ==(other);7;end;end;def make;C.new;end;def run(x:any)->int;if (x==Plain.new).is_type?(:bool);7;else;'wrong';end;end",
        "class Plain;end;class C;def ==(other);7;end;end;def make;C.new;end;def run(x)->int;if (x==Plain).is_type?(:bool);7;else;'wrong';end;end",
        "class Plain;end;class C;def !=(other);7;end;end;def make;C.new;end;def run(x:any)->int;if (x != Plain.new).is_type?(:bool);7;else;'wrong';end;end",
        "class Plain;end;class C;def !=(other);7;end;end;def make;C.new;end;def run(x)->int;if (x != Plain).is_type?(:bool);7;else;'wrong';end;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script.check_function("run", &options).unwrap();
        assert!(!report.is_clean(), "{source}: {report:?}");
        let made = script.call("make", &[], options.clone()).unwrap().value;
        assert_eq!(
            script
                .call("run", &[made], options.clone())
                .unwrap_err()
                .kind,
            ErrorKind::Type,
            "{source}"
        );
    }
    // A missing != falls back to a negated ==, but the receiver's class is
    // unknown, so the result still must not be assumed boolean.
    let script = Engine::new()
        .compile("class Plain;end;class C;def ==(other);7;end;end;def run(x:any)->int;if (x != Plain.new).is_type?(:bool);7;else;'wrong';end;end")
        .unwrap();
    assert!(!script.check_function("run", &options).unwrap().is_clean());
    // Controls: a known native receiver with an unknown right operand keeps a
    // boolean result, and known instance identity and overrides are preserved.
    for source in [
        "class Plain;end;def run(x:any)->bool;Plain.new==x;end",
        "class Plain;end;def run(x)->bool;Plain.new != x;end",
        "class Plain;end;def run(x:any)->bool;Plain==x;end",
        "class Plain;end;def run(x)->bool;Plain != x;end",
        "class Plain;end;def run->bool;a=Plain.new;a==a;end",
        "class Plain;end;def run->bool;Plain.new != Plain.new;end",
        "class Plain;end;def run->bool;Plain==Plain;end",
        "class Plain;end;class C;def ==(other);7;end;end;def run->int;C.new==Plain.new;end",
        "class Plain;end;class C;def !=(other);7;end;end;def run->int;C.new != Plain;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script.check_function("run", &options).unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
    }
}

#[test]
fn primitive_equality_selects_typed_branches_without_rounding_or_nan_shortcuts() {
    for (expression, expected) in [
        ("1 == 1.0", true),
        ("1 != 1.0", false),
        ("9007199254740993 == 9007199254740992.0", false),
        ("9223372036854775807 == 9223372036854775808.0", false),
        ("(-9223372036854775807-1) == -9223372036854775808.0", true),
        ("-0.0 == 0.0", true),
        ("1.5 == 1.0", false),
        ("'a' == 'a'", true),
        ("'a' != 'b'", true),
        ("'a' == :a", false),
        (":a == :a", true),
        ("false == false", true),
        ("true != false", true),
        ("nil == nil", true),
        ("nil == 0", false),
        ("(1..3) == (1..3)", true),
        ("(1..3) == (1...3)", false),
        ("/a/i == /a/i", true),
        ("/a/i == /a/", false),
        ("money_cents(100,'USD') == 100", false),
        ("money_cents(100,'USD') == 100.0", false),
        ("Duration.build(1) == 1", false),
        ("Time.at(0) == 0", false),
    ] {
        equality_branch("", expression, expected, CallOptions::default());
    }
    let options = CallOptions {
        globals: [
            ("nan".into(), Value::float(f64::NAN)),
            ("infinity".into(), Value::float(f64::INFINITY)),
            ("bytes".into(), Value::bytes([0, 255, b'a'])),
            ("same".into(), Value::bytes([0, 255, b'a'])),
            ("different".into(), Value::bytes([0, 254, b'a'])),
        ]
        .into(),
        ..CallOptions::default()
    };
    for (expression, expected) in [
        ("nan == nan", false),
        ("nan != nan", true),
        ("nan == 7", false),
        ("infinity == infinity", true),
        ("infinity == 9223372036854775807", false),
        ("bytes == same", true),
        ("bytes == different", false),
    ] {
        equality_branch("", expression, expected, options.clone());
    }
}

#[test]
fn primitive_equality_keeps_abstract_alternatives_and_receiver_dispatch() {
    for source in [
        "def run(x:float)->int;if x==x;7;else;'wrong';end;end",
        "def run(x:string,y:string)->int;if x==y;7;else;'wrong';end;end",
        "def run(x:int)->int;if x==1.0;7;else;'wrong';end;end",
        "def run(x:int|float)->int;if x==1;7;else;'wrong';end;end",
    ] {
        let report = Engine::new()
            .compile(source)
            .unwrap()
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(
            report.incomplete.is_empty() && !report.diagnostics.is_empty(),
            "{source}: {report:?}"
        );
    }
    for left in ["1", "1.0", "'a'", "nil", "true", ":a"] {
        let source =
            format!("def run(x:any)->int;if ({left}==x).is_type?(:bool);7;else;'wrong';end;end");
        let script = Engine::new().compile(&source).unwrap();
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        for argument in [Value::nil(), Value::int(7), Value::array(vec![])] {
            assert_eq!(
                script
                    .call("run", &[argument], CallOptions::default())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
        }
    }
    let source = "class C;def ==(other);7;end;end;def run -> int; C.new=='a'; end";
    let script = Engine::new().compile(source).unwrap();
    assert!(
        script
            .check_call("run", &[], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}
