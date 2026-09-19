use crate::{CallOptions, Engine, Script, ScriptInner, Value};
use std::sync::Arc;

fn file(engine: &Engine, source: &str) -> Script {
    Script {
        inner: Arc::new(ScriptInner {
            code: crate::code::Code::compile_file(source, &engine.hosts).unwrap(),
            loader: engine.loader.clone(),
            strict_effects: engine.strict_effects,
            random_source: engine.random_source.clone(),
            output_writer: engine.output_writer.clone(),
            error_writer: engine.error_writer.clone(),
        }),
    }
}

fn witness(source: &str, expected: i64) {
    let script = file(&Engine::new(), source);
    let outcome = script
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error:?}"));
    assert_eq!(outcome.value.as_int(), Some(expected), "{source}");
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{source}: {report:?}");
}

#[test]
fn private_file_bindings_survive_calls_and_parameter_shadowing() {
    for (source, expected) in [
        ("x=7;def read->int;x;end;read()", 7),
        ("x=7;def read(x:int)->int;x+1;end;read(9)+x", 17),
        ("x=7;def bump;x+=1;end;bump();x", 8),
        (
            "def read->int;7;end;def replace;read=9;end;replace();read",
            9,
        ),
        ("x=[1];def bump;x.push(2);end;bump();x.length", 2),
        ("x=1;[2].each{|n|x+=n};x", 3),
        ("x=1;def bump;[2].each{|n|x+=n};end;bump();x", 3),
        ("x=0;for i in 1..3;x+=i;end;x+i", 9),
        ("if false;x=7;end;def read;x;end;read().nil? ? 1 : 0", 1),
    ] {
        witness(source, expected);
    }
}

#[test]
fn private_file_return_errors_are_visible_without_running_the_file() {
    let script = file(&Engine::new(), "x=false;def read->int;x;end;read()");
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(script.run(CallOptions::default()).is_err());
}

