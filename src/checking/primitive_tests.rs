use super::{
    collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime,
    native_tests::witness,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Value};

fn forms(receiver: &str, method: &str, arguments: &str) -> [String; 3] {
    let suffix = if arguments.is_empty() {
        String::new()
    } else {
        format!(",{arguments}")
    };
    [
        format!("({receiver}).{method}({arguments})"),
        format!("({receiver}).send(:{method}{suffix})"),
        format!("({receiver}).public_send(:send,:{method}{suffix})"),
    ]
}

#[test]
fn numeric_methods_share_direct_and_forwarded_result_contracts() {
    for (receiver, method, arguments, ty, output) in [
        ("-7", "abs", "", "int", "7"),
        ("-1.5", "abs", "", "float", "1.5"),
        ("7", "even?", "", "bool", "false"),
        ("7", "odd?", "", "bool", "true"),
        ("0", "zero?", "", "bool", "true"),
        ("1.5", "positive?", "", "bool", "true"),
        ("-1", "negative?", "", "bool", "true"),
        ("0", "nonzero?", "", "nil", "nil"),
        ("7", "nonzero?", "", "int", "7"),
        ("7", "succ", "", "int", "8"),
        ("7", "next", "", "int", "8"),
        ("7", "pred", "", "int", "6"),
        ("1.5", "nan?", "", "bool", "false"),
        ("1.5", "finite?", "", "bool", "true"),
        ("1.5", "infinite?", "", "nil", "nil"),
        ("1.5", "round", "", "int", "2"),
        ("1.5", "floor", "", "int", "1"),
        ("1.5", "ceil", "", "int", "2"),
        ("1.25", "round", "1", "float", "1.3"),
        ("145", "round", "-1", "int", "150"),
        ("-7", "div", "3", "int", "-3"),
        ("-7", "divmod", "3", "array<int>", "[-3, 2]"),
        ("7", "fdiv", "2", "float", "3.5"),
        ("-7", "remainder", "3", "int", "-1"),
        ("-7", "modulo", "3", "int", "2"),
        ("7.0", "divmod", "2", "array<number>", "[3, 1]"),
        ("7", "clamp", "1,5", "int", "5"),
        ("7", "clamp", "1.0,5.5", "float", "5.5"),
        ("7", "clamp", "1..5", "int", "5"),
        ("7", "between?", "1,9", "bool", "true"),
        ("7", "between?", "9,{}", "bool", "false"),
        ("7", "to_s", "", "string", "7"),
        ("7", "string", "", "string", "7"),
        ("7", "inspect", "", "string", "7"),
        ("7", "to_i", "", "int", "7"),
        ("7", "to_f", "", "float", "7"),
    ] {
        for call in forms(receiver, method, arguments) {
            witness(
                &format!("def run -> {ty}; {call}; end"),
                Some(output),
                false,
            );
        }
    }
}

#[test]
fn numeric_domains_keep_exception_classes_and_big_integer_paths() {
    for (receiver, method, arguments, class) in [
        ("7", "div", "0", "ZeroDivisionError"),
        ("7", "divmod", "0.0", "ZeroDivisionError"),
        ("7.0", "modulo", "0", "ZeroDivisionError"),
        ("7", "remainder", "0", "ZeroDivisionError"),
        ("7", "round", "1.5", "RuntimeError"),
        ("7", "round", "2147483648", "RuntimeError"),
        ("7", "clamp", "5...7", "RuntimeError"),
        ("7", "clamp", "9,1", "RuntimeError"),
        ("7", "clamp", "false,nil", "RuntimeError"),
        ("7", "nan?", "", "RuntimeError"),
        ("7.0", "even?", "", "RuntimeError"),
        ("7.0", "succ", "", "RuntimeError"),
        ("7", "abs", "1", "RuntimeError"),
        ("7", "div", "false", "RuntimeError"),
        ("7", "between?", "[],{}", "RuntimeError"),
    ] {
        for call in forms(receiver, method, arguments) {
            witness(
                &format!("def run; begin; {call}; rescue {class}; 99; end; end"),
                Some("99"),
                true,
            );
        }
    }
    for source in [
        "def run -> int; (10**30).succ.pred.abs.div(3); end",
        "def run -> array<int>; (10**30).divmod(7); end",
        "def run -> float; (10**30).fdiv(7); end",
        "def run -> int?; 7.fdiv(0).infinite?; end",
        "def run -> bool; 0.fdiv(0).nan?; end",
        "def run; begin; 7.fdiv(0).to_i; rescue RuntimeError; 99; end; end",
        "def run; begin; 0.fdiv(0).round; rescue RuntimeError; 99; end; end",
    ] {
        witness(source, None, false);
    }
}

