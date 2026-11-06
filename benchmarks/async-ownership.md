# Async VM ownership transfers

VIBE-179 removes all 31 production unwrap sites in `src/vm/asynchronous.rs`. Inline work borrows its enclosing execution, and a dropped unfinished borrow latches a terminal error. Worker submission and finalization remain owned. `unwrap_used` remains allowed for the coordinated rollout.

## Timing and accounting

Baseline: `0d7e07e2`. Both binaries use the same external host harness, dependency lock (Tokio 1.54.1), and repository release settings: optimization level 3, thin LTO, one codegen unit, debug level 1 and overflow checks. Measurements ran on an Apple M4 with Homebrew rustc 1.98.1 / LLVM 22.1.8, using the default SIMD feature and a current-thread Tokio runtime with one script worker. The repository integration tests separately use its pinned Tokio 1.53.1.

Each script performs 500 host/block iterations. Each process warms up four calls and records twelve calls per case. Sixteen before/after pairs alternate which binary runs first. The table gives medians of the sixteen process medians and the median of the sixteen paired after/before ratios; the latter need not equal the ratio of the displayed medians. The shared fleet machine was busy, so these are regression observations, not an overall speedup claim.

| Workload | Before (µs/call) | After (µs/call) | Paired change | Peak bytes, before → after | Steps |
|---|---:|---:|---:|---:|---:|
| `sync` | 995.0 | 976.0 | -1.84% | 5,804 → 5,796 | 28,019 |
| `inline_async` | 1,865.5 | 1,722.5 | -10.00% | 8,833 → 8,753 | 38,018 |
| `nested_inline` | 3,020.5 | 2,795.0 | +0.46% | 13,441 → 13,281 | 57,518 |
| `inline_yield` | 2,189.5 | 2,248.0 | +0.36% | 9,029 → 8,949 | 43,517 |
| `worker_async` | 12,589.5 | 12,715.0 | -0.62% | 7,669 → 7,885 | 28,019 |

Every call returned 500. Logical steps match before/after in every case, and retained tracked bytes are zero. Inline peaks fall by 8–160 bytes. The worker async peak increases by 216 bytes because the private host-invocation future has a different layout; this is an intentional accounting change, not untracked storage. Exact-limit and one-byte/one-step-short host tests pass with the new peaks.

No workload showed a paired median increase above 0.5% in this sample. The variation between aggregate and paired medians is a reason to avoid interpreting the measured reductions as a reliable speedup.

## Verification

- Fresh Adamantium audits with `unwrap_used` temporarily removed from the allow list report zero active findings in this file, with all features and without default features. The allow list was restored, and the normal audit is clean.
- Final optimized runtime: 755 library tests and 1,278 integration tests pass. Portable workspace libraries/integration: 2,215 tests pass. All 61 doctests pass; Clippy passes with both feature configurations.
- New nested-success, abandonment, caught-panic and prior-quota cases assert the same context address, terminal errors, no rescue/ensure after failure, zero retained bytes on success, worker-permit release and exactly-once finalization.
- The missing-run host-access matrix covers synchronous, worker and inline callbacks, each with receiver reads, receiver writes and block calls, both with and without a previously latched quota. Repairing the injected run and ignoring the error cannot resume the invocation.
- Both new regression tests passed 100 additional optimized repetitions each. Existing native host integration covers cancellation, deadline, panic, recursion, snapshots, capabilities and exact accounting.
- The real CLI produced `[[2,4,6],1208925819614629174706176,"0007",true]` for iteration, big integers, formatting and regex.

Public method signatures are unchanged. The private mutable trait-object backend makes the async handle lifetime invariant, matching `HostCall`; callbacks still receive a `Send`, non-`Sync` scoped handle. Read-only review found inherited caught-panic recovery concerns, tracked separately in VIBE-190 and VIBE-191. Those follow-ups have not yet been runtime reproduced.

## Reproduction fixture

The harness below is built in a separate Cargo package against each checkout. Enable `vibescript/tokio`; use Tokio 1.54.1 with `rt`, `time` and `macros`, and the release settings above. Preserve one dependency lock for both builds. Run the resulting binaries in alternating order; compilation is outside all timed intervals.

```rust
use std::time::Instant;
use vibescript::{CallOptions, Engine, HostMethod, Value, asynchronous::Runner};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut engine = Engine::new();
    engine.register_method("sync", HostMethod::new_with_block("sync", |call, _, _| call.call_block(&[])));
    engine.register_method("async", HostMethod::new_async("async", |call, _, _| Box::pin(async move { call.call_block(vec![]).await })));
    engine.register_method("tick", HostMethod::new_async("tick", |_, _, _| Box::pin(async { tokio::task::yield_now().await; Ok(Value::int(1)) })));
    let runner = Runner::new(1).unwrap();
    for (name, body) in [
        ("sync", "sync { 1 }"),
        ("inline_async", "sync { async { 1 } }"),
        ("nested_inline", "sync { async { sync { async { 1 } } } }"),
        ("inline_yield", "sync { async { tick() } }"),
        ("worker_async", "async { 1 }"),
    ] {
        let source = format!("def run -> int; n = 0; 500.times {{ n += ({body}).as(int) }}; n; end");
        let script = engine.compile(&source).unwrap();
        let mut samples = Vec::new();
        let mut stats = None;
        for round in 0..16 {
            let start = Instant::now();
            let result = runner.call(script.clone(), "run".into(), vec![], CallOptions::default()).await.unwrap();
            let micros = start.elapsed().as_micros();
            assert_eq!(result.value.as_int(), Some(500));
            stats = Some(result.stats);
            if round >= 4 { samples.push(micros); }
        }
        samples.sort_unstable();
        println!("{name}: median_us={} samples={samples:?} stats={:?}", samples[samples.len()/2], stats.unwrap());
    }
}
```

Raw samples, the dependency lock and test logs remain under `/Volumes/AI/Build/tron-vibe-179` until the card is complete. The paired binary SHA-256 digests are:

- before: `9287431ca9b9c76abf9e5762db30ced02466e81af44408c16f8bf08759f157f7`
- after: `eeebb78a323e8b354f58e47fc0fe7d9c4a339c77ad7db1068f32f2391202dcec`
