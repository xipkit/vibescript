**Go 1.27 SIMD can accelerate Vibescript's byte scans while preserving its accounting.** An ARM64 prototype reduced the time of existing ASCII string-call benchmarks by 50–68%, with no additional heap allocations. A normal 64-bit word scan already achieved 43–61% reductions, so that is the simpler first production change. SIMD provided a further 12–18% reduction against the word scan in these workloads.

This is an analysis and measurement fixture, not a production implementation. It is based on commit [`e6f1fca90c012885453a6c413c0826f6abff1f32`](https://github.com/xipkit/vibescript/tree/e6f1fca90c012885453a6c413c0826f6abff1f32), measured on an Apple M4, darwin/arm64, using Go 1.27.1 with `GOEXPERIMENT=simd`. The production sources and `go.mod` are unchanged; Go overlays substitute only `stringIsASCII` during measurements.

Go 1.27 adds experimental portable `simd` APIs and ARM64 NEON support in `simd/archsimd`. Both require the experiment flag and have unstable APIs. The portable API specializes explicit SIMD-dependent functions and dispatches by CPU capabilities; enabling the flag does not automatically vectorize Vibescript's ordinary scalar loops. This prototype uses the ARM64-specific API, not portable dispatch. [Release notes](https://go.dev/doc/go1.27#simd), [compiler specialization design](https://github.com/golang/go/blob/go1.27.1/src/cmd/compile/internal/midway/rewrite.go#L16-L71).

The existing language benchmarks each perform 200 calls using roughly 4 KiB strings. All three variants use the same compiler, experiment flag, source revision, and benchmark fixture. Six trials rotate the variant order; each benchmark runs for 200 ms per trial. Values below are medians in microseconds per 200-call loop, not per individual language call.

| ASCII workload | Current scalar loop | Ordinary word scan | SIMD scan | SIMD time reduction vs current | Further reduction vs word |
|---|---:|---:|---:|---:|---:|
| `.length` | 272.9 µs | 131.2 µs | 109.1 µs | 60.0% | 16.8% |
| `.index` | 301.4 µs | 156.8 µs | 134.9 µs | 55.2% | 14.0% |
| `.rindex` | 483.8 µs | 189.0 µs | 154.9 µs | 68.0% | 18.0% |
| `.slice` | 339.9 µs | 192.6 µs | 170.5 µs | 49.9% | 11.5% |

All four SIMD/current time differences have benchstat p=0.002, n=6. Allocation counts remain exactly 18 per loop for length/index/rindex and 419 for slice. Allocated bytes remain approximately 4.5 KiB and 8.4 KiB respectively, with a few bytes of run-to-run measurement variation. This is a CPU optimization, not a reduction in retained script memory. See [raw results and benchstat](results/end-to-end-benchstat.txt).

The isolated 4 KiB ASCII predicate costs 970 ns in the current loop, 201 ns with word scanning, and 145 ns with SIMD: 4.8× and 6.7× faster respectively. All three allocate 0 B/op and 0 allocs/op. SIMD is not universally faster: for an 8-byte ASCII string it costs 3.08 ns versus 2.83 ns currently, and an immediate high-bit byte costs about 1.12 ns versus 0.75 ns in the 4 KiB fixture. Preserve a short-input path and measure representative input lengths before choosing a threshold. [All kernel samples](results/kernels.txt).

Unicode index/rindex/slice controls changed by less than 1% in median time. Unicode length improved by 32% in both optimized builds, but its classifier exits at the second byte, so this is not evidence of SIMD accelerating UTF-8 decoding. Disassembly shows secondary register-allocation and spill differences in the inlined rune-counting loop; every variant still calls the same scalar decoder. The full size of that improvement remains unattributed and is excluded from the SIMD recommendation.

The most promising follow-up targets are:

| Priority | Candidate | Expected benefit and constraint |
|---|---|---|
| 1 | `stringIsASCII` | Measured here. Read-only, contiguous bytes, scalar result; preserves all callers' existing quota and ownership behavior. |
| 2 | JSON parse string scanning | Skip safe ASCII runs, then resume at the first quote, backslash, control byte, or high bit. Preserve UTF-8/error handling and `strings.Clone` so small tokens do not retain entire documents. Needs long-token benchmarks. |
| 3 | JSON stringify string scanning | Skip safe spans before escaping; also recognize `<`, `>`, and `&`. Preserve output-byte charging, expansion preflights, invalid UTF-8 replacement, and U+2028/U+2029 escaping. |
| 4 | Explicit ASCII case transforms | Independent byte operations fit SIMD. Preserve the existing output/scratch reservation; SIMD does not remove those buffers. Full Unicode case conversion is a different problem. |
| 5 | Whitespace scanning | Long runs can be classified in batches. Ruby split, Ruby strip, and JSON have different whitespace sets. Preserve projection and materialization agreement. |
| 6 | ASCII case comparison | Useful for long equal strings/common prefixes. Preserve first-mismatch ordering, punctuation behavior, and Unicode fallbacks. |

These are predictions except for the measured ASCII predicate. [The candidate analysis](opportunities.md) has pinned source links and test coverage. Existing `strings.Index`, `IndexByte`, and `Count` paths already reach optimized standard-library assembly; custom scalar classification around them is the new opportunity.

**Accounting remains correct only if the optimization preserves the existing logical contract.** The current byte-scan convention is `floor(bytes/64)` steps, not one step per vector. Other calls have different contracts: `String#count` charges per rune plus character-set probes. Chunk boundaries must preserve cumulative charges and cancellation checks. `stepN` detects crossed periodic boundaries, but performs the slow checks once; it does not poll context during a long kernel. This ASCII overlay preserves the existing full-scan cancellation behavior and does not establish a tighter latency bound.

There was a concrete memory pitfall in the first probe: `archsimd.LoadUint8x16([]byte(text))` inside the shrinking-string loop caused **254 heap allocations for a 4 KiB scan** on Go 1.27.1. The compiler did not eliminate those string-to-byte copies. Such temporary input buffers would be invisible to the reachable-value estimator unless explicitly charged. The final probe copies exactly 16 bytes into a fixed local array and loads that array. It has zero measured heap allocations; disassembly confirms NEON `VUMAXV` and a bounded stack tile. This is a prototype mistake caught by validation, not an existing Vibescript bug.

Keep new dynamic packing buffers, structural-index tables, and pooled scratch out of a first implementation. If introduced later, reserve and check their concurrent peak before allocating, and release the reservation only after their lifetime ends. Passing the estimator oracle alone cannot detect scratch omitted by both estimator paths. Preserve output cloning, collection ownership, mutation epochs, and pointer write barriers. [Detailed accounting trace](accounting-analysis.md).

Avoid SIMD reductions over current numeric arrays initially. Arrays contain heterogeneous 32-byte `Value` elements on this machine, not packed numbers. Unboxing adds work and scratch. `Array#sum` is an ordered fold with arbitrary-precision promotion and intermediate memory checks; tree reductions can change floating-point answers, temporary big-integer allocations, and quota failures. Callback forms also have observable ordering. Checker, environment, and memory-estimator graph walks are similarly poor SIMD targets.

Validation completed for the final SIMD overlay:

- Predicate parity for every single-byte substitution in otherwise ASCII strings at lengths 0–129, covering every position and byte value; zero-allocation checks through 1 MiB.
- `go test ./... -count=1` with the overlay and Go 1.27.1 SIMD enabled.
- Full runtime suite with `VIBES_ESTIMATOR_VERIFY=1` and `VIBES_ENV_RECYCLE_VERIFY=1`, including existing quota, cancellation, JSON-retention, and builder-capacity tests.
- `go vet ./...` with the overlay; formatted Go probe and Python fixture syntax checks.
- Word-only overlay compiled and passed builder-capacity tests on Go 1.26.3; its production packages also cross-built for linux/386. The common SIMD measurement fixture itself requires ARM64 and Go 1.27.1. The 32-bit test suite has existing 64-bit-only quota constants, so the cross-platform check uses `go build ./...`, as CI does.

A Go 1.27 upgrade also requires a CI adjustment independent of SIMD/accounting: [the leak-profile job](https://github.com/xipkit/vibescript/blob/e6f1fca90c012885453a6c413c0826f6abff1f32/.github/workflows/test.yml#L147) still sets `GOEXPERIMENT=goroutineleakprofile`. Go 1.27.1 rejects that removed flag with `go: unknown GOEXPERIMENT goroutineleakprofile`. Allocator-rounding tests passed on this ARM64 toolchain; the full supported-platform matrix still needs validation before an upgrade. [Go 1.27 release notes](https://go.dev/doc/go1.27).

The recommended sequence is: implement the portable word scan first, retaining its quota behavior; measure JSON ASCII-run scanning with long plain, escaped, and Unicode payloads; then add optional architecture-specific SIMD only where its incremental gain justifies the experimental build dependency. Any production SIMD change needs scalar fallback, supported-target build coverage, exact quota-threshold parity, and a short-input policy. The current measurements establish useful local headroom, not a general application-wide speedup or an AMD64 result.

To reproduce the ARM64 assessment from this branch:

```sh
python3 benchmarks/audits/2026-09-08-simd/measure.py /tmp/vibescript-simd-rerun
```

The script creates overlays, builds three test binaries, measures kernels and language calls, then runs tests and accounting oracles. Production files remain unchanged. Benchstat used for this report was `golang.org/x/perf` version `v0.0.0-20260908200009-22c9c6c9d4da`. The report retains [the fixture](probe_test.go.txt), [overlay generator](prepare.py), [measurement runner](measure.py), and raw results.

With that benchstat installed, regenerate the comparison using:

```sh
benchstat /tmp/vibescript-simd-rerun/end-to-end-scalar.txt /tmp/vibescript-simd-rerun/end-to-end-word.txt /tmp/vibescript-simd-rerun/end-to-end-simd.txt
```

The runner does not regenerate the disassembly excerpts. Those can be inspected with `go tool objdump` against the generated test binaries.
