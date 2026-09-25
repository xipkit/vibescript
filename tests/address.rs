//! Writes through an element keep the address they started from while the
//! parent grows. An array element may be missing, so a mutating call on it
//! goes through `&.`; a tuple's elements are present, so nested writes
//! through tuple types need none.

mod common;

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

/// The code and offset of each static diagnostic that refuses `source`.
fn refused(source: &str) -> Vec<(String, usize)> {
    let error = common::static_engine()
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| (d.code.to_string(), d.span.start))
        .collect()
}

#[test]
fn negative_receivers_keep_selected_elements_when_their_parents_grow() {
    for (index, selected) in [("-1", 2), ("-2", 1)] {
        let argument = "(while true;a.push([9]);break 3;end)";
        let expected = if selected == 1 {
            serde_json::json!([[1, 3], [[1, 3], [2], [9]]])
        } else {
            serde_json::json!([[2, 3], [[1], [2, 3], [9]]])
        };
        for alias in ["", "unused_snapshot=a;"] {
            let source = format!("a=[[1],[2]];{alias}x=a[{index}]&.push({argument});[x,a]");
            assert_eq!(evaluate(&source), expected, "{source}");
        }
    }
    for (source, expected) in [
        (
            "a=[[1,2]];before=a;r=a[-1]&.fill((while true;a.push([9]);break 7;end));[a,r,before]",
            serde_json::json!([[[7, 7], [9]], [7, 7], [[1, 2]]]),
        ),
        (
            "module M;@@rows: array<array<int>> = [[1]];def self.run -> array<any>;x=@@rows[-1]&.push((while true;@@rows.push([9]);break 2;end));[x,@@rows];end;end;M.run",
            serde_json::json!([[1, 2], [[1, 2], [9]]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
    // A float index, a call on an element that may be missing, dispatch by
    // name, a nested element path, a dotted field and a namespace written
    // like a hash are refused before running.
    for (source, expected) in [
        (
            "a=[[1],[2]];x=a[-1.9]&.push(3);[x,a]",
            vec![("V0101", "1.9")],
        ),
        ("a=[[1],[2]];x=a[-1].push(3);[x,a]", vec![("V0107", "push")]),
        (
            "a=[[1],[2]];x=a[-1].send(:push,3);[x,a]",
            vec![("V0405", "send")],
        ),
        (
            "a=[[[1]]];x=a[-1][-1].push((while true;a[-1].push([8]);a.push([[9]]);break 2;end));[x,a]",
            vec![("V0107", "a[-1][-1]"), ("V0107", "push([8])")],
        ),
        (
            "a=[{items:[1]}];x=a[-1].items.push((while true;a.push({items:[9]});break 2;end));[x,a]",
            vec![("V0415", ".items")],
        ),
    ] {
        let expected: Vec<(String, usize)> = expected
            .into_iter()
            .map(|(code, text)| (code.to_owned(), source.find(text).unwrap()))
            .collect();
        assert_eq!(refused(source), expected, "{source}");
    }
    let source = "JSON[:rows]=[[1]]";
    assert_eq!(
        refused(source),
        [("V0112".to_owned(), 0), ("V0409".to_owned(), 5)]
    );
}

#[test]
fn captured_compound_targets_keep_their_original_positions_and_evaluation_order() {
    for (source, expected) in [
        (
            "a=[1];a[-1]=(while true;a.push(9);break 7;end);a",
            serde_json::json!([1, 7]),
        ),
        (
            "class C;getter seen: array<any>;def initialize;@seen=[];end;def [](key: float) -> int;@seen.push(key);1;end;def []=(key: float,value: int) -> int;@seen.push([key,value]);value;end;end;c=C.new;c[-1.9]+=2;c.seen",
            serde_json::json!([-1.9, [-1.9, 3]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
    // An array element may be missing, so a compound assignment to one is
    // refused, and so are `||=` and `&&=` on a value that is not a bool.
    for (source, expected) in [
        (
            "a=[1];a[-1]+=(while true;a.push(9);break 2;end);a",
            vec![("V0107", "a[-1]")],
        ),
        (
            "a: array<int?> = [nil];a[-1]||=(while true;a.push(9);break 7;end);a",
            vec![("V0104", "a[-1]")],
        ),
        (
            "a=[[1]];a[-1][-1]+=(while true;a[0].push(8);a.push([9]);break 2;end);a",
            vec![("V0107", "a[-1]"), ("V0107", "push(8)")],
        ),
        (
            "a=[1,2];begin;a[-1]+=(while true;a.pop;break 3;end);rescue;a;end",
            vec![("V0107", "a[-1]")],
        ),
    ] {
        let expected: Vec<(String, usize)> = expected
            .into_iter()
            .map(|(code, text)| (code.to_owned(), source.find(text).unwrap()))
            .collect();
        assert_eq!(refused(source), expected, "{source}");
    }
}

#[test]
fn replaced_bindings_and_children_keep_pending_mutations_detached() {
    for (source, expected) in [
        (
            "a=[[1]];x=a[-1]&.push((while true;a=[[8]];break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[8]]]),
        ),
        (
            "a=[[1]];x=a[-1]&.push((while true;a[0]=[8];a.push([9]);break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[8], [9]]]),
        ),
        (
            "a=[[1]];x=a[-1]&.push((while true;a.pop;a.push([9]);break 2;end));[x,a]",
            serde_json::json!([[1, 2], [[9]]]),
        ),
        (
            "a=[[1],[2]];x=a[-2..-1]&.push((while true;a.push([9]);break [3];end));[x,a]",
            serde_json::json!([[[1], [2], [3]], [[1], [2], [9]]]),
        ),
        (
            "a=[[1],[2]];x=a[-2,1]&.push((while true;a.push([9]);break [3];end));[x,a]",
            serde_json::json!([[[1], [3]], [[1], [2], [9]]]),
        ),
        (
            "a: hash<string, array<int>> = {\"-1\": [1]};x=a[\"-1\"]&.push((while true;a[\"extra\"]=[9];break 2;end));[x,a]",
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
            "def run(input: array<array<int>>) -> array<array<int>>;input[-1]&.push((while true;input.push([9]);break 2;end));input;end",
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
    // Each level is a one-element tuple, so the path's elements are present.
    let path = format!("type Path = {}int{}\n", "[".repeat(48), "]".repeat(48));
    let engine = Engine::new();
    let baseline = engine
        .compile(&format!("{path}def run(input: Path) -> Path\ninput\nend"))
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let target = format!("input{}", "[0]".repeat(48));
    let source = format!("{path}def run(input: Path) -> int\n{target}=2\n{target}\nend");
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
    // A compound write to a nested element goes through a tuple type; the
    // others grow the array.
    let rows = "array<array<any>>";
    let pair = "Pair\ntype Pair = [[int, string]]";
    for (ty, body) in [
        (rows, "input.push((while true\nreturn 7\nend))"),
        (rows, "input[0]&.push((while true\nreturn 7\nend))"),
        (pair, "input[0][0]+=(while true\nreturn 7\nend)"),
        (rows, "input[0]&.push(input[0]&.push(2))\n7"),
        (
            rows,
            "input[-1]&.push((while true\ninput.push([9])\nreturn 7\nend))",
        ),
        (
            rows,
            "input[-1]&.fill((while true\ninput.push([9])\nreturn 7\nend))\n7",
        ),
    ] {
        let (ty, alias) = ty.split_once('\n').unwrap_or((ty, ""));
        let source = format!(
            "{alias}\ndef f(input: {ty}) -> any\n{body}\nend\ndef run(input: {ty}) -> int\ni=0\nwhile i<100\nf(input)\ni+=1\nend\n7\nend"
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
    // Growing the array and writing an element of an element cannot be
    // combined, since the element may be missing.
    let source = "def f(input: array<array<any>>) -> any\ninput[-1][-2]+=(while true\ninput.push([9])\nreturn 7\nend)\nend";
    assert_eq!(refused(source), [("V0107".to_owned(), 39)]);
}

#[test]
fn temporary_receivers_run_once_and_do_not_write_through_function_results() {
    let source = "type Grid = [[int]]\n\
                  def get(a: array<array<int>>) -> array<array<int>>\na\nend\n\
                  def grid(b: Grid) -> Grid\nb\nend\n\
                  def run(input: any) -> array<any>\na=[[1]]\nb: Grid = [[1]]\nget(a)[0]&.push(2)\ngrid(b)[0][0]=9\n[a,b]\nend";
    let script = Engine::new().compile(source).unwrap();
    let result = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(b"[[[1]],[[1]]]".as_slice()));
}

#[test]
fn nested_loop_expressions_preserve_the_enclosing_pending_write() {
    let source = "a: array<array<any>> = [[1]]\nx=a[0]&.push((for n in [1,2]\nnext if n==1\nbreak 7\nend))\n[a,x]";
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
