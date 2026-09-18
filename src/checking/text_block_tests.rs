use super::{
    arguments,
    calls::{self, World},
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Engine, Value};

fn expected(source: &str, display: &str, exact: bool, warnings: bool) {
    super::native_tests::witness(source, Some(display), warnings);
    witness(source, exact, warnings);
}

#[test]
fn text_callbacks_distinguish_bytes_runes_and_complete_lines() {
    for (method, output) in [
        ("each_byte", "[65, 195, 169, 10, 122]"),
        ("each_codepoint", "[65, 233, 10, 122]"),
        ("each_char", "[A, é, \n, z]"),
        ("each_line", "[Aé\n, z]"),
    ] {
        expected(
            &format!("def run; seen=[]; \"Aé\\nz\".{method} {{|x| seen.push(x)}}; seen; end"),
            output,
            true,
            false,
        );
    }
    for method in ["each_byte", "each_codepoint", "each_char", "each_line"] {
        expected(
            &format!("def run; \"\".{method} {{missing}}; end"),
            "",
            true,
            false,
        );
    }
    for (text, output) in [
        ("a\\n", "[a\n]"),
        ("a\\nb\\n", "[a\n, b\n]"),
        ("\\r\\n", "[\r\n]"),
    ] {
        expected(
            &format!("def run; seen=[]; \"{text}\".each_line {{|x| seen.push(x)}}; seen; end"),
            output,
            true,
            false,
        );
    }
}

#[test]
fn materializers_ignore_blocks_and_reject_arguments_and_keywords() {
    for (method, output) in [
        ("bytes", "[65, 195, 169, 10]"),
        ("codepoints", "[65, 233, 10]"),
        ("chars", "[A, é, \n]"),
        ("lines", "[Aé\n]"),
    ] {
        expected(
            &format!("def run; \"Aé\\n\".{method} {{missing}}; end"),
            output,
            true,
            false,
        );
        expected(&format!("def run; \"\".{method}; end"), "[]", true, false);
        for arguments in ["1", "chomp:true"] {
            expected(
                &format!(
                    "def run; begin; \"a\".{method}({arguments}) {{missing}}; rescue RuntimeError; 7; end; end"
                ),
                "7",
                true,
                true,
            );
        }
    }
}

#[test]
fn materialized_arrays_mutate_as_detached_values() {
    for method in ["chars", "bytes", "codepoints", "lines"] {
        for expression in [
            format!("text.{method}.push(7)"),
            format!("text.{method}().push(7)"),
            format!("text.{method} {{missing}}.push(7)"),
        ] {
            witness(
                &format!("def run; text=\"ab\"; r={expression}; [text,r]; end"),
                true,
                false,
            );
        }
    }
    for expression in [
        "\"a\".match(/(a)/).captures.push(7)",
        "\"a\".match(/(a)/) {|m| m}.captures.push(7)",
    ] {
        expected(
            &format!("def run; begin; {expression}; rescue RuntimeError; 9; end; end"),
            "9",
            true,
            true,
        );
    }
}

