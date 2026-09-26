use super::{
    arguments,
    calls::{self, World},
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Engine, Value};

fn expected(source: &str, output: &str, exact: bool, warnings: bool) {
    super::native_tests::witness(source, Some(output), warnings);
    witness(source, exact, warnings);
}

#[test]
fn substitutions_distinguish_literal_patterns_regex_values_and_keywords() {
    for (call, output) in [
        (r#""ab.a".sub(".","X")"#, "abXa"),
        (r#""ab.a".gsub(".","X")"#, "abXa"),
        (r#""ab.a".sub(".","X",regex:true)"#, "Xb.a"),
        (r#""ab.a".gsub(".","X",regex:true)"#, "XXXX"),
        (r#""ab.a".gsub(/./,"X")"#, "XXXX"),
        (r#""ab.a".gsub(".","X",regex:false)"#, "abXa"),
        (r#""ab.a".gsub(".","X",**{regex:true})"#, "XXXX"),
        (r#""aba".gsub("a","$0\\1")"#, "$0\\1b$0\\1"),
    ] {
        expected(&format!("def run; {call}; end"), output, true, false);
    }
    for flag in [false, true] {
        inferred_runtime(
            "def run(flag:bool); \"ab.\".gsub(\".\",regex:flag) {\"X\"}; end",
            &[Value::boolean(flag)],
            false,
        );
        inferred_runtime(
            "def run(flag:bool); \"ab.\".gsub(\".\",\"X\",regex:flag); end",
            &[Value::boolean(flag)],
            false,
        );
    }
}

#[test]
fn replacement_templates_preserve_capture_and_named_reference_rules() {
    for (call, output) in [
        (
            r#""aba".gsub(/(a)(b)?/,"<\\0|\\1|\\2|\\+>")"#,
            "<ab|a|b|b><a|a||a>",
        ),
        (
            r#""aba".gsub(/(?<x>a)(b)?/,"<\\0|\\1|\\2|\\+>")"#,
            "<ab|||a><a|||a>",
        ),
        (r#""aba".gsub(/(?<x>a)|(?<x>b)/,"<\\k<x>>")"#, "<a><b><a>"),
        (r#""abc".sub(/b/,"<\\`|\\&|\\'>")"#, "a<a|b|c>c"),
        (r#""abc".sub(/z/,"\\k<missing>")"#, "abc"),
        (r#""abc".sub!(/z/,"\\k<missing>")"#, "nil"),
    ] {
        expected(&format!("def run; {call}; end"), output, true, false);
    }
    for replacement in [r#""\\k<missing>""#, r#""\\k<name""#] {
        expected(
            &format!(
                "def run; begin; \"a\".gsub(/a/,{replacement}); rescue RuntimeError; 7; end; end"
            ),
            "7",
            true,
            false,
        );
    }
}

#[test]
fn bang_results_follow_matches_without_mutating_the_receiver() {
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        witness(
            &format!(
                "def run; text=\"aba\"; alias=text; r=text.{method}(\"a\",\"X\"); [text,alias,r,text.{method}(\"z\",\"X\")]; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; text=\"aba\"; alias=text; r=text.{method}(\"a\") {{\"X\"}}; [text,alias,r,text.{method}(\"z\") {{missing}}]; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; text=\"aba\"; seen=[]; r=text.{method}(/a/) {{|x| text.clear; seen.push(x); \"X\"}}; [text,r,seen]; end"
            ),
            true,
            false,
        );
    }
    expected(
        "def run; [\"a\".sub!(\"a\",\"a\"),\"a\".gsub!(\"a\") {\"a\"},\"\".sub!(\"\",\"\")]; end",
        "[a, a, ]",
        true,
        false,
    );
}

#[test]
fn substitution_callbacks_receive_whole_matches_and_render_results() {
    expected(
        "def run; seen=[]; r=\"aba\".gsub(/(a)(b)?/) {|whole,extra| seen.push([whole,extra]); next 7}; [r,seen]; end",
        "[77, [[ab, nil], [a, nil]]]",
        true,
        false,
    );
    for (body, output, exact) in [
        ("nil", "", true),
        ("7", "7", true),
        ("true", "true", true),
        ("1.5", "1.5", true),
        (":x", "x", true),
        ("[1,nil,:x]", "[1, , x]", true),
        ("{a:[true]}", "{a: [true]}", true),
        ("{z:2,a:[true]}", "{z: 2, a: [true]}", false),
        ("/x/i", "/x/i", true),
        ("1..3", "1..3", true),
        ("Regexp", "<object>", true),
        ("\"a\".match(/a/)", "a", true),
        ("\"a\".match(/a/)[:begin]", "<builtin>", true),
    ] {
        expected(
            &format!("def run; \"a\".sub(/a/) {{{body}}}; end"),
            output,
            exact,
            false,
        );
    }
}

#[test]
fn substitution_schedules_keep_anchors_flags_and_zero_width_boundaries() {
    for (call, output) in [
        (r#""éa".gsub("") {"-"}"#, "-é-a-"),
        (r#""éa".gsub(//) {"-"}"#, "-é-a-"),
        (r#""abc".gsub(/a*/) {"X"}"#, "XbXcX"),
        (r#""ab".gsub(/^|$/) {"X"}"#, "XabX"),
        (r#""ab".gsub(/(?:^a$)/) {missing}"#, "ab"),
        (r#""a\nb".sub(/a.b/m) {"X"}"#, "X"),
        (r#""a".sub(/A/i) {"X"}"#, "X"),
        (r#""".gsub!(//) {""}"#, ""),
    ] {
        expected(&format!("def run; {call}; end"), output, true, false);
    }
}

#[test]
fn signatures_and_pattern_errors_precede_callback_effects() {
    for call in [
        r#""a".sub"#,
        r#""a".sub("a")"#,
        r#""a".sub("a",nil)"#,
        r#""a".sub(:a,"x")"#,
        r#""a".sub("a","x",extra:true)"#,
        r#""a".sub("a","x",regex:1)"#,
        r#""a".sub(/a/,"x",regex:false)"#,
        r#"7.gsub("a","x")"#,
    ] {
        expected(
            &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
    for call in [
        r#""a".sub("a","x")"#,
        r#""a".sub(nil)"#,
        r#""a".sub(/a/,regex:false)"#,
        r#""a".sub("a",extra:true)"#,
    ] {
        expected(
            &format!("def run; begin; {call} {{missing}}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
    for call in [
        r#""a".gsub("[",regex:true) {missing}"#,
        r#""".sub("[","x",regex:true)"#,
    ] {
        expected(
            &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            false,
        );
    }
    expected(
        "def run; seen=[]; begin; \"a\".sub(\"a\") {|x:int| seen.push(x)}; rescue RuntimeError; seen; end; end",
        "[]",
        true,
        true,
    );
}

#[test]
fn substitution_callbacks_preserve_break_next_return_and_cleanup() {
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        for pattern in ["\"a\"", "/a/"] {
            for body in [
                "seen.push(7); break 9",
                "seen.push(7); return 9",
                "seen.push(7); next 9",
                "begin; break 9; ensure; seen.push(7); end",
                "begin; return 9; ensure; seen.push(7); end",
                "begin; raise \"bad\"; ensure; break 9; end",
            ] {
                witness(
                    &format!(
                        "def run; seen=[]; r=\"aba\".{method}({pattern}) {{{body}}}; [r,seen]; end"
                    ),
                    true,
                    false,
                );
            }
        }
    }
}

#[test]
fn substitution_callbacks_preserve_rescue_retry_and_error_classes() {
    for class in [
        "RuntimeError",
        "StandardError",
        "AssertionError",
        "LimitError",
        "TypeError",
        "ZeroDivisionError",
        "LocalJumpError",
        "ArgumentError",
    ] {
        expected(
            &format!(
                "def run; seen=[]; begin; \"aa\".gsub(/a/) {{seen.push(7); raise {class}, \"bad\"}}; rescue {class}; seen; ensure; seen.push(9); end; end"
            ),
            "[7]",
            true,
            false,
        );
    }
    witness(
        "def run; seen=[]; first=true; begin; r=\"ab\".gsub(/./) {|x| seen.push(x); if first; first=false; raise \"bad\"; end; x}; rescue; retry; end; [r,seen]; end",
        false,
        false,
    );
}

#[test]
fn generic_substitution_inputs_converge_with_captured_writes() {
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        for pattern in ["\"a\"", "/a/", "\"\""] {
            for input in ["", "a", "aba", "zzz"] {
                inferred_runtime(
                    &format!(
                        "def run(text:string); seen=[]; r=text.{method}({pattern}) {{|x| seen=[seen,x]; \"X\"}}; [r,seen]; end"
                    ),
                    &[Value::bytes(input)],
                    false,
                );
            }
        }
        for input in ["a", "(a)", "[", "z"] {
            inferred_runtime(
                &format!(
                    "def run(pattern:string); seen=[]; begin; r=\"aba\".{method}(pattern,regex:true) {{|x| seen.push(x); :X}}; [r,seen]; rescue RuntimeError; seen; end; end"
                ),
                &[Value::bytes(input)],
                false,
            );
        }
    }
}

#[test]
fn generic_replacements_remain_strings_and_validate_unused_values() {
    for source in [
        "def run(replacement:string); \"aba\".sub(\"a\",replacement); end",
        "def run(replacement:string); \"aba\".gsub!(/a/,replacement); end",
        "def run(replacement:string); \"aba\".gsub!(\"z\",replacement); end",
        "def run(replacement:any); begin; \"aba\".gsub(/a/) {replacement}; rescue LimitError; 7; end; end",
    ] {
        inferred_runtime(source, &[Value::bytes("X")], false);
    }
    for value in [
        Value::nil(),
        Value::int(7),
        Value::array(vec![Value::int(1), Value::nil()]),
    ] {
        inferred_runtime(
            "def run(value:any); \"aba\".gsub(/a/) {value}; end",
            &[value],
            false,
        );
    }
}

#[test]
fn substitution_results_keep_nested_pending_addresses() {
    let mut count = 0;
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        for pattern in ["\"a\"", "/a/"] {
            for change in [
                "a.push([3])",
                "a.prepend([3])",
                "a.clear",
                "a.push([9],[8])",
                "a=[[9]]",
                "a[0]=[7]",
                "a.pop",
                "a.shift",
                "a.insert(0,[7])",
                "a[-1].clear",
                "a[-1].push(3)",
                "a[-1]=[9]",
            ] {
                for exit in ["7", "break 7", "return 9"] {
                    witness(
                        &format!(
                            "def run; a=[[1],[2]]; r=a[-1].push(begin; \"aba\".{method}({pattern}) {{{change}; {exit}}}; end); [a,r]; end"
                        ),
                        false,
                        false,
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 288);
}

fn supplied(source: &str, input: Value, exact: bool) -> (Value, u64) {
    supplied_options(source, input, exact, CallOptions::default())
}

fn fixed_limit_supplied(source: &str, input: Value, exact: bool) -> (Value, u64) {
    // Literal searches over 1 MiB exceed the default work budget before the size guard.
    // Keep the default memory limit and test work exhaustion separately.
    supplied_options(
        source,
        input,
        exact,
        CallOptions {
            limits: crate::Limits {
                steps: None,
                ..crate::Limits::default()
            },
            ..CallOptions::default()
        },
    )
}

fn supplied_options(source: &str, input: Value, exact: bool, options: CallOptions) -> (Value, u64) {
    let actual = Engine::legacy_unchecked()
        .compile(source)
        .unwrap()
        .call("run", std::slice::from_ref(&input), options.clone())
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .value;
    let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(options);
    let mut facts = Facts::new(&mut ctx).unwrap();
    let input = literal_fact(&mut ctx, &mut facts, &input);
    let before = ctx.stats().steps;
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
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
    let steps = ctx.stats().steps - before;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{source}: {report:?}"
    );
    let value = literal_fact(&mut ctx, &mut facts, &actual);
    assert_ne!(
        facts.relation(&mut ctx, value, report.returns).unwrap(),
        super::relation::Relation::Rejected,
        "{source}: {report:?}"
    );
    if exact {
        assert_eq!(value, report.returns, "{source}: {report:?}");
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    (actual, steps)
}

#[test]
fn raw_byte_substitution_preserves_literal_and_regex_windows() {
    for source in [
        "def run(text); text.gsub(/./) {|x| x}; end",
        "def run(text); text.gsub(\"\") {\"-\"}; end",
        "def run(text); text.gsub(//) {\"-\"}; end",
        "def run(text); text.sub(\"a\",\"X\"); end",
    ] {
        supplied(source, Value::bytes(b"a\xff\0\xc3\xa9"), true);
    }
}

#[test]
fn literal_no_match_limits_differ_between_blocks_and_replacements() {
    let input = Value::bytes(vec![b'a'; crate::regex::MAX_TEXT + 1]);
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        let (result, _) = fixed_limit_supplied(
            &format!(
                "def run(text); begin; text.{method}(\"z\") {{missing}}; rescue LimitError; 7; end; end"
            ),
            input.clone(),
            true,
        );
        assert_eq!(result.as_int(), Some(7));
        let (result, _) = fixed_limit_supplied(
            &format!("def run(text); text.{method}(\"z\",text); end"),
            input.clone(),
            true,
        );
        if method.ends_with('!') {
            assert!(matches!(result.0, crate::value::Kind::Nil));
        } else {
            assert_eq!(result.as_bytes().unwrap().len(), crate::regex::MAX_TEXT + 1);
        }
    }
}

#[test]
fn large_literal_matches_can_shrink_before_output_validation() {
    let input = Value::bytes(vec![b'a'; crate::regex::MAX_TEXT + 1]);
    for call in [
        "text.sub(text) {\"X\"}",
        "text.gsub(text) {\"X\"}",
        "text.sub(text,\"X\")",
        "text.gsub(text,\"X\")",
    ] {
        let (result, _) =
            fixed_limit_supplied(&format!("def run(text); {call}; end"), input.clone(), true);
        assert_eq!(result.as_bytes(), Some(b"X".as_slice()));
    }
}

#[test]
fn prefix_suffix_and_replacement_limits_keep_completed_callback_writes() {
    let mut input = vec![b'x'; crate::regex::MAX_TEXT + 1];
    input.push(b'a');
    let (result, _) = fixed_limit_supplied(
        "def run(text); seen=[]; begin; text.sub(\"a\") {seen.push(7); \"X\"}; rescue LimitError; seen; end; end",
        Value::bytes(input),
        true,
    );
    assert!(result.as_array().unwrap().is_empty());
    let mut input = vec![b'x'; crate::regex::MAX_TEXT];
    input[0] = b'a';
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        for pattern in ["\"a\"", "/a/"] {
            let (result, _) = fixed_limit_supplied(
                &format!(
                    "def run(text); seen=[]; begin; text.{method}({pattern}) {{seen.push(7); \"XX\"}}; rescue LimitError; seen; end; end"
                ),
                Value::bytes(input.as_slice()),
                true,
            );
            assert_eq!(result.to_string(), "[7]");
        }
    }
    let (result, _) = fixed_limit_supplied(
        "def run(text); seen=[]; begin; \"aa\".gsub(/a/) {seen.push(7); text}; rescue LimitError; seen; end; end",
        Value::bytes(vec![b'x'; crate::regex::MAX_TEXT + 1]),
        true,
    );
    assert_eq!(result.to_string(), "[7]");
}

#[test]
fn regex_input_guards_precede_matching_and_callback_dispatch() {
    for call in [
        "text.sub(/a/) {missing}",
        "text.sub(\"a\",regex:true) {missing}",
        "\"a\".sub(/z/,text)",
        "\"a\".sub(text,regex:true) {missing}",
    ] {
        let (result, _) = supplied(
            &format!("def run(text); begin; {call}; rescue LimitError; 7; end; end"),
            Value::bytes(vec![b'a'; crate::regex::MAX_TEXT + 1]),
            true,
        );
        assert_eq!(result.as_int(), Some(7));
    }
}

#[test]
fn rendering_uses_an_accounted_stack_at_the_full_value_depth() {
    let mut input = Value::int(7);
    for _ in 0..128 {
        input = Value::array(vec![input]);
    }
    let (result, _) = supplied("def run(value); \"a\".sub(/a/) {value}; end", input, true);
    assert_eq!(result.as_bytes().unwrap().len(), 257);
}

#[test]
fn shared_rendered_values_fail_before_later_callbacks() {
    let mut input = Value::bytes(vec![b'x'; 16384]);
    for _ in 0..7 {
        input = Value::array(vec![input.clone(), input]);
    }
    let (result, _) = supplied(
        "def run(value); seen=[]; begin; \"aa\".gsub(/a/) {seen.push(7); value}; rescue LimitError; seen; end; end",
        input,
        true,
    );
    assert_eq!(result.to_string(), "[7]");
}

#[test]
fn work_exhaustion_cannot_be_rescued_as_an_output_limit() {
    // Scanning the oversized subject costs about 16,400 steps before the
    // output limit could apply.
    let steps = 8_000;
    for call in ["text.sub(\"z\") {7}", "text.sub(text) {7}"] {
        let source = format!("def run(text); begin; {call}; rescue LimitError; 9; end; end");
        let input = Value::bytes(vec![b'a'; crate::regex::MAX_TEXT + 1]);
        let error = Engine::legacy_unchecked()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: crate::Limits {
                        steps: Some(steps),
                        ..crate::Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Steps);
        let program = crate::bytecode::compile(&source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let input = literal_fact(&mut ctx, &mut facts, &input);
        ctx.options.limits.steps = Some(ctx.stats().steps + steps);
        let error = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
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
        .unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Steps);
        assert!(ctx.exhausted());
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn rendering_unions_preserves_possible_text_and_common_exact_results() {
    for body in [
        "value=if flag; [7]; else; :x; end; [value]",
        "value=if flag; {a:7}; else; nil; end; value",
        "value=if flag; 7; else; \"7\"; end; [value]",
    ] {
        let source = format!("def run(flag:bool); \"a\".sub(/a/) {{{body}}}; end");
        for flag in [false, true] {
            inferred_runtime(&source, &[Value::boolean(flag)], false);
        }
    }
    let source =
        "def run(flag:bool); value=if flag; 7; else; \"7\"; end; \"a\".sub(/a/) {[value]}; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.issues.data.is_empty() && report.incomplete.data.is_empty());
    assert_eq!(report.returns, facts.string(&mut ctx, b"[7]").unwrap());
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn shared_rendered_values_within_the_limit_keep_exact_text() {
    let mut input = Value::bytes(vec![b'x'; 8192]);
    for _ in 0..6 {
        input = Value::array(vec![input.clone(), input]);
    }
    let (result, steps) = supplied("def run(value); \"a\".sub(/a/) {value}; end", input, true);
    assert_eq!(result.as_bytes().unwrap().len(), 524540);
    assert!(steps < 500_000, "{steps}");
}

#[test]
fn early_transfers_skip_remaining_substitution_work() {
    let mut input = vec![b'x'; 512 << 10];
    input[0] = b'a';
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        let (result, steps) = supplied(
            &format!("def run(text); text.{method}(\"a\") {{break 7}}; end"),
            Value::bytes(input.as_slice()),
            true,
        );
        assert_eq!(result.as_int(), Some(7));
        assert!(steps < 20_000, "{method}: {steps}");
    }
}

#[test]
fn nested_substitution_callbacks_use_the_default_stack() {
    let mut body = "seen.push(7); 9".to_string();
    for _ in 0..24 {
        body = format!("\"a\".sub(/a/) {{{body}}}");
    }
    witness(
        &format!("def run; seen=[]; r=begin; {body}; end; [r,seen]; end"),
        true,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(
        ctx,
        &mut facts,
        "def run(text:string,flag:bool); seen=[]; a=\"aba\".gsub(\"a\",regex:flag) {|x| seen.push(x); [7,nil,:x]}; b=text.gsub!(/a/) {|x| seen=[seen,x]; {a:7,b:nil}}; c=\"aba\".sub(/a/,\"X\"); [a,b,c,seen]; end",
    )?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    assert_ne!(report.returns, Atom::Never.fact());
    Ok(())
}

#[test]
fn substitution_analysis_has_exact_quotas_and_failed_allocation_cleanup() {
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
fn substitution_analysis_keeps_cancellation_and_deadlines_latched() {
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
