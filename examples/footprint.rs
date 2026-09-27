//! Startup and retained-script footprint, with allocator accounting enabled by
//! `allocation-stats`. Run through `scripts/footprint.py` for fresh-process rounds.

#[cfg(feature = "allocation-stats")]
#[path = "footprint/allocations.rs"]
mod allocations;
#[path = "footprint/programs.rs"]
mod programs;
#[path = "footprint/rss.rs"]
mod rss;
#[path = "footprint/snapshot.rs"]
mod snapshot;

use std::{hint::black_box, time::Instant};
use vibescript::{CallOptions, Capability, Engine, HostMethod, Signature, SignatureParam, Value};

#[cfg(feature = "allocation-stats")]
#[global_allocator]
static ALLOCATOR: allocations::Counting = allocations::Counting;

struct Stage {
    name: &'static str,
    allocations: [usize; 4],
    rss: Option<usize>,
    elapsed_ns: u128,
}

impl Stage {
    fn capture(name: &'static str, elapsed_ns: u128) -> Self {
        Self {
            name,
            #[cfg(feature = "allocation-stats")]
            allocations: allocations::snapshot(),
            #[cfg(not(feature = "allocation-stats"))]
            allocations: [0; 4],
            rss: rss::current(),
            elapsed_ns,
        }
    }
}

fn registered(engine: &mut Engine) -> vibescript::Result<Vec<Capability>> {
    let mut capabilities = Vec::new();
    for name in ["log_event", "metric", "lookup", "publish"] {
        let method = HostMethod::new(name, |ctx, args, _| {
            ctx.charge(1)?;
            Ok(args[0].clone())
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "value".into(),
                ty: "string".into(),
                optional: false,
            }],
            result: "string".into(),
            accepts_block: false,
        })?;
        engine.register_method(name, method.clone());
        let capability = Capability::from_value(
            format!("host_{name}"),
            Value::object(vec![(b"send".to_vec(), method.value())]),
        );
        engine.declare_capability(&capability)?;
        capabilities.push(capability);
    }
    engine.set_output_writer(|_, _| Ok(()));
    engine.set_error_writer(|_, _| Ok(()));
    engine.set_random_source(|_, bytes| {
        bytes.fill(7);
        Ok(bytes.len())
    });
    Ok(capabilities)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let timer = Instant::now();
    black_box(timer.elapsed());
    let start = Stage::capture("process_start", 0);
    let now = Instant::now();
    let mut engine = black_box(Engine::new());
    let new = Stage::capture("engine_new", now.elapsed().as_nanos());
    let capabilities = registered(&mut engine)?;
    let registered = Stage::capture("registered", 0);
    let mut scripts = Vec::with_capacity(100);
    let now = Instant::now();
    scripts.push(engine.compile(programs::PROGRAMS[0].1)?);
    let first = Stage::capture("compiled_1", now.elapsed().as_nanos());
    let now = Instant::now();
    for (_, source) in &programs::PROGRAMS[1..10] {
        scripts.push(engine.compile(source)?);
    }
    let ten = Stage::capture("compiled_10", now.elapsed().as_nanos());
    let now = Instant::now();
    for (name, source) in &programs::PROGRAMS[10..] {
        scripts.push(
            engine
                .compile(source)
                .map_err(|error| format!("{name}: {error}"))?,
        );
    }
    let hundred = Stage::capture("compiled_100", now.elapsed().as_nanos());
    snapshot::pause("compiled_100");
    let mut steps = 0;
    let mut peak = 0;
    let mut retained = 0;
    let now = Instant::now();
    for i in 0..1_000 {
        let outcome = scripts[i % scripts.len()].call(
            "run",
            &[],
            CallOptions {
                capabilities: capabilities.clone(),
                ..CallOptions::default()
            },
        )?;
        steps += outcome.stats.steps;
        peak = peak.max(outcome.stats.peak_memory_bytes);
        retained += outcome.stats.retained_memory_bytes;
        black_box(outcome);
    }
    let calls = Stage::capture("calls_1000", now.elapsed().as_nanos());
    snapshot::pause("calls_1000");
    drop(scripts);
    let dropped = Stage::capture("scripts_dropped", 0);
    if std::env::args().any(|arg| arg == "--check") {
        if !cfg!(feature = "allocation-stats") {
            return Err("--check requires allocation-stats".into());
        }
        assert!(
            new.allocations[0] - start.allocations[0] <= 16,
            "Engine::new allocation count"
        );
        assert!(
            new.allocations[1] - start.allocations[1] <= 4_096,
            "Engine::new allocation volume"
        );
        assert!(
            first.allocations[0] - registered.allocations[0] <= 16_000,
            "first compile allocation count"
        );
        assert!(
            hundred.allocations[2] - ten.allocations[2] <= 90 * 28 * 1024,
            "per-script retained heap"
        );
    }
    for stage in [start, new, registered, first, ten, hundred, calls, dropped] {
        println!(
            "{}",
            serde_json::json!({
                "stage": stage.name, "allocations": stage.allocations[0],
                "allocated_bytes": stage.allocations[1], "live_bytes": stage.allocations[2],
                "peak_live_bytes": stage.allocations[3], "rss_bytes": stage.rss,
                "elapsed_ns": stage.elapsed_ns,
            })
        );
    }
    println!(
        "{}",
        serde_json::json!({"calls": 1000, "steps": steps, "tracked_peak_bytes": peak,
        "tracked_retained_bytes_sum": retained, "source_bytes": programs::PROGRAMS.iter().map(|(_, source)| source.len()).sum::<usize>()})
    );
    Ok(())
}
