use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn fused_loop_work_preserves_step_boundaries_and_numeric_semantics() {
    // Counts come from the unfused VM, including entry and return work.
    for (source, steps, expected) in [
        (
            "def run -> int\ni=0\nwhile i<4\ni+=1\nnext if i==2\nbreak if i>=3\nend\ni\nend",
            84,
            Value::int(3),
        ),
        (
            "def run -> int\nn=0\n[1,2,3].each_with_index { |v,i| n+=v+i }\nn\nend",
            106,
            Value::int(9),
        ),
        (
            "def run -> bool\nx=9223372036854775807\nx+=1\nx>9223372036854775807 && x-1==9223372036854775807\nend",
            35,
            Value::boolean(true),
        ),
        (
            "def run -> bool\nx=0.0/0.0\nx<1.0 || x==x || x != x\nend",
            30,
            Value::boolean(true),
        ),
        (
            "def run -> int\n[1,2].each { |x|\n[3,4].map { |y| break y if y==4; x*y }\n}\n7\nend",
            152,
            Value::int(7),
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        for limit in 0..=steps {
            let result = script.call(
                "run",
                &[],
                CallOptions {
                    limits: Limits {
                        steps: Some(limit),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            );
            if limit < steps {
                let error = result.unwrap_err();
                assert_eq!(error.kind, ErrorKind::Steps, "{source}: {limit}");
                assert_eq!(error.message, format!("step quota exceeded ({limit})"));
            } else {
                let result = result.unwrap();
                assert_eq!(result.stats.steps, steps, "{source}");
                assert_eq!(result.value.as_int(), expected.as_int(), "{source}");
                assert_eq!(result.value.type_name(), expected.type_name(), "{source}");
                assert_eq!(result.value.truthy(), expected.truthy(), "{source}");
            }
        }
    }
}

#[test]
fn array_loop_results_and_saved_snapshots_remain_values() {
    for (source, expected) in [
        (
            "out: array<int> = []\nfor i in 1..4\nout << i\nend",
            "[1,2,3,4]",
        ),
        (
            "out: array<int> = []\nsaved: array<array<int>> = []\nfor i in 1..4\nsaved << out\nout << i\nend\n[saved,out]",
            "[[[],[1],[1,2],[1,2,3]],[1,2,3,4]]",
        ),
        (
            "out: array<int> = []\nfor i in 0...10\nnext if i==2\nbreak if i==5\nout << i\nend\nout",
            "[0,1,3,4]",
        ),
        (
            "out: array<int> = []\nresult=for i in [1,2,3]\nout << i\nend\n[result,out]",
            "[[1,2,3],[1,2,3]]",
        ),
        (
            "out: array<int> = []\nfor i in 1..4\nfor j in 1..2\nout << i*j\nend\nend\nout",
            "[1,2,2,4,3,6,4,8]",
        ),
        (
            "out: array<int> = []\nbegin\nfor i in 1..4\nraise \"stop\" if i==3\nout << i\nend\nrescue\nout << 9\nend\nout",
            "[1,2,9]",
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let result = script.run(CallOptions::default()).unwrap();
        let json = vibescript::stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(
            json.value.as_bytes().unwrap(),
            expected.as_bytes(),
            "{source}"
        );
    }
}
