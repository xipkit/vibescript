use super::{
    facts::Facts,
    normalization_tests::{analyze, witness},
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value, bytecode};

#[test]
fn defaults_publish_global_effects_before_later_argument_type_failures() {
    witness(
        "def change(first=Math.push(2), x:int:); Math.push(99); end; def run; Math=[1]; begin; change(x:\"bad\"); rescue; Math; end; end",
        &[],
        "[1, 2]",
        true,
    );
}

#[test]
fn globals_track_replacement_nested_mutations_and_value_copies() {
    for (body, expected) in [
        (
            "Math=[1]; copy=Math; Math.push(2); Math[0]+=3; [Math,copy]",
            "[[4, 2], [1]]",
        ),
        (
            "Math={items:[1]}; Math.items.push(2); Math.items[0]=3; Math",
            "{items: [3, 2]}",
        ),
        (
            "Math=[1]; Math.push(Math.push(2).length); Math",
            "[1, 2, 2]",
        ),
        ("Math=nil; Math ||= [1]; Math &&= [2]; Math", "[2]"),
    ] {
        witness(&format!("def run; {body}; end"), &[], expected, false);
    }
}

#[test]
fn helpers_publish_global_writes_on_value_and_error_exits() {
    for (helper, body, expected) in [
        (
            "Math.push(2); 7",
            "value=change; [Math,value]",
            "[[1, 2], 7]",
        ),
        ("Math=[9]; 7", "value=change; [Math,value]", "[[9], 7]"),
        (
            "Math.push(2); raise 'bad'",
            "begin; change; rescue; Math.push(3); ensure; Math.push(4); end; Math",
            "[1, 2, 3, 4]",
        ),
        (
            "begin; Math.push(2); return 7; ensure; Math.push(3); end",
            "value=change; [Math,value]",
            "[[1, 2, 3], 7]",
        ),
    ] {
        witness(
            &format!("def change; {helper}; end; def run; Math=[1]; {body}; end"),
            &[],
            expected,
            false,
        );
    }
}

#[test]
fn helper_replacements_detach_previously_selected_global_addresses() {
    for (replacement, body, expected) in [
        ("Math=[9]", "Math.push(change); Math", "[9]"),
        ("Math=[1]", "Math.push(change); Math", "[1]"),
        ("Math=Math", "Math.push(change); Math", "[1, 2]"),
        ("Math=[9]", "Math[0]=change; Math", "[2]"),
        ("Math=[9]", "Math[0]+=change; Math", "[9]"),
        ("Math.push(9)", "Math[-1]+=change; Math", "[3, 9]"),
    ] {
        witness(
            &format!("def change; {replacement}; 2; end; def run; Math=[1]; {body}; end"),
            &[],
            expected,
            false,
        );
    }
}

#[test]
fn globals_survive_yield_forwarding_and_collection_callbacks() {
    for (body, expected) in [
        ("once {Math.push(2)}; Math", "[1, 2]"),
        ("relay {Math.push(2)}; Math", "[1, 2]"),
        ("[2,3].each {|n| once {Math.push(n)}}; Math", "[1, 2, 3]"),
        (
            "value=once {Math.push(2); break 7}; [Math,value]",
            "[[1, 2], 7]",
        ),
        (
            "value=[2,3].map {|n| Math.push(n); n*2}; [Math,value]",
            "[[1, 2, 3], [4, 6]]",
        ),
        ("Math.push(once {Math=[9]; 2}); Math", "[1, 2]"),
        ("Math[-1]+=once {Math.push(9); 2}; Math", "[3, 9]"),
    ] {
        witness(
            &format!(
                "def once; yield; end; def relay; once {{yield}}; end; def run; Math=[1]; {body}; end"
            ),
            &[],
            expected,
            false,
        );
    }
}

#[test]
fn global_descriptors_and_call_targets_use_current_bindings() {
    for (body, expected, warnings) in [
        ("JSON={parse:Math::sqrt}; JSON.send(:parse,9)", "3", false),
        ("format=7; format", "7", false),
        ("format=7; begin; format('x'); rescue; 9; end", "9", true),
        ("Math=[1]; format=Math.length; format", "1", false),
        (
            "to_int=Math::sqrt; to_int(begin; to_int=7; 9; end)",
            "3",
            false,
        ),
        ("to_int=7; to_int.nil?", "false", false),
    ] {
        witness(&format!("def run; {body}; end"), &[], expected, warnings);
    }
}

#[test]
fn absent_local_call_bindings_fall_back_to_the_current_global() {
    witness(
        "def once; yield; end; def run; format=Math::sqrt; once {ignored=if false; begin; format=7; end; end; format(9)}; end",
        &[],
        "3",
        false,
    );
}

#[test]
fn original_builtin_address_reads_fail_before_argument_effects() {
    witness(
        "def change; Math.push(1); raise 'bad'; end; def run; Math=[]; begin; format.push(change); rescue; Math; end; end",
        &[],
        "[]",
        true,
    );
    let source = "def change; Math.push(1); raise 'bad'; end; def run(flag:bool); Math=[]; if flag; format=7; end; begin; format.push(change); rescue; Math; end; end";
    for (flag, expected) in [(false, "[]"), (true, "[1]")] {
        witness(source, &[Value::boolean(flag)], expected, true);
    }
    witness(
        "def change; Math.push(1); raise 'bad'; end; def run; Math=[]; begin; now.push(change); rescue; Math; end; end",
        &[],
        "[1]",
        false,
    );
}

