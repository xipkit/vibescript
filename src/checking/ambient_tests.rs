use super::entry::{self, Call, Check};
use super::scope_tests::top;
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Script, Signature,
    Value,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn check(ctx: &mut CallContext, script: &Script, options: &CallOptions) -> Result<Check> {
    entry::check(
        ctx,
        Call {
            script,
            name: "__main__",
            arguments: &[],
            keywords: &[],
            options,
        },
    )
}

#[test]
fn initializers_read_and_mutate_present_declaring_bindings() {
    for (source, expected) in [
        ("x=1;module M;x+=1;end;x", "2"),
        ("x=1;module M;Result=x;end;[x,M::Result]", "[1, 1]"),
        ("items=[1];module M;items.push(2);end;items", "[1, 2]"),
        (
            "items=[1];module M;[1].each{items.push(2)};end;items",
            "[1, 2]",
        ),
        (
            "x=1;module M;x+=1;module N;x+=1;end;Result=x;end;[x,M::Result]",
            "[3, 3]",
        ),
        (
            "x=1;C=7;module M;x=2;[3].each{x=3};D=x;E=C;C=9;end;[x,C,M.C,M.D,M.E]",
            "[2, 7, 9, 2, 7]",
        ),
        ("x=1;module M;Result=block_given?;end;M::Result", "false"),
    ] {
        top(source, expected, false);
    }
}

#[test]
fn ambient_updates_preserve_pending_addresses_and_value_copies() {
    for (source, expected) in [
        (
            "x=[2];module M;x[-1]+=begin;x.push(5);3;end;end;x",
            "[5, 5]",
        ),
        ("x=[2];module M;x[-1]+=begin;x=[9];3;end;end;x", "[9]"),
        ("x=[1];y=x;module M;x.push(2);end;[x,y]", "[[1, 2], [1]]"),
        (
            "x={items:[2]};module M;x[:items][-1]+=5;end;x[:items]",
            "[7]",
        ),
        (
            "x=[1];module M;Result=x+x.push(2);end;[x,M::Result]",
            "[[1, 2], [1, 1, 2]]",
        ),
    ] {
        top(source, expected, false);
    }
}

#[test]
fn ambient_state_survives_rescue_ensure_and_early_initializer_return() {
    for (source, expected, issues) in [
        (
            "x=1;module M;begin;x=7;raise 'x';rescue;nil;end;end;x",
            "7",
            false,
        ),
        (
            "x=1;module M;begin;x=7;1/0;rescue;x+=1;ensure;x+=2;end;end;x",
            "10",
            false,
        ),
        ("x=1;module M;x=7;return 99;x=8;end;x", "7", false),
        (
            "x=1;module M;x+=1;begin;[2].each{x+=1};rescue;nil;end;end;x",
            "2",
            true,
        ),
        (
            "x=1;module M;begin;[2].each{return 7};rescue;x=8;end;end;x",
            "8",
            false,
        ),
    ] {
        top(source, expected, issues);
    }
}

#[test]
fn ambient_bindings_follow_blocks_without_becoming_method_captures() {
    for (source, expected, issues) in [
        (
            "def twice;yield;yield;end;x=[1];module M;twice{[2].each{|n|x.push(n)}};end;x",
            "[1, 2, 2]",
            false,
        ),
        (
            "def outer;inner{yield};end;def inner;yield;end;x=[1];module M;outer{x.push(2)};end;x",
            "[1, 2]",
            false,
        ),
        (
            "def f;yield;end;x=1;module M;f{x=7};D=x;end;[x,M.D]",
            "[1, 1]",
            false,
        ),
        (
            "x=1;module M;def self.get;x;end;end;begin;M.get;rescue;7;end",
            "7",
            true,
        ),
        (
            "x=1;module M;D=[3].map{|x|x+1};E=x;end;[x,M.D,M.E]",
            "[1, [4], 1]",
            false,
        ),
    ] {
        top(source, expected, issues);
    }
}

#[test]
fn ambient_type_bindings_resolve_in_initializer_blocks() {
    for (source, expected) in [
        (
            "enum Choice;Yes;No;end;t=Choice;module M;Result=[:yes].map{|x:t|x};end;M::Result",
            "[Choice::Yes]",
        ),
        (
            "enum Choice;Yes;No;end;t=Choice;module M;Result=[1].map{[:yes].map{|x:t|x}};end;M::Result",
            "[[Choice::Yes]]",
        ),
        (
            "enum Choice;Yes;No;end;t=Choice;module M;Result=Choice::Yes.is_type?(:T);end;M::Result",
            "false",
        ),
        (
            "enum A;Yes;end;enum B;No;end;t=A;module M;t=B;Result=[:no].map{|x:t|x};end;[t==B,M::Result]",
            "[true, [B::No]]",
        ),
    ] {
        top(source, expected, false);
    }
}