#[test]
fn text_iterators_validate_arguments_before_entering_a_block() {
    for method in ["each_byte", "each_codepoint", "each_char", "each_line"] {
        for text in ["", "a"] {
            for call in [
                format!("\"{text}\".{method}"),
                format!("\"{text}\".{method}(1) {{missing}}"),
                format!("\"{text}\".{method}(chomp:true) {{missing}}"),
                format!("7.{method} {{missing}}"),
            ] {
                expected(
                    &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
                    "7",
                    true,
                    true,
                );
            }
        }
    }
    for call in [
        "\"a\".each_char {|x:int| missing}",
        "\"a\".each_line {|x:int| missing}",
        "\"a\".each_byte {|x:string| missing}",
        "\"a\".each_codepoint {|x:string| missing}",
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
fn text_iteration_preserves_receiver_snapshots_and_callback_arity() {
    for method in ["each_byte", "each_codepoint", "each_char", "each_line"] {
        witness(
            &format!(
                "def run; text=\"a\\nb\"; alias=text; seen=[]; r=text.{method} {{|x,y| text=\"changed\"; seen.push([x,y]); next 9}}; [text,alias,r,seen]; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; text=\"a\\nb\"; seen=[]; r=text.{method} {{|x| text.clear; seen.push(x)}}; [text,r,seen]; end"
            ),
            true,
            false,
        );
    }
}

#[test]
fn generic_text_iteration_converges_with_captures_and_nested_results() {
    for method in ["each_byte", "each_codepoint", "each_char", "each_line"] {
        for text in ["", "a", "a\nb\n"] {
            inferred_runtime(
                &format!(
                    "def run(text:string); seen=[]; r=text.{method} {{|x| seen=[seen,x]}}; [r,seen]; end"
                ),
                &[Value::bytes(text)],
                false,
            );
        }
    }
    for method in ["bytes", "codepoints", "chars", "lines"] {
        inferred_runtime(
            &format!("def run(text:string); text.{method} {{missing}}; end"),
            &[Value::bytes("aé\nz")],
            false,
        );
    }
}

#[test]
fn literal_match_preserves_protected_capture_data_and_rune_offsets() {
    expected(
        "def run; \"éabz\".match(/(?P<first>a)(b)?/) {|m| [m.to_s,m.captures,m.named_captures,m.pre_match,m.post_match,m.begin(0),m.end(0),m.begin(-1),m.end(-1)]}; end",
        "[ab, [a, b], {first: a}, é, z, 1, 3, 2, 3]",
        true,
        false,
    );
    expected(
        "def run; \"a\".match(/(?P<first>a)(b)?/) {|m| [m.captures,m.begin(2),m.end(2)]}; end",
        "[[a, nil], nil, nil]",
        true,
        false,
    );
    expected(
        "def run; \"a\".match(/(a)/) {|m| c=m.captures; c.push(\"x\"); [c,m.captures,m.dup.captures]}; end",
        "[[a, x], [a], [a]]",
        true,
        false,
    );
    for mutation in [
        "m.captures.push(\"x\")",
        "m.captures.clear",
        "m.captures[0]=\"x\"",
        "m.dup.captures.push(\"x\")",
    ] {
        expected(
            &format!(
                "def run; \"a\".match(/(a)/) {{|m| begin; {mutation}; rescue RuntimeError; m.captures; end}}; end"
            ),
            "[a]",
            true,
            true,
        );
    }
}

#[test]
fn match_and_scan_have_distinct_callback_results_and_capture_schedules() {
    expected(
        "def run; [\"ab\".match(/a/) {7}, \"ab\".scan(/a/) {7}, \"ab\".match(/z/) {missing}]; end",
        "[7, ab, nil]",
        true,
        false,
    );
    for (pattern, output) in [
        ("/./", "[a, b]"),
        ("/(.)/", "[[a], [b]]"),
        ("/(a)|(b)/", "[[a, nil], [nil, b]]"),
        ("//", "[, , ]"),
        ("/a*/", "[a, ]"),
        ("/z/", "[]"),
    ] {
        expected(
            &format!("def run; seen=[]; \"ab\".scan({pattern}) {{|x| seen.push(x)}}; seen; end"),
            output,
            true,
            false,
        );
        expected(
            &format!("def run; \"ab\".scan({pattern}); end"),
            output,
            true,
            false,
        );
    }
    expected(
        "def run; seen=[]; \"ab\".scan(/(a)(b)/) {|a,b| seen.push([a,b])}; seen; end",
        "[[a, b]]",
        true,
        false,
    );
}

#[test]
fn regex_matching_honors_anchors_flags_and_zero_width_unicode_boundaries() {
    for (pattern, text, output) in [
        ("/(?:^a$)/", "ab", "[]"),
        ("/(?m:^a$)/", "a\\nb", "[a]"),
        ("/A/i", "a", "[a]"),
        ("//", "é🐈", "[, , ]"),
        ("/a*/", "aa", "[aa]"),
    ] {
        expected(
            &format!("def run; \"{text}\".scan({pattern}); end"),
            output,
            true,
            false,
        );
    }
    expected(
        "def run; \"ab\".match(\"(?:^a$)\") {missing}; end",
        "nil",
        true,
        false,
    );
}

#[test]
fn match_offsets_use_runes_and_truncate_finite_floats() {
    for (offset, output) in [
        ("0", "é"),
        ("1", "a"),
        ("1.9", "a"),
        ("-1", "b"),
        ("-1.9", "b"),
        ("-4", "nil"),
        ("999", "nil"),
    ] {
        expected(
            &format!("def run; \"éab\".match(/./,{offset}) {{|m| m.to_s}}; end"),
            output,
            true,
            false,
        );
    }
    expected(
        "def run; \"éab\".match(//,999) {|m| [m.to_s,m.begin(0),m.end(0)]}; end",
        "[, 3, 3]",
        true,
        false,
    );
    for offset in ["nil", "\"1\"", "1e30", "-1e30"] {
        expected(
            &format!(
                "def run; begin; \"a\".match(/./,{offset}) {{missing}}; rescue RuntimeError; 7; end; end"
            ),
            "7",
            true,
            true,
        );
    }
}

#[test]
fn matching_rejects_bad_patterns_and_signatures_before_callbacks() {
    for call in [
        "\"a\".match",
        "\"a\".match(/a/,0,1)",
        "\"a\".match(/a/,bad:true)",
        "\"a\".scan",
        "\"a\".scan(/a/,0)",
        "\"a\".scan(/a/,bad:true)",
        "\"a\".match(:a)",
        "\"a\".scan(nil)",
        "7.match(/a/)",
    ] {
        expected(
            &format!("def run; begin; {call} {{missing}}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            true,
        );
    }
    for call in [
        "\"a\".match(\"[\")",
        "\"a\".scan(\"(\")",
        "\"\".scan(\"[\")",
    ] {
        expected(
            &format!("def run; begin; {call} {{missing}}; rescue RuntimeError; 7; end; end"),
            "7",
            true,
            false,
        );
    }
}

#[test]
fn regex_values_ignore_match_blocks_and_namespace_helpers_reject_them() {
    witness(
        "def run; m=/a/.match(\"ab\") {missing}; if m; m.to_s; else; nil; end; end",
        false,
        false,
    );
    witness("def run; Regex.match(\"a\",\"ab\"); end", false, false);
    expected(
        "def run; begin; Regex.match(\"a\",\"ab\") {missing}; rescue RuntimeError; 7; end; end",
        "7",
        true,
        true,
    );
    expected("def run; {match:7}.match; end", "7", true, false);
}

#[test]
fn generic_regex_inputs_keep_optional_captures_and_matching_paths() {
    for source in [
        "def run(text:string); seen=[]; r=text.scan(/(a)(b)?/) {|x| seen.push(x)}; [r,seen]; end",
        "def run(text:string); text.match(/(?P<first>a)(b)?/) {|m| [m.captures,m.named_captures,m.begin(0),m.end(1)]}; end",
        "def run(text:string); text.scan(/(a)(b)?/); end",
    ] {
        for text in ["", "a", "ab", "aba"] {
            inferred_runtime(source, &[Value::bytes(text)], false);
        }
    }
    for method in ["match", "scan"] {
        let body = if method == "match" { "x.captures" } else { "x" };
        for pattern in ["a", "(a)(b)?", "[", "z"] {
            inferred_runtime(
                &format!(
                    "def run(pattern:string); seen=[]; begin; r=\"aba\".{method}(pattern) {{|x| seen.push({body})}}; [r,seen]; rescue RuntimeError; seen; end; end"
                ),
                &[Value::bytes(pattern)],
                false,
            );
        }
    }
}

#[test]
fn generic_matching_offsets_and_pattern_unions_include_runtime_witnesses() {
    for offset in [-9, -1, 0, 2, 99] {
        inferred_runtime(
            "def run(offset:int); \"aba\".match(/a/,offset) {|m| m.to_s}; end",
            &[Value::int(offset)],
            false,
        );
    }
    for flag in [false, true] {
        inferred_runtime(
            "def run(flag:bool); pattern_choice=if flag; /a/; else; \"(b)\"; end; out=[]; \"aba\".scan(pattern_choice) {|m| out.push(m)}; out; end",
            &[Value::boolean(flag)],
            false,
        );
    }
}

#[test]
fn text_callbacks_preserve_break_next_return_and_cleanup() {
    for call in [
        "\"ab\".each_char",
        "\"ab\".each_byte",
        "\"ab\".each_codepoint",
        "\"a\\nb\".each_line",
        "\"ab\".match(/a/)",
        "\"ab\".scan(/./)",
    ] {
        for body in [
            "seen.push(7); break 9",
            "seen.push(7); return 9",
            "seen.push(7); next 9",
            "begin; break 9; ensure; seen.push(7); end",
            "begin; return 9; ensure; seen.push(7); end",
            "begin; raise \"bad\"; ensure; break 9; end",
        ] {
            witness(
                &format!("def run; seen=[]; r={call} {{{body}}}; [r,seen]; end"),
                true,
                false,
            );
        }
    }
}

#[test]
fn text_callbacks_preserve_rescue_retry_and_every_ordinary_error() {
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
        for call in ["\"ab\".each_char", "\"ab\".match(/a/)", "\"ab\".scan(/./)"] {
            expected(
                &format!(
                    "def run; seen=[]; begin; {call} {{seen.push(7); raise {class}, \"bad\"}}; rescue {class}; seen; ensure; seen.push(9); end; end"
                ),
                "[7]",
                true,
                false,
            );
        }
    }
    witness(
        "def run; first=true; seen=[]; begin; \"ab\".scan(/./) {|x| seen.push(x); if first; first=false; raise \"bad\"; end}; rescue; retry; end; seen; end",
        false,
        false,
    );
}

#[test]
fn text_callbacks_preserve_nested_pending_mutation_addresses() {
    let mut count = 0;
    for call in [
        "\"ab\".each_char",
        "\"ab\".each_byte",
        "\"ab\".each_codepoint",
        "\"a\\nb\".each_line",
        "\"ab\".match(/a/)",
        "\"ab\".scan(/./)",
    ] {
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
                        "def run; a=[[1],[2]]; r=a[-1].push(begin; {call} {{{change}; {exit}}}; end); [a,r]; end"
                    ),
                    false,
                    false,
                );
                count += 1;
            }
        }
    }
    assert_eq!(count, 216);
}

