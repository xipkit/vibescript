use super::{
    arguments::{Failure, Input},
    attached_tests::admitted,
    calls::{self, Analysis, Host, Target, World},
    facts::{Atom, Facts},
    flow::IssueKind,
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Script, Signature,
    SignatureParam, Value, budget::Buffer, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = "enum Status; Draft; Sent; end; enum Review; Draft; end;";

struct Fixture {
    script: Script,
    method: Value,
    globals: Vec<(String, Value)>,
    counters: Arc<[AtomicUsize; 3]>,
}

impl Fixture {
    fn new(source: &str, params: &[(&str, bool)], result: &str, accepts_block: bool) -> Self {
        let counters = Arc::new(std::array::from_fn::<_, 3, _>(|_| AtomicUsize::new(0)));
        let calls = counters.clone();
        let arguments = counters.clone();
        let returns = counters.clone();
        let method = HostMethod::new("echo", move |_, args, _| {
            calls[0].fetch_add(1, Ordering::Relaxed);
            Ok(args.first().cloned().unwrap_or_else(Value::nil))
        })
        .with_contract(
            move |_, _, _| {
                arguments[1].fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            move |_, _| {
                returns[2].fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .with_signature(Signature {
            params: params
                .iter()
                .map(|(ty, optional)| SignatureParam {
                    name: "value".into(),
                    ty: (*ty).into(),
                    optional: *optional,
                })
                .collect(),
            result: result.into(),
            accepts_block,
        })
        .unwrap();
        let mut engine = Engine::new();
        engine.register_method("echo", method.clone());
        let script = engine.compile(&format!("{SOURCE} {source}")).unwrap();
        let program = &script.inner.code.program;
        let status = program.declarations[program.declaration_names["Status"]].clone();
        let object = Value::object(vec![(b"deliver".to_vec(), method.value())]);
        Self {
            script,
            method: method.value(),
            globals: vec![
                ("alias".into(), status),
                ("sms".into(), object),
                ("count".into(), Value::int(0)),
            ],
            counters,
        }
    }

    fn analyze(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        inputs: &[Input],
    ) -> Result<Analysis> {
        let program = &self.script.inner.code.program;
        let Kind::Host(method) = &self.method.0 else {
            panic!()
        };
        let mut hosts = Buffer::empty();
        let host = Host::new(ctx, facts, method.compiled_signature())?;
        hosts.push(ctx, host)?;
        let mut globals = Buffer::empty();
        for (name, value) in &self.globals {
            let value = admitted(ctx, facts, program, &mut hosts, value)?;
            let name = ctx.bytes(name.as_bytes())?;
            globals.push(ctx, (name, Target::Value(value)))?;
        }
        let mut contracts = Buffer::empty();
        for ty in &program.types {
            let contract = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            contracts.push(ctx, contract)?;
        }
        calls::analyze(
            ctx,
            facts,
            World {
                loader: None,
                inputs: &[],
                program,
                source_owner: 0,
                contracts: &contracts.data,
                hosts: &hosts.data,
                globals: &globals.data,
            },
            program.names["run"],
            inputs,
        )
    }

    fn run(&self, args: &[Value]) -> crate::Outcome {
        self.script
            .call(
                "run",
                args,
                CallOptions {
                    globals: self.globals.iter().cloned().collect(),
                    ..CallOptions::default()
                },
            )
            .unwrap()
    }
}

fn witness(source: &str, ty: &str, result: &str, expected: &str, rejected: bool, calls: usize) {
    let fixture = Fixture::new(source, &[(ty, false)], result, false);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    for counter in fixture.counters.iter() {
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "analysis executed host code"
        );
    }
    let value = fixture.run(&[]).value;
    assert_eq!(value.to_string(), expected, "{source}");
    assert_eq!(
        fixture.counters[0].load(Ordering::Relaxed),
        calls,
        "{source}"
    );
    let actual = observed(
        &mut ctx,
        &mut facts,
        &fixture.script.inner.code.program,
        &value,
    );
    assert_eq!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Accepted,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_signatures_refresh_global_types_before_every_call() {
    for (source, ty, expected) in [
        ("def run; echo(:draft).enum.name; end", "Status", "Status"),
        (
            "def run; Math=Status; a=echo(:draft); Math=Review; b=echo(:draft); [a.enum.name,b.enum.name]; end",
            "Math",
            "[Status, Review]",
        ),
        (
            "def run; a=echo(:draft); alias=Review; b=echo(:draft); [a.enum.name,b.enum.name]; end",
            "alias",
            "[Status, Review]",
        ),
        (
            "def change; alias=Review; end; def run; change; echo(:draft).enum.name; end",
            "alias",
            "Review",
        ),
    ] {
        let calls = usize::from(source.contains("a=echo")) + 1;
        witness(source, ty, ty, expected, false, calls);
    }
    witness(
        "def run; echo(:sent); alias=Review; begin; echo(:sent); rescue; 99; end; end",
        "alias",
        "alias",
        "99",
        true,
        1,
    );
}

#[test]
fn host_types_observe_completed_argument_effects_and_selected_methods() {
    for (source, ty, expected) in [
        (
            "def change; alias=Review; :draft; end; def run; echo(change).enum.name; end",
            "alias",
            "Review",
        ),
        (
            "def run; Math=Status; echo(begin Math=Review; :draft end).enum.name; end",
            "Math",
            "Review",
        ),
        (
            "def run; sms.deliver(begin sms.clear; alias=Review; :draft end).enum.name; end",
            "alias",
            "Review",
        ),
        (
            "def run; sms[:deliver](begin sms.clear; alias=Review; :draft end).enum.name; end",
            "alias",
            "Review",
        ),
        (
            "def run; sms::deliver(begin sms.clear; alias=Review; :draft end).enum.name; end",
            "alias",
            "Review",
        ),
        (
            "def run; alias=Review; sms.send(:deliver, :draft).enum.name; end",
            "alias",
            "Review",
        ),
        (
            "def run; alias=Review; sms.public_send(:deliver, *[:draft]).enum.name; end",
            "alias",
            "Review",
        ),
    ] {
        witness(source, ty, ty, expected, false, 1);
    }
}

#[test]
fn host_signatures_ignore_caller_locals_and_lexical_block_aliases() {
    for (source, ty, expected, calls) in [
        (
            "def helper(alias); echo(:draft).enum.name; end; def run; alias=Review; helper(Status); end",
            "alias",
            "Review",
            1,
        ),
        (
            "def run; local=Review; begin; echo(:draft); rescue; 99; end; end",
            "local",
            "99",
            0,
        ),
        (
            "def run; Math=Status; [1].map { Math=Review; echo(:draft).enum.name }; end",
            "Math",
            "[Status]",
            1,
        ),
        (
            "def run; [1,2].map { name=echo(:draft).enum.name; alias=Review; name }; end",
            "alias",
            "[Status, Review]",
            2,
        ),
    ] {
        witness(source, ty, ty, expected, calls == 0, calls);
    }
}

#[test]
fn live_host_type_lookup_preserves_exact_folded_and_qualified_names() {
    for (source, ty, expected, rejected) in [
        (
            "enum MATH; Draft; end; def run; Math=Review; echo(:draft).enum.name; end",
            "MATH",
            "MATH",
            false,
        ),
        (
            "enum MATH; Draft; end; def run; Math=Review; echo(:draft).enum.name; end",
            "Math",
            "Review",
            false,
        ),
        (
            "enum MATH; Draft; end; def run; Math=Review; begin; echo(:draft); rescue; 99; end; end",
            "math",
            "99",
            true,
        ),
        (
            "enum MATH; Draft; end; def run; Math=7; echo(:draft).enum.name; end",
            "math",
            "MATH",
            false,
        ),
        (
            "def run; Math[:State]=Review; echo(:draft).enum.name; end",
            "Math.STate",
            "Review",
            false,
        ),
        (
            "def run; Math[:State]=Review; begin; echo(:draft); rescue; 99; end; end",
            "math.State",
            "99",
            true,
        ),
        (
            "def run; sms[:State]=Review; echo(:draft).enum.name; end",
            "sms.State",
            "Review",
            false,
        ),
        (
            "enum ΣΤΑΤΕ; Draft; end; def run; echo(:draft).enum.name; end",
            "στατε",
            "ΣΤΑΤΕ",
            false,
        ),
    ] {
        witness(source, ty, ty, expected, rejected, usize::from(!rejected));
    }
}

#[test]
fn missing_host_parameter_types_are_catchable_before_callbacks() {
    for (ty, argument) in [
        ("Missing", ":draft"),
        ("Missing?", "nil"),
        ("array<Missing>", "[]"),
        ("hash<string,Missing>", "{}"),
        ("{state?:Missing}", "{}"),
        ("Missing | any", "7"),
        ("any | Missing", "7"),
        ("array<Missing | any>", "[]"),
    ] {
        let source = format!(
            "def run; begin; echo({argument}); rescue; count=7; ensure; count+=1; end; count; end"
        );
        witness(&source, ty, "", "8", true, 0);
    }
}

#[test]
fn live_host_result_types_resolve_after_callbacks_and_fail_in_callers() {
    for (source, result, expected, rejected) in [
        (
            "def run; alias=Review; echo(:draft).enum.name; end",
            "alias",
            "Review",
            false,
        ),
        (
            "def run; begin; echo(nil); rescue; 99; end; end",
            "Missing?",
            "99",
            true,
        ),
        (
            "def run; alias=nil; begin; echo([]); rescue; 99; end; end",
            "array<alias>",
            "99",
            true,
        ),
    ] {
        witness(source, "", result, expected, rejected, 1);
    }
}

#[test]
fn live_host_contracts_normalize_nested_and_ordered_types() {
    for (ty, argument, expected) in [
        (
            "array<{state:alias}>",
            "[{state: :draft}]",
            "[{state: Review::Draft}]",
        ),
        (
            "hash<string,alias>",
            "{state: :draft}",
            "{state: Review::Draft}",
        ),
        ("alias | symbol", ":draft", "Review::Draft"),
        ("symbol | alias", ":draft", "draft"),
        ("alias?", "nil", "nil"),
        ("any | alias", ":draft", "Review::Draft"),
    ] {
        let source = format!("def run; alias=Review; echo({argument}); end");
        witness(&source, ty, ty, expected, false, 1);
    }
    witness(
        "class Box; end; def run; echo(nil); end",
        "Box?",
        "Box?",
        "nil",
        false,
        1,
    );
}

#[test]
fn invalid_host_values_stop_before_resolving_later_parameters() {
    let fixture = Fixture::new(
        "def run; begin; echo(\"bad\", nil); rescue; 99; end; end",
        &[("int", false), ("Missing", false)],
        "Missing",
        false,
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
    assert!(report.incomplete.data.is_empty());
    assert!(report.issues.data.iter().any(|issue| matches!(
        issue.issue.kind,
        IssueKind::Call {
            failure: Failure::Type { parameter: 0, .. },
            ..
        }
    )));
    assert!(!report.issues.data.iter().any(|issue| matches!(
        issue.issue.kind,
        IssueKind::Call {
            failure: Failure::HostTypeBinding { .. },
            ..
        }
    )));
    assert_eq!(fixture.run(&[]).value.to_string(), "99");
    assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 0);
}

#[test]
fn omitted_host_parameters_do_not_resolve_their_types() {
    let fixture = Fixture::new("def run; echo(); end", &[("Missing", true)], "", false);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
    assert!(report.incomplete.data.is_empty());
    assert!(report.issues.data.is_empty());
    assert_eq!(fixture.run(&[]).value.to_string(), "nil");
    assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 1);
}

#[test]
fn host_arity_keywords_and_block_guards_precede_type_resolution() {
    for (call, failure) in [
        ("echo()", Failure::HostArity),
        ("echo(nil,nil)", Failure::HostArity),
        ("echo(nil, key: 7)", Failure::HostKeywords),
        ("echo(nil) { count=99 }", Failure::HostBlock),
    ] {
        let source = format!("def run; begin; {call}; rescue; count+=7; end; count; end");
        let fixture = Fixture::new(&source, &[("Missing", false)], "Missing", false);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.issues.data.iter().any(|issue| matches!(issue.issue.kind, IssueKind::Call { failure: actual, .. } if actual == failure)), "{source}: {report:?}");
        assert!(matches!(
            facts.node(report.returns),
            super::facts::Node::Integer(7)
        ));
        assert_eq!(report.contexts, 1);
        assert_eq!(fixture.run(&[]).value.to_string(), "7");
        assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 0);
    }
}

#[test]
fn unknown_host_type_environments_stay_incomplete() {
    for (source, ty, accepts_block, inputs) in [(
        "def run(flag); Math=if flag; Status; else; Review; end; echo(:draft); end",
        "Math",
        false,
        vec![Input::Supplied(Atom::Bool.fact())],
    )] {
        let fixture = Fixture::new(source, &[(ty, false)], ty, accepts_block);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = fixture.analyze(&mut ctx, &mut facts, &inputs).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        for counter in fixture.counters.iter() {
            assert_eq!(counter.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn missing_host_type_members_fail_in_the_initializer_before_callbacks() {
    let source = "module Box; echo(:draft); State=Status; end; def run; echo(:draft); end";
    let fixture = Fixture::new(source, &[("Box.State", false)], "Box.State", false);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(report.returns, Atom::Never.fact());
    assert!(
        report.issues.data.iter().any(|issue| matches!(
            issue.issue.kind,
            IssueKind::Call {
                failure: Failure::HostTypeBinding { .. },
                ..
            }
        )),
        "{report:?}"
    );
    for counter in fixture.counters.iter() {
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
    assert!(
        fixture
            .script
            .call(
                "run",
                &[],
                CallOptions {
                    globals: fixture.globals.iter().cloned().collect(),
                    ..CallOptions::default()
                }
            )
            .is_err()
    );
    assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 0);
    assert_eq!(fixture.counters[1].load(Ordering::Relaxed), 1);
    assert_eq!(fixture.counters[2].load(Ordering::Relaxed), 0);
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_type_failures_identify_the_first_boundary_and_keep_callback_errors() {
    for (source, parameter, result, expected_parameter, ambiguous, callback, block) in [
        (
            "def run; echo(nil); end",
            "Missing?",
            "Missing?",
            Some(0),
            false,
            false,
            false,
        ),
        (
            "def run; echo(nil); end",
            "",
            "Missing?",
            None,
            false,
            true,
            false,
        ),
        (
            "enum MATH; Draft; end; def run; Math=Review; echo(:draft); end",
            "math",
            "math",
            Some(0),
            true,
            false,
            false,
        ),
        (
            "def run; echo(nil) { count=99 }; end",
            "Missing?",
            "",
            Some(0),
            false,
            false,
            true,
        ),
    ] {
        let fixture = Fixture::new(source, &[(parameter, false)], result, block);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert_eq!(report.returns, Atom::Never.fact());
        assert_eq!(report.contexts, 1);
        assert_eq!(report.throws, u8::MAX);
        assert_eq!(report.issues.data.len(), 1, "{source}: {report:?}");
        assert!(
            matches!(report.issues.data[0].issue.kind, IssueKind::Call { failure: Failure::HostTypeBinding { parameter, ambiguous: actual, .. }, .. } if parameter == expected_parameter && actual == ambiguous)
        );
        for counter in fixture.counters.iter() {
            assert_eq!(counter.load(Ordering::Relaxed), 0);
        }
        let error = fixture
            .script
            .call(
                "run",
                &[],
                CallOptions {
                    globals: fixture.globals.iter().cloned().collect(),
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(
            error.message.contains(if ambiguous {
                "ambiguous named type"
            } else {
                "unknown type"
            }),
            "{error}"
        );
        assert_eq!(
            fixture.counters[0].load(Ordering::Relaxed),
            usize::from(callback)
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn host_root_type_bindings_override_source_declarations_including_nil() {
    for missing in [false, true] {
        let mut fixture = Fixture::new(
            "def run; begin; echo(:draft).enum.name; rescue; 99; end; end",
            &[("Status", false)],
            "Status",
            false,
        );
        let program = &fixture.script.inner.code.program;
        let replacement = if missing {
            Value::nil()
        } else {
            program.declarations[program.declaration_names["Review"]].clone()
        };
        fixture.globals.push(("Status".into(), replacement));
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert_eq!(!report.issues.data.is_empty(), missing, "{report:?}");
        assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 0);
        let value = fixture.run(&[]).value;
        assert_eq!(value.to_string(), if missing { "99" } else { "Review" });
        assert_eq!(
            fixture.counters[0].load(Ordering::Relaxed),
            usize::from(!missing)
        );
        let actual = observed(&mut ctx, &mut facts, program, &value);
        assert_eq!(
            facts.relation(&mut ctx, actual, report.returns).unwrap(),
            Relation::Accepted
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn host_type_bindings_restart_for_each_analysis_and_execution() {
    let fixture = Fixture::new(
        "def run; first=echo(:draft); alias=Review; [first.enum.name,echo(:draft).enum.name]; end",
        &[("alias", false)],
        "alias",
        false,
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for _ in 0..3 {
        let report = fixture.analyze(&mut ctx, &mut facts, &[]).unwrap();
        assert!(report.incomplete.data.is_empty());
        assert!(report.issues.data.is_empty());
        assert_eq!(fixture.run(&[]).value.to_string(), "[Status, Review]");
    }
    assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 6);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn work(ctx: &mut CallContext, fixture: &Fixture) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = fixture.analyze(ctx, &mut facts, &[])?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

fn accounting_fixture() -> Fixture {
    Fixture::new(
        "def run; Math[:State]=Status; first=echo([{state: :draft}]); Math[:State]=Review; second=echo([{state: :draft}]); [first,second]; end",
        &[("array<{state:Math.State}>", false)],
        "array<{state:Math.State}>",
        false,
    )
}

#[test]
fn live_host_types_obey_exact_and_interrupted_work_and_memory_limits() {
    let fixture = accounting_fixture();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &fixture).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, kind) in [
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
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(work(&mut ctx, &fixture).err().map(|error| error.kind), kind);
        if let Some(kind) = kind {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..24 {
        for memory in [false, true] {
            let (limits, kind) = if memory {
                (
                    Limits {
                        memory_bytes: Some(stats.peak_memory_bytes * sample / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Memory,
                )
            } else {
                (
                    Limits {
                        steps: Some(stats.steps * sample as u64 / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Steps,
                )
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(work(&mut ctx, &fixture).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn live_host_type_resolution_keeps_cancellation_and_deadlines_latched() {
    let fixture = accounting_fixture();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        drop(fixture.analyze(&mut ctx, &mut facts, &[]).unwrap());
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(
            fixture.analyze(&mut ctx, &mut facts, &[]).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
