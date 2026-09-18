use super::{entry, normalization_tests::observed, relation::Relation};
use crate::{CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Signature, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn general(source: &str, clean: bool) {
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
}

#[test]
fn declared_parameters_and_optional_defaults_define_the_general_scope() {
    for (source, clean) in [
        ("def run(x:int)->int;x+1;end", true),
        ("def run(x:string)->int;x;end", false),
        ("def run(x:int=false)->int;x;end", false),
        ("def run(x:int=7)->int;x;end", true),
        ("def run(a:int,b:int=a+1)->int;b;end", true),
        ("def run(a:int,b:string=a+1);b;end", false),
        ("def run(x)->int;x;end", true),
        ("def run(x:any)->int;x;end", true),
        ("def run(x:{name:string})->int;x[:name];end", false),
        ("def run(x:int|string)->int;x;end", false),
        ("def run(x:int?)->int;if x.nil?;0;else;x;end;end", true),
    ] {
        general(source, clean);
    }
}

#[test]
fn rest_keywords_and_shape_domains_keep_their_declared_boundaries() {
    for (source, clean) in [
        ("def run(*xs:array<int>)->int;xs.sum;end", true),
        ("def run(*xs:array<string>)->array<int>;xs;end", false),
        (
            "def run(y:int=2,x:int:,**rest:hash<string,int>)->int;x+y;end",
            true,
        ),
        ("def run(x:int:);x+false;end", false),
        (
            "def run(**rest:hash<string,int>)->hash<string,string>;rest;end",
            false,
        ),
        ("def run(*xs,**rest);[xs.length,rest.keys];end", true),
        (
            "def run(**rest:hash<string,int>)->array<int>;rest.values;end",
            true,
        ),
        (
            "def run(**rest:hash<symbol,int>)->array<string>;rest.keys;end",
            true,
        ),
        ("def run(*xs:array<int>?)->array<int>;xs;end", true),
        (
            "def run(**rest:hash<string,int>?)->array<int>;rest.values;end",
            true,
        ),
    ] {
        general(source, clean);
    }
}

#[test]
fn named_contracts_resolve_after_initializers_and_keep_nominal_identity() {
    for (source, clean) in [
        ("enum E;a;b;end;def run(x:E)->E;x;end", true),
        ("enum E;a;b;end;enum F;a;b;end;def run(x:E)->F;x;end", false),
        ("class C;end;def run(x:C)->C;x;end", true),
        ("class C;end;class D;end;def run(x:C)->D;x;end", false),
        (
            "enum E;a;b;end;module M;Math.store(:State,E);end;def run(x:Math.State)->E;x;end",
            true,
        ),
        ("enum E;a;b;end;def run(x:array<E>)->array<E>;x;end", true),
    ] {
        general(source, clean);
    }
    let script = Engine::new().compile("def run(x:Missing);x;end").unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
}

#[test]
fn general_entries_do_not_hide_invalid_recursive_arguments() {
    for source in [
        "def run(n:int);if n>0;run(false);else;7;end;end",
        "def other(n:int);run(n);end;def run(n:int);if n>0;other(false);else;7;end;end",
        "enum E;a;b;end;def run(x:E);if x==E::a;run(7);else;x;end;end",
        "def run(n:int=0);if n>0;run(false);else;7;end;end",
    ] {
        general(source, false);
    }
    general("def run(n:int)->int;if n>0;run(n-1);else;7;end;end", true);
    general(
        "def run(n:int)->array;if n>0;[run(n-1)];else;[];end;end",
        true,
    );
}

#[test]
fn general_checks_follow_callees_but_keep_unrelated_code_outside_the_scope() {
    general(
        "false+true;def unrelated->int;false;end;def run->int;7;end",
        true,
    );
    general(
        "def bad(n:int)->int;false;end;def run(n:int);bad(n);end",
        false,
    );
    general("def run(n:int);[1,2].each{|i| n+=i};n;end", true);
    general("def run(n:int);[1].each{|i| n=false};n;end", false);
    let script = Engine::new()
        .compile("def run(flag:bool)->int;if flag;7;else;false;end;end")
        .unwrap();
    assert!(
        script
            .check_call("run", &[Value::boolean(true)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert!(
        !script
            .check_function("run", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    let script = Engine::new().compile("def run(x:int=false);x;end").unwrap();
    assert!(
        script
            .check_call("run", &[Value::int(7)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert!(
        !script
            .check_function("run", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn general_checks_keep_unmodeled_collection_and_instance_dispatch_explicit() {
    for source in [
        "def run(a:array<int>,b:array<int>);a+b;end",
        "class C;def read;7;end;end;def run(value:C);value.read;end",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        assert!(!report.incomplete.is_empty(), "{source}: {report:?}");
    }
}

#[test]
fn general_analysis_never_executes_defaults_initializers_callbacks_or_writers() {
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
    let script = engine
        .compile("module M;C=tick();end;def run(x:int=tick())->int;puts x;x+M::C;end")
        .unwrap();
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(14)
    );
    assert_eq!(effects.load(Ordering::Relaxed), 3);
}

fn top(source: &str, expected: &str, issues: bool) {
    let script = Engine::new().compile(source).unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(options.clone());
    let mut checked = entry::check(
        &mut ctx,
        entry::Call {
            script: &script,
            name: "__main__",
            arguments: &[],
            keywords: &[],
            options: &options,
        },
    )
    .unwrap();
    assert!(
        checked.analysis.incomplete.data.is_empty(),
        "{source}: {checked:?}"
    );
    assert_eq!(
        !checked.analysis.issues.data.is_empty(),
        issues,
        "{source}: {checked:?}"
    );
    let actual = script.run(options).unwrap().value;
    assert_eq!(actual.to_string(), expected, "{source}");
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
        "{source}: {checked:?}"
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn top_level_analysis_tracks_expressions_calls_loops_and_cleanup() {
    top(
        "a=[1,2,3];n=0;i=0;while i<a.length;n+=a[i];i+=1;end;n",
        "6",
        false,
    );
    top("def add(x:int)->int;x+1;end;add(7)", "8", false);
    top("begin;1/0;rescue;7;ensure;1+1;end", "7", false);
    top("return 7;false+true", "7", false);
    top("def unused->int;false;end;7", "7", false);
}

#[test]
fn top_level_namespaces_initialize_in_source_order() {
    top(
        "module M;@@n=7;def self.value;@@n;end;end;M.value+1",
        "8",
        false,
    );
    top(
        "M.value;module M;@@n=7;def self.value;@@n;end;end;M.value",
        "7",
        false,
    );
    top("return 7;module M;false+true;end", "7", false);
    top("module M;module N;D=7;end;C=N::D;end;M::C", "7", false);
}

#[test]
fn named_calls_keep_their_preinitialization_and_entry_shape_order() {
    let source = "M.value+1;module M;@@n=7;def self.value;@@n;end;end;def run;M.value+1;end";
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_call("__main__", &[], &CallOptions::default())
        .unwrap();
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(script.run(CallOptions::default()).is_err());
    assert!(
        script
            .check_function("run", &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(8)
    );
    let script = Engine::new().compile("module M;false+true;end").unwrap();
    let report = script
        .check_call("__main__", &[Value::int(7)], &CallOptions::default())
        .unwrap();
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(
        script
            .call("__main__", &[Value::int(7)], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Argument
    );
}

#[test]
fn general_analysis_is_metered_interruptible_and_releases_temporary_state() {
    let script=Engine::new().compile("enum E;a;b;end;module M;Math.store(:State,E);end;def run(xs:array<Math.State>,n:int=2)->array<E>;if n>0;run(xs,n-1);else;xs;end;end").unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(options.clone());
    let checked = entry::check_function(&mut ctx, &script, "run", &options).unwrap();
    assert!(checked.analysis.issues.data.is_empty(), "{checked:?}");
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    let stats = ctx.stats();
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in [0, 1, 8, 15, 16] {
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
            let checked = entry::check_function(&mut ctx, &script, "run", &options);
            if sample == 16 {
                drop(checked.unwrap());
            } else {
                assert_eq!(checked.unwrap_err().kind, kind);
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
            entry::check_function(&mut ctx, &script, "run", &options)
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