#[test]
fn file_local_writes_do_not_replace_supplied_roots() {
    let script = file(&Engine::new(), "count+=1;def read->int;count;end;read()");
    let options = CallOptions {
        globals: [("count".into(), Value::int(7))].into(),
        ..CallOptions::default()
    };
    let outcome = script.run(options.clone()).unwrap();
    assert_eq!(outcome.value.as_int(), Some(8));
    let report = script.check_call("__main__", &[], &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(options.globals["count"].as_int(), Some(7));
}

#[test]
fn file_mutations_keep_snapshots_pending_targets_and_cleanup() {
    for (source, expected) in [
        (
            "x=[1];old=x;def append;x.push(2);end;append();old.length*10+x.length",
            12,
        ),
        (
            "x={items:[1]};def append;x.items.push(2);end;append();x.items.length",
            2,
        ),
        (
            "x=[1];def append;x.push(2);3;end;x[0]+=append();x[0]*10+x.length",
            42,
        ),
        (
            "x=[1];def append;x.push(2);3;end;x[-1]+=append();x[0]*10+x[1]",
            42,
        ),
        ("x=[1];def replace;x=[9];3;end;x[0]+=replace();x[0]", 9),
        (
            "x=1;def change;begin;x=7;return 2;ensure;x+=1;end;end;change();x",
            8,
        ),
        (
            "x=1;def change;begin;x=false;raise 'bad';rescue;x=7;end;end;change();x",
            7,
        ),
        (
            "x=0;def change;begin;x+=1;raise 'again' if x<2;rescue;retry;end;end;change();x",
            2,
        ),
        ("x=1;def once;yield;end;once{x=7};x", 7),
        ("x=1;def change;[2].each{x=7;break};end;change();x", 7),
    ] {
        witness(source, expected);
    }
}

#[test]
fn file_binding_presence_keeps_function_fallbacks_and_call_rejections() {
    for (tail, clean) in [("helper", true), ("helper()", false)] {
        let source = format!(
            "def helper->int;9;end;def run(flag:bool)->int;if flag;helper=7;end;{tail};end"
        );
        let script = file(&Engine::new(), &source);
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
        for flag in [true, false] {
            let result = script.call("run", &[Value::boolean(flag)], CallOptions::default());
            if clean || !flag {
                assert_eq!(
                    result.unwrap().value.as_int(),
                    Some(if flag { 7 } else { 9 })
                );
            } else {
                assert!(result.is_err());
            }
        }
    }
}

#[test]
fn file_declarations_and_calls_do_not_materialize_shadowed_roots() {
    let foreign = Engine::new()
        .compile("class Foreign;end;Foreign")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    for (source, name) in [
        ("def answer;7;end;answer()", "answer"),
        ("enum E;A;end;E::A.to_s.length", "E"),
        ("enum E;A;end;def accept(x:E)->E;x;end;accept(E::A)", "E"),
        ("class C;def value;7;end;end;C.new.value", "C"),
        ("class C;end;def accept(x:C)->C;x;end;accept(C.new)", "C"),
    ] {
        let script = file(&Engine::new(), source);
        let options = CallOptions {
            globals: [(name.into(), foreign.clone())].into(),
            ..CallOptions::default()
        };
        assert!(script.run(options.clone()).is_ok(), "{source}");
        let report = script.check_call("__main__", &[], &options).unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
    }
}

#[test]
fn file_variables_remain_private_to_their_lexical_owner() {
    for source in [
        "def create;fresh=7;end;def read;fresh;end;create();read()",
        "[1].each{fresh=7};fresh",
        "x=7;def read->int;x;end",
    ] {
        let script = file(&Engine::new(), source);
        let function = if source.ends_with("end") {
            "read"
        } else {
            "__main__"
        };
        let report = script
            .check_call(function, &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        assert!(
            script.call(function, &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn file_builtin_rebinding_and_namespace_methods_use_private_state() {
    for (source, expected) in [
        ("Math={PI:7};def read->int;Math.PI;end;read()", 7),
        ("def replace;Math={PI:7};end;replace();Math.PI", 7),
        (
            "Math.data=[1];def append;Math.data.push(2);end;append();Math.data.length",
            2,
        ),
        ("x=7;module M;def self.read->int;x;end;end;M.read", 7),
        ("x=7;class C;def read->int;x;end;end;C.new.read", 7),
        ("x=7;module M;def self.change;x+=1;end;end;M.change;x", 8),
    ] {
        witness(source, expected);
    }
}

#[test]
fn file_type_aliases_follow_private_state_at_source_and_host_boundaries() {
    use crate::{HostMethod, Signature, SignatureParam};
    let method = HostMethod::new("echo", |_, args, _| Ok(args[0].clone()))
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "value".into(),
                ty: "Alias".into(),
                optional: false,
            }],
            result: "Alias".into(),
            accepts_block: false,
        })
        .unwrap();
    let mut engine = Engine::new();
    engine.register_method("echo", method);
    for source in [
        "enum E;A;end;Alias=E;def accept(x:Alias)->Alias;x;end;accept(E::A)",
        "enum E;A;end;Alias=E;echo(E::A)",
        "enum E;A;end;Alias=E;[E::A].map{|x:Alias|x}.first",
    ] {
        let script = file(&engine, source);
        assert!(script.run(CallOptions::default()).is_ok(), "{source}");
        let report = script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
    }
    for source in [
        "enum Alias;A;end;Alias=7;echo(:a)",
        "enum Alias;A;end;def replace;Alias=7;end;replace();echo(:a)",
    ] {
        let script = file(&engine, source);
        assert!(script.run(CallOptions::default()).is_err(), "{source}");
        let report = script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    }
}

#[test]
fn file_rescue_bindings_are_temporary_and_skipped_assignments_remain_visible() {
    witness(
        "error=7;begin;raise 'bad';skipped=9;rescue=>error;seen=error.message.length;rescue_only=11;ensure;ensured=13;end;def read;error+seen+rescue_only+ensured+(skipped.nil? ? 1 : 0);end;read()",
        35,
    );
    for flag in ["true", "false"] {
        witness(
            &format!(
                "x=1;def run(flag);begin;if flag;x=7;end;return;ensure;x+=1;end;end;run({flag});x"
            ),
            if flag == "true" { 8 } else { 2 },
        );
    }
}

#[test]
fn file_namespace_constants_parameters_and_initializer_blocks_keep_their_scopes() {
    for (source, expected) in [
        (
            "DATA=[1];module M;DATA=[2,3];def self.read;DATA.size;end;def self.parameter(DATA);DATA.size;end;end;M.read*10+M.parameter([4,5,6])",
            23,
        ),
        ("module M;Result=[2].map{|n|x=n+1;x}.first;end;M::Result", 3),
        (
            "module M;x=7;Result=[2].map{|n|x=n+1}.first;After=x;end;M::After*10+M::Result",
            33,
        ),
    ] {
        witness(source, expected);
    }
    let source = "module M;[1].each{x=7};def self.read;x;end;end;M.read";
    let script = file(&Engine::new(), source);
    assert!(script.run(CallOptions::default()).is_err());
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(!report.diagnostics.is_empty(), "{report:?}");
}

#[test]
fn file_whole_and_named_checks_use_their_own_initialization_scope() {
    let script = file(&Engine::new(), "x=7;def read->int;x;end");
    let whole = script.check(&CallOptions::default()).unwrap();
    assert!(whole.is_clean(), "{whole:?}");
    let named = script
        .check_function("read", &CallOptions::default())
        .unwrap();
    assert!(named.incomplete.is_empty(), "{named:?}");
    assert!(!named.diagnostics.is_empty(), "{named:?}");
    let script = file(&Engine::new(), "x=false;def unused->int;x;end;7");
    let exact = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(exact.is_clean(), "{exact:?}");
    let whole = script.check(&CallOptions::default()).unwrap();
    assert!(whole.incomplete.is_empty(), "{whole:?}");
    assert!(!whole.diagnostics.is_empty(), "{whole:?}");
}

#[test]
fn checking_private_state_never_runs_host_callbacks_and_checked_calls_stay_isolated() {
    use crate::{CheckedOutcome, HostMethod, Signature};
    use std::sync::atomic::{AtomicUsize, Ordering};
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
    let mut engine = Engine::new();
    engine.register_method("visit", method);
    let script = file(
        &engine,
        "items=[];def run;visit{items.push(1);break 7};end;result=run();[items,result]",
    );
    for iteration in 0..2 {
        let report = script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(calls.load(Ordering::Relaxed), iteration);
        let CheckedOutcome::Executed(outcome) = script
            .checked_call("__main__", &[], CallOptions::default())
            .unwrap()
        else {
            panic!("valid file rejected");
        };
        assert_eq!(outcome.value.to_string(), "[[1], 7]");
        assert_eq!(calls.load(Ordering::Relaxed), iteration + 1);
    }
}

#[test]
fn private_type_aliases_observe_rebinding_and_ambiguous_names() {
    for source in [
        "enum E;A;end;Alias=E;def replace;Alias=7;end;def accept(x:Alias);x;end;replace();accept(:a)",
        "enum E;A;end;enum F;B;end;Alias=E;ALIAS=F;def accept(x:alias);x;end;accept(:a)",
        "enum E;A;end;Alias=E;def accept(x:Alias);x;end;accept(:nope)",
        "enum E;A;end;def replace;E=7;end;def accept(x:E);x;end;replace();accept(:a)",
        "class C;end;def replace;C=7;end;def accept(x:C);x;end;replace();accept(nil)",
    ] {
        let script = file(&Engine::new(), source);
        assert!(script.run(CallOptions::default()).is_err(), "{source}");
        let report = script
            .check_call("__main__", &[], &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    }
    witness(
        "enum E;A;end;Alias=E;def accept(x:Alias);x.is_type?(:E) ? 1 : 0;end;accept(:a)",
        1,
    );
}

#[test]
fn file_analysis_obeys_exact_and_interrupted_limits_and_reclaims_scratch() {
    use super::entry::{self, Call};
    use crate::{CallContext, ErrorKind, Limits};
    let long = "private_name".repeat(512);
    for source in [
        "x=[1];def change;x[0]+=begin;x.push(2);3;end;end;change();x".to_owned(),
        "x=1;def change;begin;x=7;return 2;rescue;nil;ensure;x+=1;end;end;change();x".to_owned(),
        "enum E;A;end;Alias=E;def accept(x:Alias);x;end;[E::A].map{|x:Alias|accept(x)}".to_owned(),
        format!("{long}=1;def read;{long}+1;end;read()"),
    ] {
        let script = file(&Engine::new(), &source);
        let check = |ctx: &mut CallContext, options: &CallOptions| {
            entry::check(
                ctx,
                Call {
                    script: &script,
                    name: "__main__",
                    arguments: &[],
                    keywords: &[],
                    options,
                },
            )
        };
        let options = CallOptions::default();
        let mut ctx = CallContext::new(options.clone());
        let checked = check(&mut ctx, &options).unwrap();
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
            for sample in [0, 1, 4, 8, 12, 15, 16, 17] {
                let exact = sample == 16;
                let steps = if sample == 17 {
                    stats.steps - 1
                } else {
                    stats.steps * sample / 16
                };
                let memory = if sample == 17 {
                    stats.peak_memory_bytes - 1
                } else {
                    stats.peak_memory_bytes * sample as usize / 16
                };
                let options = CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(steps),
                        memory_bytes: (kind == ErrorKind::Memory).then_some(memory),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                };
                let mut ctx = CallContext::new(options.clone());
                let result = check(&mut ctx, &options);
                if exact {
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
            assert_eq!(check(&mut ctx, &options).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn conditional_file_builtins_keep_present_and_absent_paths() {
    for (source, clean, both_valid) in [
        (
            "def run(flag:bool);if flag;format=7;end;format('%d',1);end",
            false,
            false,
        ),
        (
            "def run(flag:bool);if flag;Math={PI:7};end;Math.PI+1;end",
            true,
            true,
        ),
        (
            "def run(flag:bool);if flag;Math={data:[1]};else;Math.data=[2];end;Math.data.push(3);Math.data.length;end",
            false,
            true,
        ),
        (
            "def helper;7;end;def run(flag:bool);begin;if flag;helper=9;end;return 2;ensure;helper+1;end;end",
            true,
            true,
        ),
    ] {
        let script = file(&Engine::new(), source);
        for flag in [true, false] {
            let result = script.call("run", &[Value::boolean(flag)], CallOptions::default());
            assert_eq!(
                result.is_ok(),
                both_valid || !flag,
                "{source}, {flag}: {result:?}"
            );
            let exact = script
                .check_call("run", &[Value::boolean(flag)], &CallOptions::default())
                .unwrap();
            assert!(exact.incomplete.is_empty(), "{source}: {exact:?}");
            assert_eq!(exact.is_clean(), both_valid || !flag, "{source}: {exact:?}");
        }
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
    }
}

#[test]
fn missing_required_files_are_diagnosed_and_captured_foreign_bindings_are_analyzed() {
    let script = file(&Engine::new(), "require('dependency')");
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("module paths not configured")),
        "{report:?}"
    );
    let foreign = file(&Engine::new(), "x=7;module M;def self.read;x;end;end;M")
        .run(CallOptions::default())
        .unwrap()
        .value;
    let script = file(&Engine::new(), "foreign.read");
    let options = CallOptions {
        globals: [("foreign".into(), foreign)].into(),
        ..CallOptions::default()
    };
    assert_eq!(script.run(options.clone()).unwrap().value.as_int(), Some(7));
    let report = script.check_call("__main__", &[], &options).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn equivalent_file_compilations_keep_the_same_analysis_budget() {
    let source =
        "enum Zebra;A;end;enum Medium;B;end;enum S;C;end;def run(value:Zebra)->Zebra;value;end";
    let mut expected = None;
    for _ in 0..16 {
        let script = file(&Engine::new(), source);
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{report:?}");
        let actual = (
            report.stats.steps,
            report.stats.peak_memory_bytes,
            report.stats.retained_memory_bytes,
        );
        assert_eq!(actual, *expected.get_or_insert(actual));
    }
}
