mod common;

use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn wide_slice_counts_preserve_checker_schedules_and_block_effects() {
    for width in [4_294_967_296_i64, 4_294_967_297, i64::MAX] {
        for (array, slice_calls) in [("[]", 0), ("[1,2,3]", 1)] {
            for (method, expected) in [("each_slice", slice_calls), ("each_cons", 0)] {
                let source = format!(
                    "def run -> int; seen=0; {array}.{method}({width}){{|part|seen+=1}}; \
                     if seen=={expected}; 7; else; 'wrong'; end; end"
                );
                let script = common::gradual_engine().compile(&source).unwrap();
                let report = script
                    .check_call("run", &[], &CallOptions::default())
                    .unwrap_or_else(|error| panic!("{source}: {error}"));
                assert!(report.is_clean(), "{source}: {report:?}");
                let result = script
                    .call("run", &[], CallOptions::default())
                    .unwrap_or_else(|error| panic!("{source}: {error}"));
                assert_eq!(result.value.as_int(), Some(7), "{source}");
            }
        }
    }
}

#[test]
fn integer_loop_counts_keep_the_language_width_and_obey_quotas() {
    for count in [2_147_483_648_i64, 4_294_967_296, 4_294_967_297, i64::MAX] {
        let script = Engine::new()
            .compile(&format!(
                "seen=[]; {count}.times{{|i|seen.push(i);break if seen.size==2}};seen"
            ))
            .unwrap();
        let result = script.run(CallOptions::default()).unwrap();
        let values: Vec<_> = result
            .value
            .as_array()
            .unwrap()
            .iter()
            .map(Value::as_int)
            .collect();
        assert_eq!(values, [Some(0), Some(1)], "{count}");

        let script = Engine::new()
            .compile(&format!("begin; {count}.times{{|i|i}};rescue;7;end"))
            .unwrap();
        assert_eq!(
            script
                .run(CallOptions {
                    limits: Limits {
                        steps: Some(100),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::Steps,
            "{count}"
        );
    }
}