#[test]
fn text_methods_share_direct_and_forwarded_result_contracts() {
    for (receiver, method, arguments, ty, output) in [
        ("\"é\"", "ord", "", "int", "233"),
        ("\"éabc\"", "chr", "", "string", "é"),
        ("\"abc\"", "start_with?", "\"a\",7", "bool", "true"),
        ("\"abc\"", "end_with?", "\"c\",7", "bool", "true"),
        ("\"a,b,\"", "split", "\",\",-1", "array<string>", "[a, b, ]"),
        ("\" a b \"", "split", "nil", "array<string>", "[a, b]"),
        ("\"éabc\"", "index", "\"a\",1.5", "int", "1"),
        ("\"ababa\"", "rindex", "\"a\",-2", "int", "2"),
        ("\"fF\"", "hex", "", "int", "255"),
        ("\"17\"", "oct", "", "int", "15"),
        ("\"c\"", "clamp", "\"a\",\"b\"", "string", "b"),
        ("\"b\"", "between?", "\"a\",\"c\"", "bool", "true"),
        ("\"abc\"", "to_sym", "", "symbol", "abc"),
        ("\"abc\"", "intern", "", "symbol", "abc"),
        ("\"abc\"", "to_s", "", "string", "abc"),
        ("\"abc\"", "string", "", "string", "abc"),
        ("\"7\"", "to_i", "", "int", "7"),
        ("\"1.5\"", "to_f", "", "float", "1.5"),
        ("\"AbC\"", "casecmp", "\"abc\"", "int", "0"),
        ("\"Σ\"", "casecmp?", "\"ς\"", "bool", "true"),
        ("\"abc\"", "casecmp", "false", "nil", "nil"),
        ("\"abc\"", "upcase", "", "string", "ABC"),
        ("\"Straße\"", "downcase", ":fold", "string", "strasse"),
        ("\"aBC\"", "capitalize", "", "string", "Abc"),
        ("\"aBC\"", "swapcase", ":ascii", "string", "Abc"),
        ("\"x\"", "center", "4.9,\".\"", "string", ".x.."),
        ("\"x\"", "ljust", "3", "string", "x  "),
        ("\"x\"", "rjust", "3", "string", "  x"),
        (
            "\"a=b=c\"",
            "partition",
            "\"=\"",
            "array<string>",
            "[a, =, b=c]",
        ),
        (
            "\"a=b=c\"",
            "rpartition",
            "\"=\"",
            "array<string>",
            "[a=b, =, c]",
        ),
        ("\" x \"", "strip", "", "string", "x"),
        ("\" x \"", "lstrip", "", "string", "x "),
        ("\" x \"", "rstrip", "", "string", " x"),
        ("\" a  b \"", "squish", "", "string", "a b"),
        ("\"abc\"", "chomp", "\"c\"", "string", "ab"),
        ("\"abc\"", "chop", "", "string", "ab"),
        ("\"abc\"", "delete_prefix", "\"a\"", "string", "bc"),
        ("\"abc\"", "delete_suffix", "\"c\"", "string", "ab"),
        ("\"ababa\"", "count", "\"a\"", "int", "3"),
        ("\"ababa\"", "delete", "\"a\"", "string", "bb"),
        ("\"abc\"", "tr", "\"a-c\",\"A-C\"", "string", "ABC"),
        ("\"aabb\"", "squeeze", "", "string", "ab"),
    ] {
        for call in forms(receiver, method, arguments) {
            witness(
                &format!("def run -> {ty}; {call}; end"),
                Some(output),
                false,
            );
        }
    }
}

