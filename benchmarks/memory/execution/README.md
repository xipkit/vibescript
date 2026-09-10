# Execution allocation footprint

Baseline: `10ebb92c9967d0bad8a4b008071c882bcae9b511`. The final change reduces only the inline call-frame capacity from eight to four; receiver, environment and capability stacks keep their original capacities. The [representative comparison](representative/benchstat.txt) measured the layout change before commit; its manifest records the working-diff and binary hashes. The [depth comparison](depths/benchstat.txt) used commit `779d1afc` before rebasing. The [complete experiment patch](implementation.patch) preserves that committed revision against the baseline, including its benchmark fixture. Go 1.26.3, Apple M4, darwin/arm64, ten alternating 200 ms samples at GOMAXPROCS=1.

The Execution struct shrinks from **2,216 to 2,024 bytes**, moving its allocation from the 2,304-byte class into the 2,048-byte class. A short public Script.Call changes from **3,472 to 3,216 B/op (-7.37%)**, retaining eight allocations. Calls with 200 unused functions have the same reduction. [Layout probes](execution-layout-after.txt) confirm the struct size.

Arithmetic, pipelines, method dispatch and capability workloads each save 256 B/op. The complete-call and recursive-call CPU controls show no regression in this local matrix. The unused-functions call improves by 1.94%; other representative timings are statistically flat.

There is a deliberate allocation tradeoff once more than four call frames are live: the first spill adds one allocation, and cumulative allocated bytes become **128 bytes higher** than the baseline. This is about 0.6% of the recursive-Fibonacci fixture. The depth matrix covers shallow calls, both inline capacity boundaries and recursion to 128 live frames; timings remain statistically flat throughout. Keeping the eight-slot receiver buffer avoids a second extra allocation that a smaller receiver buffer caused in the initial experiment.

This is a per-execution layout change. It introduces no pooling, shared mutable execution state, new ownership rules, or changes to logical call-frame accounting. Existing closure, callback, capability re-entry, cancellation and quota behavior remains subject to the full suite and accounting oracles.

Reproduce with the patterns in the manifests and `-test.run=^$ -test.benchmem -test.cpu=1 -test.benchtime=200ms`, alternating baseline and head ten times. The native CI profile is `benchmarks/simd/execution-footprint.json`.
