use super::{
    facts::Facts,
    normalization_tests::{analyze, witness},
};
use crate::{CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Value, bytecode};

#[test]
fn uncertain_replacement_sources_do_not_lose_possible_protection() {
    let source = "def sample; /(a)/.match(\"a\"); end; def run(input:hash); Math.replace(input); begin; Math.clear; 7; rescue; 9; end; end";
    let protected = Engine::new()
        .compile(source)
        .unwrap()
        .call("sample", &[], CallOptions::default())
        .unwrap()
        .value;
    // A bare hash contract admits plain data and protected objects alike, so
    // the replaced namespace keeps both the successful and the rejected clear
    // without a static verdict on either.
    for (input, expected) in [(Value::hash(vec![]), "7"), (protected, "9")] {
        witness(source, &[input], expected, false);
    }
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("def run(flag:bool); Math[:work]=flag ? Math::sqrt : 7; begin; Math[:work](9); rescue; 7; end; Math.clear; Math[:keys]=[7] if flag; Math.keys.push(9); Math.replace({a:1,b:2}); Math[:size]=7 if flag; values=Math.map {|k,v| v}; copy=Math.merge({c:3}); Math.keep_if {|k,v| v>1}; Math.send(:store,:c,9); begin; Math.size(); rescue; Math[:c]; end; [values,copy]; end", Vec::new(), &()).unwrap()
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, program)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn namespace_chained_addresses_distinguish_fields_from_native_results() {
    for flag in [false, true] {
        witness(
            "def run(flag:bool); Math.clear; Math[:keys]=[7] if flag; result=Math.keys.push(9); [Math[:keys],result]; end",
            &[Value::boolean(flag)],
            if flag {
                "[[7, 9], [7, 9]]"
            } else {
                "[nil, [9]]"
            },
            false,
        );
    }
    for (source, expected, warnings) in [
        (
            "def run; Math.replace({dup:7,items:[1]}); Math.dup().items.push(2); Math.items; end",
            "[1]",
            false,
        ),
        (
            "def run; Math[:dup]=Math::sqrt; seen=[]; begin; Math.dup.push(seen.push(9)); rescue; seen; end; end",
            "[9]",
            true,
        ),
        (
            "def run; Math.replace({items:[1]}); Math.items.push(2); Math.items; end",
            "[1, 2]",
            false,
        ),
        (
            "def run; Math.clear; Math.clear.keys.push(7); Math.keys; end",
            "[]",
            false,
        ),
        (
            "def run; Math.replace({a:1}); Math.tap {|x| x[:a]=7}.size; end",
            "1",
            false,
        ),
        (
            "def run; Math.replace({a:1}); Math.yield_self {|x| x[:a]+1}; end",
            "2",
            false,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_expanded_arguments_validate_the_selected_method() {
    for (source, expected, warnings) in [
        (
            "def run; Math[:clear]=Math::sqrt; Math.clear(*[9]); end",
            "3",
            false,
        ),
        (
            "def run; Math[:clear]=Math::sqrt; begin; Math.clear(9,bad:7); rescue; 7; end; end",
            "7",
            true,
        ),
        (
            "def run; Math[:clear]=7; begin; Math.clear(bad:7); rescue; 7; end; end",
            "7",
            true,
        ),
        (
            "def run; Math.clear; begin; Math.store(:a,1,bad:7); rescue; Math.size; end; end",
            "0",
            true,
        ),
        (
            "def run; Math.clear; Math.store(*[:a,1]); Math.delete(*[:a]); Math.size; end",
            "0",
            false,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_computed_calls_keep_conditional_descriptors() {
    for flag in [false, true] {
        for body in [
            "Math[:call]=flag ? Math::sqrt : 7; begin; Math.call(9); rescue; 7; end",
            "Math[:work]=flag ? Math::sqrt : 7; begin; Math[:work](9); rescue; 7; end",
            "Math[:work]=flag ? Math::sqrt : 7; begin; (Math::work)(9); rescue; 7; end",
        ] {
            witness(
                &format!("def run(flag:bool); {body}; end"),
                &[Value::boolean(flag)],
                if flag { "3" } else { "7" },
                true,
            );
        }
    }
}

#[test]
fn conditional_namespace_calls_preserve_overrides_and_failure_effects() {
    for flag in [false, true] {
        let args = [Value::boolean(flag)];
        witness(
            "def run(flag:bool); Math.clear; Math[:f]=7 if flag; seen=[]; begin; Math.f(seen.push(1)); rescue; seen; end; end",
            &args,
            if flag { "[1]" } else { "[]" },
            true,
        );
        witness(
            "def run(flag:bool); copy=Math; Math.replace({a:1}); Math[:size]=copy::sqrt if flag; begin; Math.send(:size,9); rescue; 7; end; end",
            &args,
            if flag { "3" } else { "7" },
            true,
        );
        witness(
            "def run(flag:bool); Math[:clear]=flag ? Math::sqrt : 7; begin; Math.clear(9); rescue; 7; end; end",
            &args,
            if flag { "3" } else { "7" },
            true,
        );
        witness(
            "def run(flag:bool); Math[:clear]=flag ? Math::sqrt : 7; begin; Math.send(:clear,9); rescue; 7; end; end",
            &args,
            if flag { "3" } else { "7" },
            true,
        );
        witness(
            "def run(flag:bool); Math.replace({a:1}); Math[:delete_if]=7 if flag; begin; Math.delete_if {|k,v| true}; Math.size; rescue; 7; end; end",
            &args,
            if flag { "7" } else { "0" },
            true,
        );
        witness(
            "def run(flag:bool); Math.clear; value=flag ? Math : nil; value.nil?; end",
            &args,
            if flag { "false" } else { "true" },
            false,
        );
    }
}

#[test]
fn namespace_receivers_remain_selected_during_argument_mutations() {
    for (source, expected) in [
        (
            "def run; Math[:size]=Math::sqrt; Math.size(begin; Math[:size]=7; 9; end); end",
            "3",
        ),
        (
            "def run; Math.replace({items:[1]}); result=Math.store(:n,begin; Math=7; 9; end); [Math,result]; end",
            "[7, 9]",
        ),
        (
            "def run; Math.replace({items:[1]}); Math.delete_if {|k,v| Math=7; true}; Math.keys; end",
            "[]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
    witness(
        "def run; Math[:clear]=Math::sqrt; begin; Math.clear(begin; Math[:clear]=7; 9; end); rescue; 7; end; end",
        &[],
        "7",
        true,
    );
}

#[test]
fn namespace_replacement_keeps_protected_values_and_nested_guards() {
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; Math.replace(m); begin; Math.clear; rescue; 7; end; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; Math.replace(m); begin; Math.captures.push(\"x\"); rescue; 7; end; end; end",
        "def run; m=/(a)/.match(\"a\"); h={}; if m; begin; h.replace(m); rescue; 7; end; end; end",
        "def run; begin; raise \"failure\"; rescue => e; Math.replace(e); begin; Math.clear; rescue; 7; end; end; end",
    ] {
        witness(source, &[], "7", true);
    }
}

#[test]
fn namespace_iteration_and_callback_overrides_use_the_selected_values() {
    for (source, expected, warnings) in [
        (
            "def run; Math.replace({a:1,b:2}); result=[]; for k,v in Math; result.push(v); end; result; end",
            "[1, 2]",
            false,
        ),
        (
            "def run; Math[:map]=Math::sqrt; Math.map(9); end",
            "3",
            false,
        ),
        (
            "def run; Math[:map]=Math::sqrt; begin; Math.map(9) {99}; rescue; 7; end; end",
            "7",
            true,
        ),
        ("def run; Math[:map]=7; Math.map; end", "7", false),
        (
            "def run; Math[:map]=7; begin; Math.map {99}; rescue; 7; end; end",
            "7",
            true,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_mutating_callbacks_keep_dispatch_and_control_flow() {
    for (source, expected, warnings) in [
        (
            "def run; Math.replace({a:1,b:2}); Math.delete_if {|k,v| v==1}; [Math.keys,Math.size]; end",
            "[[b], 1]",
            false,
        ),
        (
            "def run; Math.replace({a:1,b:2}); Math.keep_if {|k,v| v==1}; [Math.keys,Math.size]; end",
            "[[a], 1]",
            false,
        ),
        (
            "def run; Math.replace({a:1,b:2}); value=Math.delete_if {|k,v| break 7}; [value,Math.size]; end",
            "[7, 2]",
            false,
        ),
        (
            "def run; Math.replace({a:1,b:2}); Math.keep_if {|k,v| return 7}; 99; end",
            "7",
            false,
        ),
        (
            "def run; Math.clear; result=Math.delete(:missing) {|k| Math[:added]=1; 7}; [result,Math[:added]]; end",
            "[7, 1]",
            false,
        ),
        (
            "def run; Math[:delete_if]=7; Math.delete_if; end",
            "7",
            false,
        ),
        (
            "def run; Math[:delete_if]=Math::sqrt; begin; Math.delete_if(9) {99}; rescue; 7; end; end",
            "7",
            true,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_collection_outputs_keep_the_runtime_dispatch_kind() {
    for (source, expected) in [
        (
            "def run; Math.replace({size:7,a:1}); Math.merge({}).size; end",
            "2",
        ),
        (
            "def run; Math.replace({size:7,a:1}); Math.select {|k,v| true}.size; end",
            "2",
        ),
        (
            "def run; Math.replace({size:7,a:1}); Math.transform_values {|v| v}.size; end",
            "2",
        ),
        (
            "def run; Math.replace({size:7,a:1}); Math.deep_transform_keys {|k| k}.size; end",
            "2",
        ),
        (
            "def run; Math.replace({size:7,a:1}); Math.each {|k,v| nil}.size; end",
            "7",
        ),
        (
            "def run; Math.replace({size:7,a:1}); Math.dup.size; end",
            "7",
        ),
        (
            "def run; Math.replace({items:[1]})[:items].push(2); Math[:items]; end",
            "[1]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn namespace_conditional_fields_and_receivers_keep_every_dispatch_branch() {
    for (flag, expected) in [(false, "1"), (true, "7")] {
        witness(
            "def run(flag:bool); Math.replace({a:1}); Math[:size]=7 if flag; Math.size; end",
            &[Value::boolean(flag)],
            expected,
            false,
        );
    }
    for (flag, expected) in [(false, "7"), (true, "3")] {
        witness(
            "def run(flag:bool); Math[:size]=flag ? Math::sqrt : 7; begin; Math.size(9); rescue; 7; end; end",
            &[Value::boolean(flag)],
            expected,
            true,
        );
    }
    for (flag, expected) in [(false, "1"), (true, "7")] {
        witness(
            "def run(flag:bool); Math.replace({size:7}); value=flag ? Math : [1]; value.size; end",
            &[Value::boolean(flag)],
            expected,
            false,
        );
    }
}

#[test]
fn namespace_lookup_errors_observe_argument_evaluation_order() {
    for (source, expected) in [
        (
            "def run; Math.clear; seen=[]; begin; Math.nope(seen.push(1)); rescue; seen; end; end",
            "[]",
        ),
        (
            "def run; Math.clear; seen=[]; begin; Math::size(seen.push(1)); rescue; seen; end; end",
            "[1]",
        ),
        (
            "def run; Math.replace({size:7}); seen=[]; begin; Math.size(seen.push(1)); rescue; seen; end; end",
            "[1]",
        ),
        (
            "def run; Math.replace({clear:7}); seen=[]; begin; Math.clear(seen.push(1)); rescue; seen; end; end",
            "[1]",
        ),
    ] {
        witness(source, &[], expected, true);
    }
}

#[test]
fn namespace_mutators_preserve_object_dispatch_and_type_exports() {
    for (source, expected) in [
        (
            "def run; Math.clear; [Math.size,Math.empty?,Math.keys,Math.values]; end",
            "[0, true, [], []]",
        ),
        (
            "def run; Math.clear; Math.store(:n,4); [Math[:n],Math.delete(:n),Math.size]; end",
            "[4, 4, 0]",
        ),
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math.clear; Math.store(:State,Status); echo(:draft); end",
            "Status",
        ),
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math.replace({State:Status}); echo(:draft); end",
            "Status",
        ),
        (
            "def run; Math.clear; Math.store(:items,[1]); copy=Math; Math[:items].push(2); [Math[:items],copy[:items]]; end",
            "[[1, 2], [1]]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn namespace_fields_override_native_methods_with_runtime_call_rules() {
    for (source, expected, warnings) in [
        ("def run; Math[:size]=77; Math.size; end", "77", false),
        (
            "def run; Math[:size]=77; begin; Math.size(); rescue; 99; end; end",
            "99",
            true,
        ),
        (
            "def run; Math[:size]=Math::sqrt; Math.size(9); end",
            "3",
            false,
        ),
        (
            "def run; Math[:replace]=Math::sqrt; Math.replace(16); end",
            "4",
            false,
        ),
        (
            "def run; Math[:dup]=7; copy=Math.dup; copy[:x]=9; Math[:x]; end",
            "nil",
            false,
        ),
        (
            "def run; Math[:dup]=Math::sqrt; Math.dup(9); end",
            "3",
            false,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_scoped_and_indexed_fields_keep_call_and_read_boundaries() {
    for (source, expected, warnings) in [
        (
            "def run; Math.clear; Math[:n]=7; [Math::n,Math[:n]]; end",
            "[7, 7]",
            false,
        ),
        (
            "def run; Math[:alias]=Math::sqrt; [Math::alias(9),Math[:alias](16)]; end",
            "[3, 4]",
            false,
        ),
        (
            "def run; Math.clear; begin; Math::size; rescue; 99; end; end",
            "99",
            true,
        ),
        (
            "def run; Math.clear; Math[:n]=7; begin; Math::n(); rescue; 99; end; end",
            "99",
            true,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_native_callbacks_keep_writes_breaks_and_copied_receivers() {
    for (source, expected) in [
        (
            "def run; Math.replace({a:1,b:2}); Math.map {|k,v| v+1}; end",
            "[2, 3]",
        ),
        (
            "def run; Math.replace({a:1,b:2}); Math.each {|k,v| break v if v==2}; end",
            "2",
        ),
        (
            "def run; Math.replace({a:1,b:2}); result=Math.map {|k,v| Math[:c]=3; v}; [result,Math[:c]]; end",
            "[[1, 2], 3]",
        ),
        (
            "def run; Math.replace({a:1,b:2}); Math.merge({b:3}) {|k,a,b| a+b}; end",
            "{a: 1, b: 5}",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn namespace_forwarding_resolves_native_and_overridden_members() {
    for (source, expected, warnings) in [
        (
            "def run; Math.clear; Math.send(:store,:a,1); [Math.public_send(:size),Math.send(:fetch,:a)]; end",
            "[1, 1]",
            false,
        ),
        (
            "def run; Math[:size]=Math::sqrt; Math.send(:size,9); end",
            "3",
            false,
        ),
        (
            "def run; Math[:size]=7; begin; Math.send(:size); rescue; 99; end; end",
            "99",
            true,
        ),
        (
            "def run; Math.replace({a:1,b:2}); Math.public_send(:map) {|k,v| v*2}; end",
            "[2, 4]",
            false,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn namespace_dispatch_obeys_exact_and_interrupted_quotas() {
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
fn namespace_dispatch_keeps_cancellation_and_deadlines_latched() {
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
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