#[test]
fn absent_declaring_bindings_stay_local_and_named_calls_skip_top_level_state() {
    top(
        "module M;x=7;Result=x;end;x=9;[x,M::Result]",
        "[9, 7]",
        false,
    );
    top(
        "x=nil;module M;x=7;Result=x;end;[x,M::Result]",
        "[7, 7]",
        false,
    );
    let script = Engine::new()
        .compile("x=7;module M;Result=x;end;def run;M::Result;end")
        .unwrap();
    assert!(
        script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    for report in [
        script
            .check_call("run", &[], &CallOptions::default())
            .unwrap(),
        script
            .check_function("run", &CallOptions::default())
            .unwrap(),
    ] {
        assert!(!report.diagnostics.is_empty(), "{report:?}");
        assert!(report.incomplete.is_empty(), "{report:?}");
    }
    assert!(script.call("run", &[], CallOptions::default()).is_err());
}

#[test]
fn ambient_views_keep_recursive_top_level_invocations_separate() {
    top(
        "x=1;module M;x+=1;Result=x;end;if Math[:again];x;else;Math[:again]=true;[__main__(),x,M::Result];end",
        "[1, 2, 2]",
        false,
    );
    top(
        "x=[2];module M;x[-1]+=[1].map{x.push(5);3}.first;end;x",
        "[5, 5]",
        false,
    );
    top(
        "x=1;module M;begin;x+=1;raise 'retry' if x<3;rescue;retry;ensure;x+=1;end;end;x",
        "4",
        false,
    );
}

#[test]
fn ambient_host_blocks_keep_sticky_breaks_without_analysis_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let method = HostMethod::new_with_block("visit", move |host, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
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
    let script = Engine::new()
        .compile(
            "items=[];module M;Result=driver.visit{items.push(1);break 7};end;[items,M::Result]",
        )
        .unwrap();
    let options = CallOptions {
        globals: [("driver".into(), driver)].into(),
        ..CallOptions::default()
    };
    for iteration in 0..2 {
        assert!(
            script
                .check_call("__main__", &[], &options)
                .unwrap()
                .is_clean()
        );
        assert_eq!(calls.load(Ordering::Relaxed), iteration);
        let crate::CheckedOutcome::Executed(outcome) = script
            .checked_call("__main__", &[], options.clone())
            .unwrap()
        else {
            panic!("valid initializer rejected");
        };
        assert_eq!(outcome.value.to_string(), "[[1], 7]");
        assert_eq!(calls.load(Ordering::Relaxed), iteration + 1);
    }
}

#[test]
fn ambient_views_keep_supplied_roots_lazy_and_isolated() {
    let original = Value::array(vec![Value::int(1)]);
    let options = CallOptions {
        globals: [
            ("x".into(), original.clone()),
            ("unused".into(), Value::bytes(vec![b'x'; 128 * 1024])),
        ]
        .into(),
        limits: Limits {
            memory_bytes: Some(96 * 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("y=7;module M;x.push(y);end;x")
        .unwrap();
    for _ in 0..2 {
        let report = script.check_call("__main__", &[], &options).unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            script.run(options.clone()).unwrap().value.to_string(),
            "[1, 7]"
        );
        assert_eq!(original.to_string(), "[1]");
    }
}

#[test]
fn ambient_capture_analysis_obeys_quotas_cancellation_and_cleanup() {
    for source in [
        "x=[2];module M;x[-1]+=[1].map{x.push(5);3}.first;end;x",
        "enum A;Yes;end;t=A;module M;Result=[1].map{[:yes].map{|x:t|x}};end;M::Result",
        "x=1;module M;begin;x=7;raise 'x';rescue;x+=1;ensure;x+=2;end;end;x",
        "x=[1];module M;Result=x+x.push(2);end;[x,M::Result]",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions::default();
        let mut ctx = CallContext::new(options.clone());
        let checked = check(&mut ctx, &script, &options).unwrap();
        assert!(
            checked.analysis.incomplete.data.is_empty(),
            "{source}: {checked:?}"
        );
        assert!(
            checked.analysis.issues.data.is_empty(),
            "{source}: {checked:?}"
        );
        let stats = ctx.stats();
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            for sample in [0, 1, 8, 15, 16] {
                let options = CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(stats.steps * sample / 16),
                        memory_bytes: (kind == ErrorKind::Memory)
                            .then_some(stats.peak_memory_bytes * sample as usize / 16),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                };
                let mut ctx = CallContext::new(options.clone());
                let result = check(&mut ctx, &script, &options);
                if sample == 16 {
                    drop(result.unwrap());
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
            assert_eq!(check(&mut ctx, &script, &options).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