fn supplied_text(source: &str, text: &[u8], expected: Option<&str>) -> u64 {
    let actual = Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[Value::bytes(text)], CallOptions::default())
        .unwrap()
        .value;
    if let Some(expected) = expected {
        assert_eq!(actual.to_string(), expected);
    }
    let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let input = facts.string(&mut ctx, text).unwrap();
    let before = ctx.stats().steps;
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
    .unwrap();
    let steps = ctx.stats().steps - before;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{source}: {report:?}"
    );
    let actual = literal_fact(&mut ctx, &mut facts, &actual);
    assert_eq!(actual, report.returns, "{source}: {report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    steps
}

#[test]
fn raw_input_facts_preserve_invalid_utf8_and_embedded_nuls() {
    let input = b"A\xc3\xa9\xff\n\0b";
    for method in ["each_char", "each_byte", "each_codepoint", "each_line"] {
        supplied_text(
            &format!("def run(text); seen=[]; text.{method} {{|x| seen.push(x)}}; seen; end"),
            input,
            None,
        );
    }
    for method in ["chars", "bytes", "codepoints", "lines"] {
        supplied_text(&format!("def run(text); text.{method}; end"), input, None);
    }
    supplied_text("def run(text); text.scan(/./); end", input, None);
    supplied_text(
        "def run(text); text.match(/b/) {|m| [m.to_s,m.begin(0),m.end(0)]}; end",
        input,
        Some("[b, 5, 6]"),
    );
}