#[test]
fn text_argument_domains_are_checked_without_entering_blocks() {
    for (method, arguments) in [
        ("start_with?", "7"),
        ("start_with?", "\"z\",7"),
        ("end_with?", "false"),
        ("ord", "7"),
        ("index", "7"),
        ("rindex", "false"),
        ("index", "\"x\",false"),
        ("split", "7"),
        ("split", "nil,1.5"),
        ("center", "false"),
        ("ljust", "7,\"\""),
        ("rjust", "7,false"),
        ("upcase", ":fold"),
        ("downcase", "\"ascii\""),
        ("capitalize", ":missing"),
        ("tr", "\"z-a\",\"x\""),
        ("count", ""),
        ("delete", "7"),
        ("chomp", "7"),
        ("partition", "false"),
        ("clamp", "\"z\",\"a\""),
    ] {
        for call in forms("\"abc\"", method, arguments) {
            witness(
                &format!(
                    "def run; seen=[]; begin; {call}; rescue RuntimeError; [99,seen]; end; end"
                ),
                Some("[99, []]"),
                true,
            );
        }
    }
    for call in [
        "\"\".ord",
        "\"bad\".to_i",
        "\"bad\".to_f",
        "\"ffffffffffffffff\".hex",
    ] {
        witness(
            &format!("def run; begin; {call}; rescue RuntimeError; 99; end; end"),
            Some("99"),
            true,
        );
    }
}

#[test]
fn validation_short_circuits_before_unused_arguments() {
    for (method, arguments, output) in [
        ("start_with?", "\"\",{}", "true"),
        ("end_with?", "\"\",[]", "true"),
        ("rindex", "{},-20", "nil"),
        ("rindex", "false,-20", "nil"),
        ("casecmp?", "{}", "nil"),
    ] {
        for call in forms("\"abc\"", method, arguments) {
            witness(&format!("def run; {call}; end"), Some(output), false);
        }
    }
    inferred_runtime(
        "def run(s:string); s.start_with?(\"\",false); end",
        &[Value::bytes("abc")],
        false,
    );
    inferred_runtime(
        "def run(s:string); begin; s.rindex(false,-20); rescue RuntimeError; 99; end; end",
        &[Value::bytes("abc")],
        true,
    );
}

#[test]
fn primitive_bang_results_leave_the_original_string_unchanged() {
    for (method, argument) in [
        ("upcase!", ""),
        ("downcase!", ""),
        ("capitalize!", ""),
        ("swapcase!", ""),
        ("strip!", ""),
        ("lstrip!", ""),
        ("rstrip!", ""),
        ("squish!", ""),
        ("chop!", ""),
        ("chomp!", "\"c\""),
        ("delete_prefix!", "\"a\""),
        ("delete_suffix!", "\"c\""),
        ("reverse!", ""),
        ("delete!", "\"a\""),
        ("tr!", "\"a\",\"A\""),
        ("squeeze!", ""),
    ] {
        for call in forms("s", method, argument) {
            witness(
                &format!("def run; s=\"aabc\"; r={call}; [s,r]; end"),
                None,
                false,
            );
        }
    }
    witness("def run -> nil; \"ABC\".upcase!; end", Some("nil"), false);
    witness(
        "def run -> string; \"abc\".upcase!; end",
        Some("ABC"),
        false,
    );
    witness(
        "def run; s=\"abc\"; [s.delete(\"a\"),s]; end",
        Some("[bc, abc]"),
        false,
    );
}

