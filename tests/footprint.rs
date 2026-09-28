#[path = "../examples/footprint/allocations.rs"]
mod allocations;
#[path = "../examples/footprint/programs.rs"]
mod programs;

use std::hint::black_box;
use vibescript::{CallOptions, Engine, Value};

#[global_allocator]
static ALLOCATOR: allocations::Counting = allocations::Counting;

#[test]
fn service_footprint_stays_bounded() {
    let before = allocations::snapshot();
    let mut engine = black_box(Engine::new());
    let after = allocations::snapshot();
    assert!(after[0] - before[0] <= 16, "Engine::new allocation count");
    assert!(
        after[1] - before[1] <= 4096,
        "Engine::new allocation volume"
    );
    engine.set_output_writer(|_, _| Ok(()));
    engine.set_error_writer(|_, _| Ok(()));

    // Warm the fixed signature tables before measuring retained script storage.
    let before = allocations::snapshot();
    drop(engine.compile(programs::PROGRAMS[0].1).unwrap());
    assert!(
        allocations::snapshot()[0] - before[0] <= 16_000,
        "first compile allocation count"
    );
    let before = allocations::snapshot();
    let scripts: Vec<_> = programs::PROGRAMS
        .iter()
        .map(|(name, source)| {
            engine
                .compile(source)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
        })
        .collect();
    let retained = allocations::snapshot()[2] - before[2];
    assert!(
        retained <= 100 * 28 * 1024,
        "100 site scripts retain {retained} bytes"
    );
    drop(scripts);

    let script = engine
        .compile("def run(n: int) -> int\n  (\"x\" * n).bytesize\nend\n")
        .unwrap();
    drop(
        script
            .call("run", &[Value::int(1)], CallOptions::default())
            .unwrap(),
    );
    let before = allocations::snapshot();
    drop(
        script
            .call(
                "run",
                &[Value::int(4 * 1024 * 1024)],
                CallOptions::default(),
            )
            .unwrap(),
    );
    for _ in 0..1_000 {
        drop(
            script
                .call("run", &[Value::int(1)], CallOptions::default())
                .unwrap(),
        );
    }
    let after = allocations::snapshot();
    assert!(
        after[2] <= before[2] + 4096,
        "calls retain buffers after a large result is dropped"
    );
}