#[test]
fn regex_input_guards_stay_catchable_and_precede_later_validation() {
    supplied_text(
        "def run(text); begin; text.match(\"[\") {missing}; rescue LimitError; 7; rescue RuntimeError; 9; end; end",
        &vec![b'a'; crate::regex::MAX_TEXT + 1],
        Some("7"),
    );
    supplied_text(
        "def run(text); begin; \"a\".match(text,\"bad\") {missing}; rescue LimitError; 7; rescue RuntimeError; 9; end; end",
        &vec![b'a'; crate::regex::MAX_PATTERN + 1],
        Some("7"),
    );
    let result = inferred_runtime(
        "def run(pattern:string); begin; \"a\".match(pattern,\"bad\") {missing}; rescue LimitError; 7; rescue RuntimeError; 9; end; end",
        &[Value::bytes(vec![b'a'; crate::regex::MAX_PATTERN + 1])],
        true,
    );
    assert_eq!(result.as_int(), Some(7));
}

#[test]
fn early_breaks_skip_later_literal_text_and_regex_matches() {
    let mut input = vec![b'x'; 512 << 10];
    input[0] = b'A';
    input[1] = b'\n';
    for (call, output) in [
        ("each_char", "A"),
        ("each_byte", "65"),
        ("each_codepoint", "65"),
        ("each_line", "A\n"),
        ("scan(/./)", "A"),
    ] {
        let steps = supplied_text(
            &format!("def run(text); text.{call} {{|x| break x}}; end"),
            &input,
            Some(output),
        );
        assert!(steps < 20_000, "{call}: {steps}");
    }
}

#[test]
fn nested_text_callbacks_keep_the_default_stack() {
    let mut body = "seen.push(7); 9".to_string();
    for index in 0..24 {
        let call = if index % 2 == 0 {
            "\"a\".scan(/a/)"
        } else {
            "\"a\".each_char"
        };
        body = format!("{call} {{{body}}}");
    }
    witness(
        &format!("def run; seen=[]; r=begin; {body}; end; [r,seen]; end"),
        true,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(text:string,flag:bool); seen=[]; a=\"ab\".scan(/(a)|(b)/) {|x| seen.push(x)}; b=text.each_line {|x| if flag; seen=[seen,x]; else; next 9; end}; c=text.match(/(?P<first>a)(b)?/) {|m| seen.push(m.captures); m.begin(0)}; d=\"ab\".chars; e=text.codepoints; [a,b,c,d,e,seen]; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    assert_ne!(report.returns, Atom::Never.fact());
    Ok(())
}

#[test]
fn text_callback_analysis_has_exact_quotas_and_failure_cleanup() {
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
fn text_analysis_keeps_cancellation_and_deadlines_latched() {
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