#[test]
fn ignored_primitive_blocks_stay_inert_for_every_dispatch_form() {
    for (receiver, method, arguments) in [
        ("7", "abs", ""),
        ("7", "round", "0"),
        ("7", "div", "2"),
        ("\"abc\"", "upcase", ""),
        ("\"abc\"", "split", "\"b\""),
        ("\"abc\"", "index", "\"b\""),
        ("\"abc\"", "strip", ""),
        ("\"abc\"", "start_with?", "\"a\""),
        ("\"abc\"", "hex", ""),
        ("\"abc\"", "center", "5"),
        ("\"abc\"", "partition", "\"b\""),
        ("1.seconds", "eql?", "1.seconds"),
        ("Time.at(0)", "eql?", "Time.at(0)"),
        ("nil", "to_s", ""),
        ("true", "string", ""),
        (":abc", "to_s", ""),
    ] {
        for call in forms(receiver, method, arguments) {
            witness(
                &format!("def run; seen=[]; {call} {{seen.push(9); return 999}}; seen; end"),
                Some("[]"),
                false,
            );
        }
    }
}

#[test]
fn primitive_flags_reject_before_block_entry_and_keep_keyword_rules() {
    for (receiver, method, arguments) in [
        ("7", "to_s", ""),
        ("7", "to_i", ""),
        ("7", "clamp", "1,9"),
        ("\"abc\"", "to_sym", ""),
        ("\"abc\"", "inspect", ""),
        ("\"abc\"", "to_i", ""),
        ("\"abc\"", "between?", "\"a\",\"z\""),
        ("\"abc\"", "count", "\"a\""),
        ("\"abc\"", "delete", "\"a\""),
        ("\"abc\"", "tr", "\"a\",\"b\""),
        ("\"abc\"", "squeeze", ""),
        ("1.seconds", "equal?", "1.seconds"),
        ("[]", "freeze", ""),
        ("7", "eql?", "7"),
    ] {
        for call in forms(receiver, method, arguments) {
            witness(
                &format!(
                    "def run; seen=[]; begin; {call} {{seen.push(9); return 999}}; rescue RuntimeError; [99,seen]; end; end"
                ),
                Some("[99, []]"),
                true,
            );
        }
    }
    for (receiver, method, arguments, rejected) in [
        ("7", "round", "0", false),
        ("\"abc\"", "upcase", "", false),
        ("\"abc\"", "split", "", false),
        ("\"abc\"", "index", "\"a\"", false),
        ("\"abc\"", "center", "5", true),
        ("\"abc\"", "partition", "\"b\"", true),
        ("1.seconds", "eql?", "1.seconds", true),
    ] {
        let arguments = if arguments.is_empty() {
            "extra:7".to_owned()
        } else {
            format!("{arguments},extra:7")
        };
        for call in forms(receiver, method, &arguments) {
            witness(
                &format!(
                    "def run; seen=[]; begin; {call} {{seen.push(9); return 999}}; rescue RuntimeError; seen.push(99); end; seen; end"
                ),
                Some(if rejected { "[99]" } else { "[]" }),
                rejected,
            );
        }
    }
}

#[test]
fn lifecycle_and_equality_preserve_value_and_namespace_semantics() {
    for receiver in [
        "nil",
        "false",
        "7",
        "1.5",
        "\"abc\"",
        ":abc",
        "[1,2]",
        "{a:1}",
        "1..3",
        "/a/",
        "1.seconds",
        "Time.at(0)",
        "JSON",
    ] {
        for method in ["clone", "freeze", "frozen?", "nil?", "itself", "dup"] {
            for call in forms(receiver, method, "") {
                witness(&format!("def run; {call}; end"), None, false);
            }
        }
    }
    for (left, right, eql, equal) in [
        ("1", "1.0", "false", "false"),
        ("[1]", "[1.0]", "false", "true"),
        ("{a:1}", "{a:1.0}", "false", "true"),
        ("\"abc\"", ":abc", "false", "false"),
    ] {
        for (method, output) in [("eql?", eql), ("equal?", equal)] {
            for call in forms(left, method, right) {
                witness(
                    &format!("def run -> bool; {call}; end"),
                    Some(output),
                    false,
                );
            }
        }
    }
    witness(
        "def run; a=[1]; b=a.clone; b.push(2); [a,b]; end",
        Some("[[1], [1, 2]]"),
        false,
    );
    witness("def run; h={clone:JSON::parse}; h.clone; end", None, false);
}

