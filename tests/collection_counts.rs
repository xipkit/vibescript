mod common;

use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn integer_loop_counts_keep_the_language_width_and_obey_quotas() {
    for count in [2_147_483_648_i64, 4_294_967_296, 4_294_967_297, i64::MAX] {
        let script = Engine::new()
            .compile(&format!(
                "seen: array<int> = []; {count}.times{{|i|seen.push(i);break if seen.length==2}};seen"
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
