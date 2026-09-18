use super::{
    arguments,
    calls::{self, World},
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    iteration_tests::inferred_runtime,
    lexical_tests::witness,
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine, Value};

fn expected(source: &str, output: &str, exact: bool, rejected: bool) {
    super::native_tests::witness(source, Some(output), rejected);
    witness(source, exact, rejected);
}

#[test]
fn forwarded_native_calls_resolve_names_and_nested_helpers() {
    for helper in ["send", "public_send"] {
        for call in [
            format!("[1,2].{helper}(:size)"),
            format!("[1,2].{helper}(\"send\",:public_send,:size)"),
            format!("[1,2].{helper}(*[:send,:size])"),
        ] {
            expected(&format!("def run; {call}; end"), "2", false, false);
        }
    }
    expected(
        "def run; [nil.send(:nil?),false.public_send(:nil?),/ab/.send(:source)]; end",
        "[true, false, ab]",
        true,
        false,
    );
}

#[test]
fn forwarded_reads_use_captured_receivers_and_mutators_use_live_addresses() {
    for helper in ["send", "public_send"] {
        for (body, output) in [
            ("a=[1,2]; r=a.HELPER(:include?,a.pop); [a,r]", "[[1], true]"),
            (
                "a=[1,2]; r=a.HELPER(:include?,[7].map {a.pop}.first); [a,r]",
                "[[1], true]",
            ),
            (
                "a=[1,2]; r=a.HELPER(begin; [7].map {a.clear}; :first; end); [a,r]",
                "[[], 1]",
            ),
            (
                "a=[1,2]; r=a.HELPER(:push,[7].map {a.pop}.first); [a,r]",
                "[[1, 2], [1, 2]]",
            ),
            (
                "a=[1,2]; r=a.HELPER(a.clear.empty? ? :first : :first); [a,r]",
                "[[], 1]",
            ),
            (
                "a=[1,2]; r=a.HELPER(:fetch,begin; a.clear; 0; end); [a,r]",
                "[[], 1]",
            ),
            (
                "a=[1,2]; r=a.HELPER(a.clear.empty? ? :size : :size); [a,r]",
                "[[], 2]",
            ),
            (
                "a=[1,2]; r=a.HELPER(:push,a.pop); [a,r]",
                "[[1, 2], [1, 2]]",
            ),
            (
                "a=[1,2]; r=a.HELPER(:push,begin; a=[4]; a; end); [a,r]",
                "[[4], [1, 2, [4]]]",
            ),
            (
                "a={x:[1,2]}; r=a.x.HELPER(:push,begin; a.x=[4]; a.x; end); [a,r]",
                "[{x: [4]}, [1, 2, [4]]]",
            ),
        ] {
            expected(
                &format!("def run; {}; end", body.replace("HELPER", helper)),
                output,
                !body.contains(":size"),
                false,
            );
        }
    }
}

#[test]
fn forwarded_blocks_keep_control_transfers_and_unused_blocks_inert() {
    for helper in ["send", "public_send"] {
        for body in [
            "seen=[]; r=[1,2].HELPER(:map) {|x| seen.push(x); x+3}; [r,seen]",
            "seen=[]; r=[1,2].HELPER(:map) {|x| seen.push(x); break 7}; [r,seen]",
            "[1,2].HELPER(:map) {|x| return x+3}; 99",
            "[1,2].HELPER(:map) {|x| next x+3}",
            "[1,2].HELPER(:size) {missing}",
            "a=[1]; a.HELPER(:push,2) {missing}; a",
            "\"aba\".HELPER(:gsub,/a/) {\"X\"}",
            "a=[1,2]; r=a.HELPER(a.clear.empty? ? :map : :map) {|x| x+3}; [a,r]",
        ] {
            witness(
                &format!("def run; {}; end", body.replace("HELPER", helper)),
                false,
                false,
            );
        }
    }
}