#[test]
fn general_primitive_inputs_preserve_result_types_and_union_paths() {
    for (source, arguments) in [
        (
            "def run(n:int) -> int; n.abs.succ.round(-1).div(3); end",
            vec![Value::int(-17)],
        ),
        (
            "def run(n:float) -> int; n.round; end",
            vec![Value::float(1.5)],
        ),
        (
            "def run(n:float) -> float; n.round(1); end",
            vec![Value::float(1.25)],
        ),
        (
            "def run(n:number) -> array<number>; n.divmod(3); end",
            vec![Value::float(7.0)],
        ),
        (
            "def run(s:string) -> string; s.upcase(:ascii).center(8.5,\".\").strip; end",
            vec![Value::bytes("abc")],
        ),
        (
            "def run(s:string) -> array<string>; s.split(nil,2); end",
            vec![Value::bytes("a b c")],
        ),
        (
            "def run(s:string) -> string?; s.squeeze!; end",
            vec![Value::bytes("abc")],
        ),
        (
            "def run(s:string) -> symbol; s.intern; end",
            vec![Value::bytes("abc")],
        ),
        (
            "def run(s:string) -> nil; s.casecmp(false); end",
            vec![Value::bytes("abc")],
        ),
        (
            "def run(s:string) -> string; s.delete(\"a\"); end",
            vec![Value::bytes("abc")],
        ),
        (
            "def run(s:string) -> array<string>; s.partition(\"b\"); end",
            vec![Value::bytes("abc")],
        ),
    ] {
        inferred_runtime(source, &arguments, false);
    }
    for flag in [false, true] {
        inferred_runtime(
            "def run(flag:bool) -> number; n=if flag; 7; else; 7.5; end; d=if flag; 0; else; 2; end; begin; n.div(d); rescue ZeroDivisionError; 99; end; end",
            &[Value::boolean(flag)],
            false,
        );
        inferred_runtime(
            "def run(flag:bool) -> number; n=if flag; 1; else; -1; end; 1.25.round(n); end",
            &[Value::boolean(flag)],
            false,
        );
    }
}

