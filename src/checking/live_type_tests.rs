use super::normalization_tests::witness;
use super::{facts::Facts, normalization_tests::analyze};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, bytecode};

#[test]
fn annotation_only_aliases_and_source_replacements_use_live_lexical_values() {
    witness(
        "def run; Alias=Status; [{state: :draft}].map { |x:{state:Alias}| x.state.symbol }; end",
        &[],
        "[draft]",
        false,
    );
    witness(
        "def run; Alias=Status; [{state: :draft}].map { |x:hash<string,Alias>| x.state.symbol }; end",
        &[],
        "[draft]",
        false,
    );
    witness(
        "def run; Alias=Status; [[:draft,:unknown]].map { |x:array<Alias|symbol>| x.map { |v| v.is_type?(:symbol) } }; end",
        &[],
        "[[false, true]]",
        false,
    );
    for (body, expected) in [
        (
            "Alias=Status; [:draft].map { |x:Alias| x.symbol }",
            "[draft]",
        ),
        (
            "Alias=Review; [:draft].map { |x:Alias| x.enum.name }",
            "[Review]",
        ),
        (
            "Status=Review; [:draft].map { |x:Status| x.enum.name }",
            "[Review]",
        ),
        (
            "Status=7; [:draft].map { |x:Status| x.enum.name }",
            "[Status]",
        ),
        (
            "Alias=Status; [[:draft,:sent]].map { |xs:array<Alias>| xs.map { |x| x.symbol } }",
            "[[draft, sent]]",
        ),
        (
            "Alias=Status; [nil,:draft].map { |x:Alias?| x&.symbol }",
            "[nil, draft]",
        ),
    ] {
        witness(&format!("def run; {body}; end"), &[], expected, false);
    }
}

#[test]
fn annotation_captures_cross_unread_scopes_and_parameter_shadows() {
    for (body, expected) in [
        (
            "Alias=Status; once { once { [:draft].map { |x:Alias| x.symbol } } }",
            "[draft]",
        ),
        (
            "Alias=Status; [Review].map { |Alias| [:draft].map { |x:Alias| x.enum.name } }",
            "[[Review]]",
        ),
        (
            "Alias=Status; [1].map { |Alias| [:draft].map { |x:Alias| x.enum.name } }",
            "[[Status]]",
        ),
        (
            "Alias=Status; [:draft].map { |Alias:Alias| Alias.symbol }",
            "[draft]",
        ),
    ] {
        witness(
            &format!("def once; yield; end; def run; {body}; end"),
            &[],
            expected,
            false,
        );
    }
}

#[test]
fn repeated_and_forwarded_callbacks_refresh_annotation_aliases() {
    witness(
        "def twice; [yield(:draft),yield(:draft)]; end; def run; Alias=Status; twice { |x:Alias| name=x.enum.name; Alias=Review; name }; end",
        &[],
        "[Status, Review]",
        false,
    );
    witness(
        "def relay; yield(:draft); end; def wrap; relay { |v| yield(v) }; end; def run; Alias=Review; wrap { |x:Alias| x.enum.name }; end",
        &[],
        "Review",
        false,
    );
    witness(
        "def run; Alias=Status; [:draft,:draft].map { |x:Alias| name=x.enum.name; Alias=Review; name }; end",
        &[],
        "[Status, Review]",
        false,
    );
}

#[test]
fn named_type_lookup_keeps_exact_precedence_folded_scopes_and_ambiguity() {
    for (body, expected, rejected) in [
        (
            "status=Review; [:draft].map { |x:Status| x.enum.name }",
            "[Status]",
            false,
        ),
        (
            "Status=Review; [:draft].map { |x:status| x.enum.name }",
            "[Review]",
            false,
        ),
        (
            "Alias=Status; ALIAS=Status; [:draft].map { |x:alias| x.enum.name }",
            "[Status]",
            false,
        ),
        (
            "Alias=Status; ALIAS=Review; begin; [:draft].map { |x:alias| x }; rescue; 99; end",
            "99",
            true,
        ),
    ] {
        witness(&format!("def run; {body}; end"), &[], expected, rejected);
    }
}

#[test]
fn missing_or_incompatible_block_types_fail_at_the_catchable_boundary() {
    for body in [
        "Alias=7; begin; [:draft].map { |x:Alias| raise \"body\" }; rescue; 99; end",
        "begin; [nil].map { |x:Missing?| x }; rescue; 99; end",
        "begin; [[]].map { |x:array<Missing>| x }; rescue; 99; end",
        "begin; [[]].map { |x:array<Missing|any>| x }; rescue; 99; end",
        "Alias=Review; begin; [Status::Draft].map { |x:Alias| x }; rescue; 99; end",
    ] {
        witness(&format!("def run; {body}; end"), &[], "99", true);
    }
}

#[test]
fn ordinary_function_contracts_do_not_inherit_caller_aliases() {
    witness(
        "def apply(Alias); [:draft].map { |x:Alias| x.enum.name }; end; def run; [apply(Status),apply(Review)]; end",
        &[],
        "[[Status], [Review]]",
        false,
    );
    witness(
        "def apply(Alias=Status); [:draft].map { |x:Alias| x.enum.name }; end; def run; apply(); end",
        &[],
        "[Status]",
        false,
    );
    witness(
        "def apply(Status,x:Status); x.enum.name; end; def run; apply(Review,:draft); end",
        &[],
        "Status",
        false,
    );
    witness(
        "def apply(x); yield(x); end; def run; Alias=Status; apply(begin; Alias=Review; :draft; end) { |x:Alias| x.enum.name }; end",
        &[],
        "Review",
        false,
    );
    witness(
        "def echo(x:Status); x.enum.name; end; def run; Status=Review; [echo(:draft),[:draft].map { |x:Status| x.enum.name }]; end",
        &[],
        "[Status, [Review]]",
        false,
    );
}

