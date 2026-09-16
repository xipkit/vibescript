use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn negative_receivers_keep_selected_elements_when_their_parents_grow() {
    for (index, selected) in [("-1", 2), ("-1.9", 2), ("-2", 1), ("-2.8", 1)] {
        let receiver = format!("a[{index}]");
        let argument = "(while true;a.push([9]);break 3;end)";
        for call in [
            format!("{receiver}.push({argument})"),
            format!("{receiver}&.push({argument})"),
            format!("({receiver}.push)({argument})"),
            format!("{receiver}.send(:push,{argument})"),
            format!("{receiver}.public_send(:push,{argument})"),
            format!("{receiver}.send(:public_send,:push,{argument})"),
        ] {
            let expected = if selected == 1 {
                serde_json::json!([[1, 3], [[1, 3], [2], [9]]])
            } else {
                serde_json::json!([[2, 3], [[1], [2, 3], [9]]])
            };
            for alias in ["", "unused_snapshot=a;"] {
                let source = format!("a=[[1],[2]];{alias}x={call};[x,a]");
                assert_eq!(evaluate(&source), expected, "{source}");
            }
        }
    }
    for (source, expected) in [
        (
            "a=[[1,2]];before=a;r=a[-1].fill{a.push([9]);7};[a,r,before]",
            serde_json::json!([[[7, 7], [9], [9]], [7, 7], [[1, 2]]]),
        ),
        (
            "a=[[[1]]];x=a[-1][-1].push((while true;a[-1].push([8]);a.push([[9]]);break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[[1, 2], [8]], [[9]]]]),
        ),
        (
            "a=[{items:[1]}];x=a[-1].items.push((while true;a.push({items:[9]});break 2;end));[x,a]",
            serde_json::json!([[1, 2], [{"items":[1,2]}, {"items":[9]}]]),
        ),
        (
            "module M;@@rows=[[1]];def self.run;x=@@rows[-1].push((while true;@@rows.push([9]);break 2;end));[x,@@rows];end;end;M.run",
            serde_json::json!([[1, 2], [[1, 2], [9]]]),
        ),
        (
            "JSON[:rows]=[[1]];x=JSON[:rows][-1].push((while true;JSON[:rows].push([9]);break 2;end));[x,JSON[:rows]]",
            serde_json::json!([[1, 2], [[1, 2], [9]]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn captured_compound_targets_keep_their_original_positions_and_evaluation_order() {
    for (source, expected) in [
        (
            "a=[1];a[-1]+=(while true;a.push(9);break 2;end);a",
            serde_json::json!([3, 9]),
        ),
        (
            "a=[nil];a[-1]||=(while true;a.push(9);break 7;end);a",
            serde_json::json!([7, 9]),
        ),
        (
            "a=[true];a[-1]&&=(while true;a.push(9);break 7;end);a",
            serde_json::json!([7, 9]),
        ),
        (
            "a=[1];n=0;a[(while true;n+=1;break -1.9;end)]+=(while true;a.push(9);break 2;end);[a,n]",
            serde_json::json!([[3, 9], 1]),
        ),
        (
            "a=[[1]];a[-1][-1]+=(while true;a[0].push(8);a.push([9]);break 2;end);a",
            serde_json::json!([[3, 8], [9]]),
        ),
        (
            "a=[1];a[-1]=(while true;a.push(9);break 7;end);a",
            serde_json::json!([1, 7]),
        ),
        (
            "a=[];a[-1]||=(while true;a.push(9);break 7;end);a",
            serde_json::json!([7]),
        ),
        (
            "a=[1,2];begin;a[-1]+=(while true;a.pop;break 3;end);rescue;a;end",
            serde_json::json!([1]),
        ),
        (
            "class C;getter seen;def initialize;@seen=[];end;def [](key);@seen.push(key);1;end;def []=(key,value);@seen.push([key,value]);value;end;end;c=C.new;c[-1.9]+=2;c.seen",
            serde_json::json!([-1.9, [-1.9, 3]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
    for index in ["1", "-1"] {
        let source = format!("a=[1,2];a[{index}]+=(while true;a.pop;break 3;end)");
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}");
        assert_eq!(error.message, "array index out of bounds", "{source}");
    }
}

#[test]
fn replaced_bindings_and_children_keep_pending_mutations_detached() {
    for (source, expected) in [
        (
            "a=[[1]];x=a[-1].push((while true;a=[[8]];break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[8]]]),
        ),
        (
            "a=[[1]];x=a[-1].push((while true;a[0]=[8];a.push([9]);break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[8], [9]]]),
        ),
        (
            "a=[[1]];x=a[-1].push((while true;a.pop;a.push([9]);break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[9]]]),
        ),
        (
            "a=[[1],[2]];x=a[-2..-1].push((while true;a.push([9]);break [3];end));[x,a]",
            serde_json::json!([[[1], [2], [3]], [[1], [2], [9]]]),
        ),
        (
            "a=[[1],[2]];x=a[-2,1].push((while true;a.push([9]);break [3];end));[x,a]",
            serde_json::json!([[[1], [3]], [[1], [2], [9]]]),
        ),
        (
            "a={\"-1\":[1]};x=a[\"-1\"].push((while true;a.extra=[9];break 2;end));[x,a]",
            serde_json::json!([[1,2],{"-1":[1,2],"extra":[9]}]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn captured_negative_paths_preserve_host_inputs_and_exact_execution_limits() {
    let input = Value::array(vec![Value::array(vec![Value::int(1)])]);
    let script = Engine::new()
        .compile(
            "def run(input);input[-1].push((while true;input.push([9]);break 2;end));input;end",
        )
        .unwrap();
    let run = |options| script.call("run", std::slice::from_ref(&input), options);
    let baseline = run(CallOptions::default()).unwrap();
    assert_eq!(
        baseline.value.as_array().unwrap()[0].as_array().unwrap()[1].as_int(),
        Some(2)
    );
    for _ in 0..2 {
        for (steps, memory, error) in [
            (Some(baseline.stats.steps), None, None),
            (Some(baseline.stats.steps - 1), None, Some(ErrorKind::Steps)),
            (None, Some(baseline.stats.peak_memory_bytes), None),
            (
                None,
                Some(baseline.stats.peak_memory_bytes - 1),
                Some(ErrorKind::Memory),
            ),
        ] {
            let mut options = CallOptions::default();
            if let Some(steps) = steps {
                options.limits.steps = Some(steps);
            }
            if let Some(memory) = memory {
                options.limits.memory_bytes = Some(memory);
            }
            let result = run(options);
            if let Some(error) = error {
                assert_eq!(result.unwrap_err().kind, error);
            } else {
                let result = result.unwrap();
                let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
                assert_eq!(encoded.value.as_bytes(), Some(b"[[1,2],[9]]".as_slice()));
                assert_eq!(result.stats.steps, baseline.stats.steps);
                assert_eq!(
                    result.stats.peak_memory_bytes,
                    baseline.stats.peak_memory_bytes
                );
            }
            let encoded = stringify_json(&input, CallOptions::default()).unwrap();
            assert_eq!(encoded.value.as_bytes(), Some(b"[[1]]".as_slice()));
        }
    }
}

#[test]
fn path_storage_and_work_are_accounted_before_writing() {
    let mut input = Value::int(1);
    for _ in 0..48 {
        input = Value::array(vec![input]);
    }
    let engine = Engine::new();
    let baseline = engine
        .compile("def run(input)\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let target = format!("input{}", "[0]".repeat(48));
    let source = format!("def run(input)\n{target}=2\n{target}\nend");
    let script = engine.compile(&source).unwrap();
    for (limits, expected) in [
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes + 512),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            Limits {
                steps: Some(baseline.stats.steps + 30),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
    ] {
        assert_eq!(
            script
                .call(
                    "run",
                    std::slice::from_ref(&input),
                    CallOptions {
                        limits,
                        ..CallOptions::default()
                    }
                )
                .unwrap_err()
                .kind,
            expected
        );
    }
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(2));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let mut leaf = &input;
    for _ in 0..48 {
        leaf = &leaf.as_array().unwrap()[0];
    }
    assert_eq!(leaf.as_int(), Some(1));
}

#[test]
fn pending_writes_are_reclaimed_on_return_and_call_completion() {
    for body in [
        "input.push((while true\nreturn 7\nend))",
        "input[0].push((while true\nreturn 7\nend))",
        "input[0][0]+=(while true\nreturn 7\nend)",
        "input[0].push(input[0].push(2))\n7",
        "input[-1].push((while true\ninput.push([9])\nreturn 7\nend))",
        "input[-1][-2]+=(while true\ninput.push([9])\nreturn 7\nend)",
        "input[-1].fill {input.push([9]);return 7}\n7",
    ] {
        let source = format!(
            "def f(input)\n{body}\nend\ndef run(input)\ni=0\nwhile i<100\nf(input)\ni+=1\nend\n7\nend"
        );
        let input = Value::array(vec![Value::array(vec![
            Value::int(1),
            Value::bytes(vec![b'a'; 16384]),
        ])]);
        let script = Engine::new().compile(&source).unwrap();
        let result = script
            .call(
                "run",
                &[input],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(40_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.value.as_int(), Some(7), "{body}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
    }
}

#[test]
fn temporary_receivers_run_once_and_do_not_write_through_function_results() {
    let source =
        "def get(a)\na\nend\ndef run(input)\na=[[1]]\nget(a)[0].push(2)\nget(a)[0][0]=9\na\nend";
    let script = Engine::new().compile(source).unwrap();
    let result = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(b"[[1]]".as_slice()));
}

#[test]
fn nested_loop_expressions_preserve_the_enclosing_pending_write() {
    let source = "a=[[1]]\nx=a[0].push((for n in [1,2]\nnext if n==1\nbreak 7\nend))\n[a,x]";
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    assert_eq!(
        encoded.value.as_bytes(),
        Some(b"[[[1,7]],[1,7]]".as_slice())
    );
}