#[test]
fn native_reducers_use_primitive_contracts_without_callbacks() {
    for (body, output) in [
        ("[64,2,4].reduce(:div)", "8"),
        ("[7,0].reduce(:fdiv).infinite?", "1"),
    ] {
        witness(&format!("def run; {body}; end"), Some(output), false);
    }
    witness(
        "def run; begin; [7,0].reduce(:div); rescue ZeroDivisionError; 99; end; end",
        Some("99"),
        true,
    );
    witness(
        "def run; begin; [\"b\",\"a\",\"c\"].reduce(:casecmp); rescue RuntimeError; 99; end; end",
        Some("99"),
        true,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(
        ctx,
        &mut facts,
        "def run; padding=\"ΐ\".upcase.center(513); parts=\"a,b,c\".split(\",\"); n=0; s=\"a\"; while n<5; n=n.succ; s=s.center(n,\".\"); end; [s.split(\".\"),n.divmod(2),s.upcase,s.eql?(\"a\"),padding,parts]; end",
    )?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn computed_primitives_converge_and_charge_work_and_memory() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, expected) in [
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
        assert_eq!(result.err().map(|e| e.kind), expected);
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..64 {
        for memory in [false, true] {
            let mut ctx = CallContext::new(CallOptions {
                limits: if memory {
                    Limits {
                        memory_bytes: Some(stats.peak_memory_bytes * sample / 64),
                        ..Limits::default()
                    }
                } else {
                    Limits {
                        steps: Some(stats.steps * sample as u64 / 64),
                        ..Limits::default()
                    }
                },
                ..CallOptions::default()
            });
            let kind = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            };
            assert_eq!(accounting(&mut ctx).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn primitive_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn primitive_contracts_contain_runtime_results_for_literal_and_general_inputs() {
    use super::{
        arguments::Arguments, builtins, collection_tests::literal_fact, facts::Atom,
        relation::Relation,
    };
    use crate::{
        arguments,
        bytecode::{CallSite, Method},
        value::Kind,
    };

    let numbers = [
        Value::int(-7),
        Value::int(0),
        Value::int(i64::MIN),
        Value::int(i64::MAX),
        Value::float(1.5),
        Value::float(0.0),
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
    ];
    let strings = [
        Value::bytes("abc"),
        Value::bytes(""),
        Value::bytes(" a\r\n"),
        Value::bytes(b"\xffa\x80b".as_slice()),
    ];
    let numeric_methods = [
        "abs",
        "even?",
        "odd?",
        "zero?",
        "positive?",
        "negative?",
        "nonzero?",
        "next",
        "succ",
        "pred",
        "nan?",
        "finite?",
        "infinite?",
        "round",
        "floor",
        "ceil",
        "div",
        "divmod",
        "fdiv",
        "remainder",
        "modulo",
        "clamp",
        "between?",
        "to_i",
        "to_f",
        "to_s",
        "string",
        "inspect",
    ];
    let text_methods = [
        "ord",
        "chr",
        "start_with?",
        "end_with?",
        "split",
        "index",
        "rindex",
        "hex",
        "oct",
        "clamp",
        "between?",
        "to_sym",
        "intern",
        "to_s",
        "string",
        "to_i",
        "to_f",
        "inspect",
        "casecmp",
        "casecmp?",
        "upcase",
        "upcase!",
        "downcase",
        "downcase!",
        "capitalize",
        "swapcase",
        "center",
        "ljust",
        "rjust",
        "partition",
        "rpartition",
        "strip",
        "strip!",
        "lstrip",
        "rstrip",
        "squish",
        "chomp",
        "chomp!",
        "chop",
        "delete_prefix",
        "delete_suffix",
        "reverse!",
        "count",
        "delete",
        "delete!",
        "tr",
        "tr!",
        "squeeze",
        "squeeze!",
    ];
    let arguments = [
        vec![],
        vec![Value::nil()],
        vec![Value::boolean(false)],
        vec![Value::int(0)],
        vec![Value::int(-20)],
        vec![Value::int(2)],
        vec![Value::float(1.5)],
        vec![Value::float(f64::NAN)],
        vec![Value::float(f64::INFINITY)],
        vec![Value::bytes("a")],
        vec![Value::bytes("")],
        vec![Value::bytes("z-a")],
        vec![Value::symbol("ascii")],
        vec![Value::symbol("fold")],
        vec![Value::int(2), Value::int(7)],
        vec![Value::int(7), Value::int(2)],
        vec![Value::nil(), Value::nil()],
        vec![Value::bytes("a"), Value::bytes("z")],
        vec![Value::bytes("z"), Value::bytes("a")],
        vec![Value::bytes(""), Value::boolean(false)],
        vec![Value::bytes("a"), Value::float(-20.5)],
    ];
    let mut cases = 0;
    for (receivers, names) in [
        (&numbers[..], &numeric_methods[..]),
        (&strings[..], &text_methods[..]),
    ] {
        for receiver in receivers {
            for &name in names {
                for values in &arguments {
                    for keywords in [false, true] {
                        for general in 0..4 {
                            let label = format!(
                                "{receiver:?}.{name}({values:?}), keywords={keywords}, general={general}"
                            );
                            let mut runtime = CallContext::new(CallOptions::default());
                            let actual_receiver = runtime.import(receiver).unwrap();
                            let mut actual_args = arguments::Arguments::empty();
                            for value in values {
                                let value = runtime.import(value).unwrap();
                                actual_args.positional.push(&mut runtime, value).unwrap();
                            }
                            let mut ctx = CallContext::new(CallOptions::default());
                            let mut facts = Facts::new(&mut ctx).unwrap();
                            let root = if general & 1 != 0 {
                                match receiver.0 {
                                    Kind::Int(_) => Atom::Int.fact(),
                                    Kind::Float(_) => Atom::Float.fact(),
                                    Kind::Bytes(_) => Atom::String.fact(),
                                    _ => unreachable!(),
                                }
                            } else if let Kind::Float(number) = receiver.0 {
                                facts.float(&mut ctx, number).unwrap()
                            } else {
                                literal_fact(&mut ctx, &mut facts, receiver)
                            };
                            let mut inputs = Arguments::new();
                            for value in values {
                                let fact = if let Kind::Float(number) = value.0 {
                                    facts.float(&mut ctx, number).unwrap()
                                } else {
                                    literal_fact(&mut ctx, &mut facts, value)
                                };
                                let fact = if general & 2 != 0 {
                                    facts.atom(fact).unwrap().fact()
                                } else {
                                    fact
                                };
                                inputs.positional.push(&mut ctx, fact).unwrap();
                            }
                            if keywords {
                                let name = facts.symbol(&mut ctx, b"extra").unwrap();
                                let value = facts.boolean(&mut ctx, true).unwrap();
                                inputs.keyword(&mut ctx, name, value).unwrap();
                                let key = runtime.bytes(b"extra").unwrap();
                                actual_args
                                    .keywords
                                    .insert(&mut runtime, key, Value::boolean(true))
                                    .unwrap();
                            }
                            let site = CallSite {
                                name: 0,
                                method: Method::parse(name),
                                auto: false,
                                parenthesized: true,
                                scope: false,
                            };
                            let inferred =
                                builtins::member(&mut ctx, &mut facts, root, site, name, &inputs)
                                    .unwrap_or_else(|error| panic!("{label}: {error}"))
                                    .unwrap();
                            assert!(!inferred.incomplete, "{label}");
                            match crate::members::call_keywords(
                                &mut runtime,
                                site,
                                name,
                                actual_receiver,
                                &actual_args,
                            ) {
                                Ok((_, value)) => {
                                    assert_ne!(
                                        inferred.value,
                                        Atom::Never.fact(),
                                        "{label}: no normal result"
                                    );
                                    let concrete = literal_fact(&mut ctx, &mut facts, &value);
                                    assert_ne!(
                                        facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                                        Relation::Rejected,
                                        "{label}: {:?}",
                                        facts.node(inferred.value)
                                    );
                                }
                                Err(error) => assert_ne!(
                                    inferred.throws & (1 << error.class().unwrap() as u8),
                                    0,
                                    "{label}: {error}, throws={}",
                                    inferred.throws
                                ),
                            }
                            drop((inputs, inferred, facts, actual_args));
                            assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                            assert_eq!(runtime.stats().retained_memory_bytes, 0, "{label}");
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 70_560);
}

#[test]
fn primitive_union_domains_and_pending_string_reads_keep_their_branches() {
    for flag in [false, true] {
        for source in [
            "def run(flag:bool) -> string; mode=if flag; :ascii; else; :fold; end; begin; \"abc\".upcase(mode); rescue RuntimeError; \"caught\"; end; end",
            "def run(flag:bool) -> int; digits=if flag; 0; else; 2147483648; end; begin; 7.round(digits); rescue RuntimeError; 99; end; end",
        ] {
            inferred_runtime(source, &[Value::boolean(flag)], true);
        }
    }
    for call in forms("s", "delete", "begin; s=\"xy\"; \"a\"; end") {
        witness(
            &format!("def run; s=\"abc\"; r={call}; [s,r]; end"),
            None,
            false,
        );
    }
    for value in ["[]", "{}", "[1,2]", "{a:1}"] {
        witness(
            &format!("def run -> int; if ({value}).nil?; missing; else; 7; end; end"),
            Some("7"),
            false,
        );
    }
    inferred_runtime(
        "def run(n:int) -> number; 2**n; end",
        &[Value::int(-3)],
        false,
    );
}
