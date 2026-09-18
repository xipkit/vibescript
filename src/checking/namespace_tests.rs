use super::{
    entry::{self, Call, Check},
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Script, Signature,
    Value,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn check(
    ctx: &mut CallContext,
    script: &Script,
    args: &[Value],
    options: &CallOptions,
) -> Result<Check> {
    entry::check(
        ctx,
        Call {
            script,
            name: "run",
            arguments: args,
            keywords: &[],
            options,
        },
    )
}

fn witness(script: &Script, args: &[Value], options: &CallOptions, expected: &str, issues: bool) {
    let mut ctx = CallContext::new(options.clone());
    let mut checked = check(&mut ctx, script, args, options).unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert_eq!(
        !checked.analysis.issues.data.is_empty(),
        issues,
        "{checked:?}"
    );
    let actual = script.call("run", args, options.clone()).unwrap().value;
    assert_eq!(actual.to_string(), expected);
    let actual = observed(
        &mut ctx,
        &mut checked.facts,
        &script.inner.code.program,
        &actual,
    );
    assert_ne!(
        checked
            .facts
            .relation(&mut ctx, actual, checked.analysis.returns)
            .unwrap(),
        Relation::Rejected,
        "{checked:?}"
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn run(source: &str, expected: &str) {
    let script = Engine::new().compile(source).unwrap();
    witness(&script, &[], &CallOptions::default(), expected, false);
}

#[test]
fn static_methods_bind_defaults_keywords_rest_and_typed_returns() {
    for kind in ["module", "class"] {
        for call in ["M.apply(3,4,5,extra:6)", "(M.apply)(3,4,5,extra:6)"] {
            run(
                &format!(
                    "{kind} M; def self.apply(x:int,y=2,*rest,extra:1)->array<int>;[x,y,rest.sum,extra];end;end;def run;{call};end"
                ),
                "[3, 4, 5, 6]",
            );
        }
        for call in ["M.answer", "M.answer()", "(M.answer)()", "M&.answer"] {
            run(
                &format!("{kind} M; def self.answer(x=7);x;end;end;def run;{call};end"),
                "7",
            );
        }
    }
}

#[test]
fn namespace_methods_override_native_names_and_carry_blocks() {
    for method in [
        "map", "push", "clear", "sort", "gsub", "nil?", "itself", "dup",
    ] {
        for call in [
            format!("M.{method}(3){{|n|n+1}}"),
            format!("(M.{method})(3){{|n|n+1}}"),
        ] {
            run(
                &format!("module M;def self.{method}(n);yield(n*2);end;end;def run;{call};end"),
                "7",
            );
        }
    }
    run(
        "module M;def self.values;[1];end;end;def run;M.values.push(2);M.values;end",
        "[1]",
    );
    run(
        "module M;def self.clear;[2];end;end;def run;M.clear.push(3);end",
        "[2, 3]",
    );
}

#[test]
fn implicit_calls_and_lexical_blocks_keep_namespace_visibility() {
    run(
        "module Other;def self.each;yield;end;def self.hidden;9;end;end;module M;private def self.hidden;7;end;public def self.check;Other.each{hidden};end;end;def run;M.check;end",
        "7",
    );
    for call in [
        "hidden",
        "hidden()",
        "(hidden)()",
        "[1].map{hidden}[0]",
        "self.visible",
    ] {
        run(
            &format!(
                "module M;private def self.hidden;7;end;protected def self.visible;hidden;end;public def self.answer;{call};end;end;def run;M.answer;end"
            ),
            "7",
        );
    }
    for call in [
        "M.hidden",
        "M.hidden()",
        "(M.hidden)()",
        "M.visible",
        "M::hidden",
        "self.hidden",
    ] {
        let call = if call == "self.hidden" {
            "M.answer"
        } else {
            call
        };
        let source = format!(
            "module M;private def self.hidden;7;end;protected def self.visible;8;end;public def self.answer;self.hidden;end;end;def run;begin;{call};rescue RuntimeError;9;end;end"
        );
        let script = Engine::new().compile(&source).unwrap();
        witness(&script, &[], &CallOptions::default(), "9", true);
    }
    let script = Engine::new().compile("module A;protected def self.hidden;7;end;end;module B;def self.answer;A.hidden;end;end;def run;begin;B.answer;rescue RuntimeError;9;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "9", true);
}

#[test]
fn aliases_arguments_and_returned_namespace_values_preserve_dispatch_identity() {
    run(
        "module A;def self.push(n);1;end;end;module B;def self.push(n);2;end;end;def run;m=A;value=m.push(begin\nm=B;0\nend);[value,m.push(0)];end",
        "[1, 2]",
    );
    run(
        "module A;def self.answer(n);1;end;end;module B;def self.answer(n);2;end;end;def run;m=A;value=(m.answer rescue missing)(begin\nm=B;0\nend);[value,m.answer(0)];end",
        "[1, 2]",
    );
    run(
        "enum E;a;b;end;module M;def self.answer;7;end;def self.identity;self;end;end;def take(m);m.answer;end;def run;m=M;m=M.identity;take([m][0]);end",
        "7",
    );
    for flag in [false, true] {
        let mut engine = Engine::new();
        engine.register("choose", move |_, _| Ok(Value::boolean(flag)));
        let script = engine.compile("module A;def self.answer(x);x+1;end;end;class B;def self.answer(x);x+2;end;end;def run;m=if choose();A;else;B;end;m.answer(3);end").unwrap();
        witness(
            &script,
            &[],
            &CallOptions::default(),
            if flag { "4" } else { "5" },
            false,
        );
    }
}

#[test]
fn static_calls_preserve_global_effects_and_block_control() {
    let source = "module M;def self.change;count+=1;yield(count);end;end;def run;M.change{|n|count+=2;break n};count;end";
    let script = Engine::new().compile(source).unwrap();
    let options = CallOptions {
        globals: [("count".into(), Value::int(1))].into(),
        ..CallOptions::default()
    };
    witness(&script, &[], &options, "4", false);
    for (body, expected) in [("break 7", "8"), ("return 7", "7")] {
        run(
            &format!("module M;def self.each;yield;99;end;end;def run;M.each{{{body}}};8;end"),
            expected,
        );
    }
    run(
        "module M;def self.answer(n:int);if n==0;7;else;answer(n-1);end;end;end;def run;M.answer(4);end",
        "7",
    );
}

#[test]
fn source_and_host_roots_precede_implicit_methods() {
    run(
        "def answer;9;end;module M;def self.answer;7;end;def self.check;[answer,answer()];end;end;def run;M.check;end",
        "[9, 9]",
    );
    let script = Engine::new()
        .compile("module M;def self.answer;7;end;def self.check;answer;end;end;def run;M.check;end")
        .unwrap();
    let options = CallOptions {
        globals: [("answer".into(), Value::int(11))].into(),
        ..CallOptions::default()
    };
    witness(&script, &[], &options, "11", false);
    let script = Engine::new()
        .compile("module M;def self.answer;7;end;end;def run;M.answer;end")
        .unwrap();
    let options = CallOptions {
        globals: [(
            "M".into(),
            Value::object(vec![(b"answer".to_vec(), Value::int(12))]),
        )]
        .into(),
        ..CallOptions::default()
    };
    witness(&script, &[], &options, "12", false);
}

#[test]
fn nested_namespaces_use_declaration_identity_and_constant_precedence() {
    for access in ["M.N.answer", "M::N.answer", "M::N::D.answer", "M.check"] {
        run(
            &format!(
                "enum E;a;b;end;module M;module N;def self.answer;7;end;module D;def self.answer;7;end;end;end;def self.check;N.answer;end;end;def run;{access};end"
            ),
            "7",
        );
    }
    let script = Engine::new().compile("module M;module N;def self.answer;7;end;end;def self.check;N.answer;end;end;def run;M.check;end").unwrap();
    let options = CallOptions {
        globals: [("N".into(), Value::int(99))].into(),
        ..CallOptions::default()
    };
    witness(&script, &[], &options, "7", false);
    let mut deep = Value::int(1);
    for _ in 0..129 {
        deep = Value::array(vec![deep]);
    }
    for call in ["N()", "(N rescue missing)()"] {
        let script = Engine::new()
            .compile(&format!(
                "module M;module N;end;def self.check;{call};end;end;def run;M.check;end"
            ))
            .unwrap();
        let options = CallOptions {
            globals: [("N".into(), deep.clone())].into(),
            ..CallOptions::default()
        };
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(!report.diagnostics.is_empty(), "{report:?}");
        assert_ne!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Recursion
        );
    }
}

#[test]
fn namespace_host_blocks_preserve_ignored_break_and_return() {
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
    for (transfer, expected) in [("break 7", "[7, [1]]"), ("return 9", "9")] {
        let script = Engine::new().compile(&format!("module M;def self.check(driver);a=[];n=driver.visit{{a.push(1);{transfer}}};[n,a];end;end;def run(driver);M.check(driver);end")).unwrap();
        witness(
            &script,
            std::slice::from_ref(&driver),
            &CallOptions::default(),
            expected,
            false,
        );
    }
}

#[test]
fn method_shape_type_and_return_errors_have_runtime_witnesses() {
    for (method, call, kind) in [
        (
            "def self.answer(x:int);x;end",
            "M.answer(\"bad\")",
            ErrorKind::Type,
        ),
        ("def self.answer(x);x;end", "M.answer", ErrorKind::Argument),
        ("def self.answer;7;end", "M.answer(1)", ErrorKind::Argument),
        (
            "def self.answer->int;\"bad\";end",
            "M.answer",
            ErrorKind::Type,
        ),
        ("def self.answer;7;end", "M.missing", ErrorKind::Name),
        ("def self.answer;7;end", "M::answer", ErrorKind::Name),
        ("def self.answer;7;end", "M.new", ErrorKind::Argument),
    ] {
        let source = format!("module M;{method};end;def run;{call};end");
        let script = Engine::new().compile(&source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .kind,
            kind,
            "{source}"
        );
    }
}

#[test]
fn computed_lookup_fails_before_arguments_and_errors_stay_catchable() {
    for (call, expected) in [
        ("(begin\nM.hidden\nend)(a.push(1))", "[]"),
        ("(M.hidden rescue missing)(a.push(1))", "[]"),
        ("(M.hidden)(a.push(1))", "[1]"),
        ("M.hidden(a.push(1))", "[1]"),
    ] {
        let script = Engine::new().compile(&format!("module M;private def self.hidden(x);x;end;end;def run;a=[];begin;{call};rescue RuntimeError;nil;end;a;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), expected, true);
    }
    for call in ["new(a.push(1))", "(new rescue missing)(a.push(1))"] {
        let script = Engine::new().compile(&format!("module M;def self.check;a=[];begin;{call};rescue RuntimeError;[7,a];end;end;end;def run;M.check;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "[7, []]", true);
    }
}

#[test]
fn checks_never_execute_method_bodies_or_host_callbacks() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let script = engine
        .compile("module M;def self.answer(x=effect());x;end;end;def run;M.answer;end")
        .unwrap();
    assert!(
        script
            .check_call("run", &[], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn initializers_state_instances_and_foreign_namespaces_remain_incomplete() {
    for source in [
        "module M;C=7;def self.answer;C;end;end;def run;M.answer;end",
        "module M;def self.answer;@@count=7;end;end;def run;M.answer;end",
        "module M;def self.answer;@@count;end;end;def run;M.answer;end",
        "class M;def answer;7;end;end;def run;M.new.answer;end",
        "module M;end;def run;M.respond_to?(:missing);end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(!report.incomplete.is_empty(), "{source}: {report:?}");
    }
    let foreign = Engine::new()
        .compile("module M;def self.answer;7;end;end;M")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new().compile("def run(m);m.answer;end").unwrap();
    assert!(
        !script
            .check_call("run", &[foreign], &CallOptions::default())
            .unwrap()
            .incomplete
            .is_empty()
    );
}

#[test]
fn static_dispatch_is_metered_interruptible_and_releases_facts() {
    let source = "module M;def self.answer(x:int);[x+1,x+2];end;end;def run;M.answer(3);end";
    let script = Engine::new().compile(source).unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(options.clone());
    let checked = check(&mut ctx, &script, &[], &options).unwrap();
    assert!(checked.analysis.incomplete.data.is_empty());
    assert!(checked.analysis.issues.data.is_empty());
    let stats = ctx.stats();
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        let mut options = CallOptions::default();
        if kind == ErrorKind::Steps {
            options.limits.steps = Some(stats.steps - 1);
        } else {
            options.limits.memory_bytes = Some(stats.peak_memory_bytes - 1);
        }
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            check(&mut ctx, &script, &[], &options).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for sample in 0..=16 {
            let limits = Limits {
                steps: (kind == ErrorKind::Steps).then_some(stats.steps * sample / 16),
                memory_bytes: (kind == ErrorKind::Memory)
                    .then_some(stats.peak_memory_bytes * sample as usize / 16),
                ..Limits::default()
            };
            let options = CallOptions {
                limits,
                ..CallOptions::default()
            };
            let mut ctx = CallContext::new(options.clone());
            let result = check(&mut ctx, &script, &[], &options);
            if sample == 16 {
                assert!(result.is_ok(), "{result:?}");
                drop(result);
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut options = CallOptions::default();
        if kind == ErrorKind::Cancelled {
            options.cancellation.cancel();
        } else {
            options.deadline = Some(std::time::Instant::now());
        }
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            check(&mut ctx, &script, &[], &options).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
