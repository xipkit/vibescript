#![no_main]

use libfuzzer_sys::fuzz_target;
use vibescript::{CallOptions, Engine, Limits};

fuzz_target!(|source: &str| {
    if source.len() > 16_384 {
        return;
    }
    let options = CallOptions {
        limits: Limits {
            steps: Some(100_000),
            memory_bytes: Some(16 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let mut engine = Engine::new();
    let _ = engine.compile_with_options(source, &options);
    engine.set_keep_type_checks(true);
    let _ = engine.compile_with_options(source, &options);
});