#[test]
fn global_effects_preserve_conditional_paths() {
    let source = "def change(flag:bool); if flag; Math=[9]; else; Math.push(2); end; 7; end; def run(flag:bool); Math=[1]; copy=Math; value=change(flag); [Math,copy,value]; end";
    for (flag, expected) in [(false, "[[1, 2], [1], 7]"), (true, "[[9], [1], 7]")] {
        witness(source, &[Value::boolean(flag)], expected, false);
    }
}

#[test]
fn global_contexts_distinguish_call_inputs_and_reset_between_invocations() {
    let source = "def add; Math+=1; end; def run; Math=1; first=add; Math=10; second=add; [first,second,Math]; end";
    witness(source, &[], "[2, 11, 11]", false);
    let script = crate::Engine::new().compile(source).unwrap();
    for _ in 0..3 {
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .to_string(),
            "[2, 11, 11]"
        );
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &script.inner.code.program).unwrap();
        assert!(report.issues.data.is_empty() && report.incomplete.data.is_empty());
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn global_effects_cross_loops_and_recursive_summaries() {
    for (source, expected) in [
        (
            "def add(n:int); Math.push(n); end; def run; Math=[]; for n in [1,2,3]; add(n); end; Math; end",
            "[1, 2, 3]",
        ),
        (
            "def walk(n:int); if n>0; Math.push(n); walk(n-1); end; end; def run; Math=[]; walk(3); Math; end",
            "[3, 2, 1]",
        ),
        (
            "def a(n:int); if n>0; Math.push(n); b(n-1); end; end; def b(n:int); a(n); end; def run; Math=[]; a(3); Math; end",
            "[3, 2, 1]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn global_pending_targets_survive_nested_calls_rescue_and_callback_returns() {
    for (source, expected) in [
        (
            "def replace; Math=[9]; raise 'bad'; end; def caught; begin; replace; rescue; 2; end; end; def run; Math=[1]; Math.push(caught); Math; end",
            "[9]",
        ),
        (
            "def replace; Math=[9]; 2; end; def once; yield; end; def run; Math=[1]; Math.push(once {replace}); Math; end",
            "[9]",
        ),
        (
            "def append; Math.push(9); 2; end; def helper; Math[-1]+=append; 3; end; def run; Math=[1]; Math[0]+=helper; Math; end",
            "[4, 9]",
        ),
        (
            "def once; yield; end; def change; once {Math.push(2); return 7}; Math.push(99); end; def run; Math=[1]; value=change; [Math,value]; end",
            "[[1, 2], 7]",
        ),
        (
            "def change; [2,3].each {|n| Math.push(n); return 7}; end; def run; Math=[1]; value=change; [Math,value]; end",
            "[[1, 2], 7]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn return_type_failures_preserve_completed_global_writes() {
    witness(
        "def change -> int; Math.push(2); 'bad'; end; def run; Math=[1]; begin; change; rescue; Math; end; end",
        &[],
        "[1, 2]",
        true,
    );
}

#[test]
fn global_mutations_preserve_existing_arguments_and_skip_noop_replacements() {
    for (source, expected) in [
        (
            "def change; Math=[9]; 2; end; def pair(a,b); [a,b]; end; def run; Math=[1]; pair(Math,change); end",
            "[[1], 2]",
        ),
        (
            "def change; Math ||= [9]; 2; end; def run; Math=[1]; Math.push(change); Math; end",
            "[1, 2]",
        ),
        (
            "def change; Math[0].push(9); 2; end; def run; Math=[[1]]; Math[0][-1]+=change; Math; end",
            "[[3, 9]]",
        ),
        (
            "def change; Math[0]=[9]; 2; end; def run; Math=[[1]]; Math[0].push(change); Math; end",
            "[[9]]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn global_analysis_keeps_expanding_pending_contexts_and_descriptor_objects_explicit() {
    for source in [
        "def walk(n:int); if n>0; Math.push(walk(n-1)); end; 7; end; def run; Math=[]; walk(3); Math; end",
        "def run; to_int=Math::sqrt; to_int.nil?; end",
        "def run; JSON={parse:Math::sqrt}; JSON.parse(9); end",
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
    bytecode::compile("def add(n:int); Math.push(n); 2; end; def once; yield; end; def run(flag:bool); Math=[[1]]; JSON=[]; value=once {JSON.push(3); 7}; Math=[1]; Math[-1]+=once {add(value)}; if flag; Math.push(4); else; Math=[9]; end; [Math,JSON]; end",Vec::new(),&()).unwrap()
}

#[test]
fn global_summaries_and_pending_targets_obey_exact_and_interrupted_quotas() {
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
            assert_eq!(work(&mut ctx, &program).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn global_analysis_preserves_latched_cancellation_and_deadlines() {
    let program = accounting_program();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        drop(analyze(&mut ctx, &mut facts, &program).unwrap());
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(
            analyze(&mut ctx, &mut facts, &program).unwrap_err().kind,
            kind
        );
        drop(facts);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn deep_global_call_chains_preserve_suspended_targets_on_the_default_stack() {
    let mut source = String::from("def run; Math=[1]; Math[-1]+=f0; Math; end;");
    for index in 0..64 {
        let body = if index == 63 {
            "Math.push(9); 2".into()
        } else {
            format!("f{}", index + 1)
        };
        source.push_str(&format!("def f{index}; {body}; end;"));
    }
    witness(&source, &[], "[3, 9]", false);
}

#[test]
fn protected_values_stay_protected_when_stored_in_globals() {
    witness(
        "def run; Math='ab'.match('(a)(b)'); begin; Math.captures.clear; rescue; Math.captures; end; end",
        &[],
        "[a, b]",
        true,
    );
    witness(
        "def run; Math='ab'.match('(a)(b)'); copy=Math.captures; copy.push('x'); [Math.captures,copy]; end",
        &[],
        "[[a, b], [a, b, x]]",
        false,
    );
}