#[test]
fn forwarded_properties_are_read_before_non_callable_errors() {
    for expression in [
        "1.send(:seconds)",
        "Time.at(0).send(:year)",
        "{data:3}.send(:data)",
        "money(\"1.00 USD\").send(:currency)",
    ] {
        expected(
            &format!("def run; begin; {expression}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
}

#[test]
fn forwarded_callable_fields_keep_hash_lookup_precedence() {
    for (body, output) in [
        ("h={go:JSON::parse}; h.send(:go,\"[7]\")", "[7]"),
        ("{size:JSON::parse}.send(:size)", "1"),
        ("{send:JSON::parse}.send(:size)", "1"),
        ("JSON.send(:parse,\"[7]\")", "[7]"),
    ] {
        expected(&format!("def run; {body}; end"), output, false, false);
    }
}

#[test]
fn forwarded_signatures_fail_before_entering_blocks() {
    expected(
        "def run; [1].send(:size,extra:7) {missing}; end",
        "1",
        false,
        false,
    );
    for call in [
        "[1].send",
        "[1].send(7)",
        "[1].send(:missing)",
        "[1].send(:send)",
        "[1].send(:size,7)",
        "JSON.send(:parse,\"[7]\")",
    ] {
        expected(
            &format!(
                "def run; seen=[]; begin; {call} {{seen.push(9); missing}}; rescue RuntimeError; [7,seen]; end; end"
            ),
            "[7, []]",
            true,
            true,
        );
    }
}

#[test]
fn forwarded_name_unions_keep_all_possible_calls() {
    let source =
        "def run(flag:bool); name=if flag; :size; else; :first; end; [7,9].send(name); end";
    for flag in [false, true] {
        inferred_runtime(source, &[Value::boolean(flag)], false);
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert_ne!(report.returns, Atom::Never.fact());
    assert!(report.incomplete.data.is_empty() && report.issues.data.is_empty());
}

#[test]
fn forwarded_flags_distinguish_ignored_and_rejected_blocks_and_keywords() {
    for call in [
        "[1,2].send(:size,extra:7) {missing}",
        "[1,2].send(:include?,2,extra:7) {missing}",
        "/a/.send(:source,extra:7) {missing}",
        "/a/.send(:match?,\"a\") {missing}",
        "Time.at(0).send(:format,\"yyyy\") {missing}",
        "money(\"1.00 USD\").send(:format,extra:7) {missing}",
    ] {
        witness(&format!("def run; {call}; end"), false, false);
    }
    for call in [
        "[1].send(:nil?)",
        "[1].send(:dup)",
        "[1].send(:itself)",
        "/a/.send(:inspect)",
        "1.send(:to_s)",
        "Time.at(0).send(:to_s)",
        "money(\"1.00 USD\").send(:to_s)",
    ] {
        expected(
            &format!("def run; begin; {call} {{missing}}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
    for call in [
        "[1].send(:first,extra:7)",
        "[1].send(:push,2,extra:7)",
        "[1].send(:dup,extra:7)",
        "/a/.send(:match?,\"a\",extra:7)",
    ] {
        expected(
            &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
}

#[test]
fn stateless_builtin_descriptors_remain_callable_without_script_captures() {
    for body in [
        "f=JSON::parse; f(\"7\")",
        "f=JSON[:parse]; f(\"7\")",
        "f=Time::at; f(0) {missing}",
        "h={run:Time::at}; h.send(:run,0) {missing}",
        "h={run:JSON::parse}; h.send(:run,h.clear.empty? ? \"[3]\" : \"[4]\")",
        "h={\"send\":JSON::parse}; h.send(:size)",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
    expected(
        "def run; begin; f=JSON::parse; f(\"7\") {missing}; rescue RuntimeError; 7; end; end",
        "7",
        true,
        true,
    );
}

#[test]
fn forwarded_protected_receivers_preserve_mutation_guards() {
    for body in [
        "m=\"ab\".match(/(a)(b)/); m.captures.send(:first)",
        "m=\"ab\".match(/(a)(b)/); m.send(:dup).captures.send(:first)",
        "m=\"ab\".match(/(a)(b)/); m.send(:begin,1)",
        "m=\"ab\".match(/(a)(b)/); a=m.captures; a.send(:push,7)",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
    for call in [
        "m.send(:clear)",
        "m.captures.send(:push,7)",
        "m.send(:dup).captures.send(:push,7)",
        "m.send(:begin,1) {missing}",
    ] {
        expected(
            &format!(
                "def run; m=\"ab\".match(/(a)(b)/); begin; {call}; rescue RuntimeError; 7; end; end"
            ),
            "7",
            true,
            true,
        );
    }
}

#[test]
fn forwarded_callbacks_keep_pending_parents_and_completed_error_writes() {
    let mut count = 0;
    for helper in ["send", "public_send"] {
        for call in [":map", ":delete_if", ":fill"] {
            for change in [
                "a.push([3])",
                "a.pop",
                "a.shift",
                "a.clear",
                "a=[[8],[9]]",
                "a[-1]=[7]",
                "a[0].push(4)",
            ] {
                for exit in ["7", "break 7", "return 9", "raise \"bad\""] {
                    witness(
                        &format!(
                            "def run; a=[[1],[2]]; begin; r=a[-1].push([1].{helper}({call}) {{{change}; {exit}}}); [a,r]; rescue RuntimeError; a; end; end"
                        ),
                        false,
                        false,
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 168);
}

#[test]
fn forwarding_chains_are_iterative_on_the_default_stack() {
    let mut source = String::from("def run; [7].send(");
    for _ in 0..1024 {
        source.push_str(":send,:public_send,");
    }
    source.push_str(":first); end");
    expected(&source, "7", true, false);
}

#[test]
fn named_reducers_use_detached_native_dispatch() {
    for (body, output, exact) in [
        ("[[1],2,3].reduce(:push)", "[1, 2, 3]", true),
        ("[[7],:first].reduce(:send)", "7", true),
        ("[[7],0].reduce(:fetch)", "7", true),
        ("[[7],7].reduce(:include?)", "true", true),
        ("[JSON,\"7\"].reduce(:parse)", "7", false),
        ("a=[1]; b=[a,2].reduce(:push); [a,b]", "[[1], [1, 2]]", true),
        ("[[7,2],:+].reduce(:reduce)", "9", false),
        ("[[7],2].reduce([1],:push) {missing}", "[1, [7], 2]", true),
    ] {
        expected(&format!("def run; {body}; end"), output, exact, false);
    }
    for call in [
        "[[1],2].reduce(:missing)",
        "[[1],2].reduce(:size)",
        "[[1],:missing].reduce(:send)",
        "[[1],0].reduce(:map)",
    ] {
        expected(
            &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
}

fn supplied(source: &str, input: Value, exact: bool, rejected: bool) -> Value {
    let actual = Engine::new()
        .compile(source)
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .value;
    let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let input = literal_fact(&mut ctx, &mut facts, &input);
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            inputs: &[],
            source_owner: 0,
            program: &program,
            contracts: &[],
            hosts: &[],
            globals: &[],
        },
        program.names["run"],
        &[arguments::Input::Supplied(input)],
    )
    .unwrap_or_else(|e| panic!("{source}: {e}"));
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    let value = literal_fact(&mut ctx, &mut facts, &actual);
    assert_ne!(
        facts.relation(&mut ctx, value, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    if exact {
        assert_eq!(value, report.returns, "{source}: {report:?}");
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    actual
}

#[test]
fn nested_named_reducers_use_the_default_stack_at_full_value_depth() {
    for depth in [2, 8, 32, 128] {
        let mut value = Value::array(vec![Value::int(7), Value::int(2)]);
        for level in 1..depth {
            value = Value::array(vec![
                value,
                Value::symbol(if level == 1 { "+" } else { "reduce" }),
            ]);
        }
        let actual = supplied(
            "def run(value); value.reduce(:reduce); end",
            value,
            false,
            false,
        );
        assert_eq!(actual.as_int(), Some(9), "depth {depth}");
    }
}

#[test]
fn forwarded_raw_byte_names_use_hash_keys_before_utf8_validation() {
    for name in [b"raw\xff".as_slice(), b"raw\0", b"\xf0\x9f\x9a\x80"] {
        for name in [Value::bytes(name), Value::symbol(name)] {
            assert_eq!(
                supplied(
                    "def run(name); h={}; h[name]=JSON::parse; h.send(name,\"7\"); end",
                    name.clone(),
                    false,
                    false,
                )
                .as_int(),
                Some(7),
            );
            assert_eq!(
                supplied(
                    "def run(name); begin; [1].public_send(name); rescue RuntimeError; 9; end; end",
                    name.clone(),
                    true,
                    true,
                )
                .as_int(),
                Some(9),
            );
            assert_eq!(
                supplied(
                    "def run(name); h={}; h[name]=JSON::parse; [h,\"7\"].reduce(name); end",
                    name,
                    false,
                    false,
                )
                .as_int(),
                Some(7),
            );
        }
    }
}

#[test]
fn forwarded_splats_and_descriptor_copies_keep_native_contracts() {
    for source in [
        "def run; \"aba\".send(*[:gsub,\"a\",\"X\"],**{regex:false}); end",
        "def run; [1,2].send(*[:public_send,:first],**{}); end",
        "def apply(f); f(\"7\"); end; def run; apply(JSON::parse); end",
        "def run; a=[JSON::parse]; b=a.dup; b[0](\"7\"); end",
        "def run; h={call:JSON::parse}; copy=h.dup; copy[:call](\"7\"); end",
        "def run; JSON.values; end",
        "def run; JSON::parse; end",
        "def run; f=Time::now; f; end",
    ] {
        super::native_tests::witness(source, None, false);
    }
    for body in [
        "[1].send(:first,**{extra:7})",
        "f=JSON::parse; JSON.stringify([f])",
        "f=JSON.parse; f(\"7\")",
        "f=JSON::parse; f",
        "f=JSON::parse; h={call:f}; h",
        "JSON.stringify([JSON::parse])",
    ] {
        expected(
            &format!("def run; begin; {body}; rescue RuntimeError; 9; end; end"),
            "9",
            body != "JSON.stringify([JSON::parse])",
            true,
        );
    }
}

#[test]
fn forwarded_receiver_unions_and_joined_snapshots_keep_all_paths() {
    for source in [
        "def run(flag:bool); a=if flag; [7]; else; {a:9}; end; a.send(:size); end",
        "def run(flag:bool); a=[1,2]; r=a.send(begin; if flag; a.clear; else; a.pop; end; :first; end); [r,a]; end",
        "def run(flag:bool); a=[1,2]; r=a.send(:push,begin; if flag; a.clear; else; a.pop; end; 9; end); [r,a]; end",
        "def run(flag:bool); op=if flag; :push; else; :include?; end; [[7],2].reduce(op); end",
        "def run(flag:bool); value=if flag; [[[1],2],:push]; else; [[7,2],:+]; end; value.reduce(:reduce); end",
    ] {
        for flag in [false, true] {
            inferred_runtime(source, &[Value::boolean(flag)], false);
        }
    }
}

#[test]
fn nested_dispatch_errors_keep_pending_writes_rescue_retry_and_ensure() {
    for (index, body) in [
        "seen=[]; begin; a=[1]; a.push([[[1],0],:map].reduce(:reduce)); seen.push(99); rescue RuntimeError; seen.push(a); ensure; seen.push(7); end; seen",
        "a=[[1],[2]]; begin; a[-1].push([[[7],:missing],:send].reduce(:reduce)); rescue RuntimeError; a.push([9]); end; a",
        "n=0; a=[]; begin; n+=1; a.push(n); [1].send(:map) {raise \"again\" if n<2; 7}; rescue; retry if n<2; ensure; a.push(9); end; a",
        "a=[]; begin; [1].send(:map) {begin; return 7; ensure; a.push(9); end}; ensure; a.push(11); end",
        "seen=[]; [1].send(:map) {begin; [[7,2],:+].reduce(:reduce); break 9; ensure; seen.push(7); end}; seen",
        "seen=[]; [[[[1],2],:push],:first].reduce(:reduce) rescue seen.push(7); seen",
    ].into_iter().enumerate() {
        witness(&format!("def run; {body}; end"), false, matches!(index, 0 | 1 | 5));
    }
}

#[test]
fn unresolved_names_and_structural_object_dispatch_stay_explicit() {
    for source in [
        "def run(name:string); [1].send(name); end",
        "def run(value); value.send(:size); end",
        "def run(value:{call:int}); value.send(:call); end",
        "def run(value:hash<string,int>); value.send(:size); end",
        "def run(op:string); [[1],2].reduce(op); end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(
        ctx,
        &mut facts,
        "def run(xs:array<int>,flag:bool); seen=[]; r=xs.send(:public_send,:map) {|x| seen.push(x); x}; op=if flag; :first; else; :last; end; a=[1,2].send(op); b=[[[1],2],:push].reduce(:reduce); h={run:JSON::parse}; c=h.send(:run,\"7\"); [r,a,b,c,seen]; end",
    )?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    assert_ne!(report.returns, Atom::Never.fact());
    Ok(())
}

#[test]
fn dispatch_analysis_has_exact_quotas_and_failed_allocation_cleanup() {
    use crate::{ErrorKind, Limits};
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, error) in [
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
        let result = accounting(&mut ctx);
        assert_eq!(result.as_ref().err().map(|e| e.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by((stats.peak_memory_bytes / 64).max(1)) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for steps in (0..stats.steps).step_by((stats.steps as usize / 64).max(1)) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn dispatch_analysis_keeps_cancellation_and_deadlines_latched() {
    use crate::ErrorKind;
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        accounting(&mut ctx).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.options.cancellation.cancel();
        }
        let error = accounting(&mut ctx).unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn forwarded_universal_flags_match_every_modeled_native_receiver() {
    let mut count = 0;
    for receiver in [
        "nil",
        "false",
        "7",
        "7.5",
        "\"x\"",
        ":x",
        "[1]",
        "{x:1}",
        "(1..3)",
        "/a/",
        "Time.at(0)",
        "Duration.build(7)",
        "money(\"1.00 USD\")",
        "JSON",
    ] {
        for method in ["nil?", "itself", "dup"] {
            for helper in ["send", "public_send"] {
                for (arguments, block) in [("", "{seen.push(7); missing}"), (",extra:7", "")] {
                    expected(
                        &format!(
                            "def run; seen=[]; begin; {receiver}.{helper}(:{method}{arguments}) {block}; rescue RuntimeError; [9,seen]; end; end"
                        ),
                        "[9, []]",
                        true,
                        true,
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 168);
    for method in [
        "to_s", "string", "to_i", "to_f", "to_sym", "intern", "inspect", "squeeze",
    ] {
        for (arguments, block) in [("", "{seen.push(7); missing}"), (",extra:7", "")] {
            expected(
                &format!(
                    "def run; seen=[]; begin; \"7\".send(:{method}{arguments}) {block}; rescue RuntimeError; [9,seen]; end; end"
                ),
                "[9, []]",
                true,
                true,
            );
        }
    }
}

#[test]
fn descriptor_binding_reads_keep_auto_calls_and_known_invalid_union_arms() {
    for source in [
        "def run(flag:bool); f=if flag; Time::now; else; 7; end; f; end",
        "def run(flag:bool); f=if flag; Hash::new; else; 7; end; [1].map {f}; end",
    ] {
        for flag in [false, true] {
            inferred_runtime(source, &[Value::boolean(flag)], false);
        }
    }
    for source in [
        "def run(flag:bool); f=if flag; JSON::parse; else; 7; end; begin; f; rescue RuntimeError; 9; end; end",
        "def run(flag:bool); f=if flag; JSON::parse; else; 7; end; begin; [1].map {f}; rescue RuntimeError; 9; end; end",
    ] {
        for flag in [false, true] {
            inferred_runtime(source, &[Value::boolean(flag)], true);
        }
    }
}