#[test]
fn typed_callback_failures_and_control_transfers_keep_completed_capture_effects() {
    witness(
        "def run; Alias=Status; log=[]; begin; [:draft,:sent].each { |x:Alias| log.push(x.enum.name); Alias=Review }; rescue; log.push(\"bad\"); end; log; end",
        &[],
        "[Status, bad]",
        true,
    );
    witness(
        "def run; Alias=Status; result=[:draft].each { |x:Alias| Alias=Review; break x.enum.name }; [result,Alias.name]; end",
        &[],
        "[Status, Review]",
        false,
    );
    witness(
        "def once; yield(:draft); end; def run; Alias=Status; once { |x:Alias| begin; return x.enum.name; ensure; Alias=Review; end }; 99; end",
        &[],
        "Status",
        false,
    );
    witness(
        "def run; Alias=Status; log=[]; begin; [:sent,:sent].each { |x:Alias| log.push(x.name); Alias=Review }; rescue; log.push(Alias.name); ensure; log.push(\"done\"); end; log; end",
        &[],
        "[Sent, Review, done]",
        true,
    );
}

#[test]
fn lexical_type_names_fold_unicode_and_ignore_ordinary_hash_exports() {
    witness(
        "def run; État=Status; [:draft].map { |x:état| x.symbol }; end",
        &[],
        "[draft]",
        false,
    );
    witness(
        "def run; Alias={Status:Status}; begin; [:draft].map { |x:Alias.Status| x.symbol }; rescue; 99; end; end",
        &[],
        "99",
        true,
    );
}

#[test]
fn uncertain_alias_identities_remain_explicit_instead_of_using_source_types() {
    for source in [
        "enum Status; Draft; end; enum Review; Draft; end; def run(flag:bool); if flag; Status=Review; else; Status=Status; end; [:draft].map { |x:Status| x.enum.name }; end",
        "enum Status; Draft; end; def run(Alias); [:draft].map { |x:Alias| x }; end",
    ] {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn annotation_only_captures_relay_across_deep_callbacks_on_the_default_stack() {
    let body = format!(
        "Alias=Status; {}[:draft].map {{ |x:Alias| x.symbol }}{}",
        "once {".repeat(40),
        "}".repeat(40)
    );
    witness(
        &format!("def once; yield; end; def run; {body}; end"),
        &[],
        "[draft]",
        false,
    );
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, program)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("enum Status; Draft; Sent; end; enum Review; Draft; end; def once; yield; end; def twice; [yield(:draft),yield(:draft)]; end; def run; Alias=Status; once { once { twice { |x:Alias| value=x.enum.name; Alias=Review; value } } }; end", Vec::new(), &()).unwrap()
}

#[test]
fn lexical_annotation_work_obeys_exact_limits_and_reclaims_partial_captures() {
    let program = accounting_program();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &program).unwrap();
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
        assert_eq!(work(&mut ctx, &program).err().map(|e| e.kind), kind);
        if let Some(kind) = kind {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..32 {
        for memory in [false, true] {
            let limits = if memory {
                Limits {
                    memory_bytes: Some(stats.peak_memory_bytes * sample / 32),
                    ..Limits::default()
                }
            } else {
                Limits {
                    steps: Some(stats.steps * sample as u64 / 32),
                    ..Limits::default()
                }
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let kind = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            };
            assert_eq!(work(&mut ctx, &program).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn lexical_annotation_analysis_observes_latched_cancellation_and_deadlines() {
    let program = accounting_program();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        drop(report);
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let kind = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        assert_eq!(
            analyze(&mut ctx, &mut facts, &program).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn host_type_replacements_stay_dynamic_without_running_capability_factories() {
    use super::{
        calls::{self, Target, World},
        type_bindings::Bindings,
    };
    use crate::{Capability, Engine, Value, budget::Buffer};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let script = Engine::legacy_unchecked().compile("enum Status; Draft; end; enum Review; Draft; end; def run; [:draft].map { |x:Status| x.enum.name }; end").unwrap();
    let program = &script.inner.code.program;
    let review = program
        .declarations
        .iter()
        .find(|value| value.as_enum_type() == Some("Review"))
        .unwrap()
        .clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let options = CallOptions {
        capabilities: vec![Capability::new("Status", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(review.clone())
        })],
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut bindings = Bindings::new();
    let scope = bindings.source(&mut ctx, &mut facts, program, 0).unwrap();
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts
            .annotation(&mut ctx, ty, |ctx, name| {
                Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
            })
            .unwrap();
        contracts.push(&mut ctx, fact).unwrap();
    }
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program,
            contracts: &contracts.data,
            hosts: &[],
            globals: &[(Value::bytes(b"Status"), Target::NonCallable)],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert!(!report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        script.call("run", &[], options).unwrap().value.to_string(),
        "[Review]"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop((report, contracts, bindings, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
