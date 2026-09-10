# Bounded snapshots and compiler working memory

Baseline: `10ebb92c9967d0bad8a4b008071c882bcae9b511`. The measured implementation is `b6bd118d30a5e235951e3df95b039b5a0753ccbe` before rebasing onto the retention fixes. The [experiment patch](implementation.patch) preserves its source and fixtures against that baseline. Measurements use Go 1.26.3, Apple M4, darwin/arm64, GOMAXPROCS=1, and ten alternating 200 ms samples per revision. See the [manifest](paired/manifest.json), [raw baseline](paired/base.txt), [raw implementation](paired/head.txt), and [benchstat comparison](paired/benchstat.txt).

| Workload | B/op before | B/op after | Reduction | Allocations before → after |
| --- | ---: | ---: | ---: | ---: |
| Group 600 hash rows | 1,291,809 | 1,138,081 | 11.90% | 5,574 → 4,973 |
| Partition 600 hash rows | 1,269,809 | 1,116,208 | 12.10% | 5,549 → 4,949 |
| 80 JSON stringify calls | 53,562 | 38,201 | 28.68% | 520 → 440 |
| Compile 251 functions | 248,888 | 222,032 | 10.79% | 2,221 → 2,208 |

The JSON loop runs in 197.2 → 193.0 µs (-2.10%) and the function-heavy compilation in 280.5 → 265.1 µs (-5.47%). Grouping and partitioning timings are statistically unchanged. Small hash/object stringify improves by 1.86%/2.39%; larger objects and nested JSON show no significant timing difference. The control-flow and typed compilation fixtures retain their byte/allocation counts and show no significant timing difference.

Each host clone or stringify operation shares a bounded buffer of eight entries across its entire recursive traversal. Parent values hold disjoint slots until their children return. Larger objects and exhausted buffers use the existing allocation path. This saves 512 bytes for the nested host-clone fixtures, rather than adding an inline array to every recursive frame. At depth four, cloning changes from 3,584 to 3,072 B/op and improves time by 3%; depth 64 and 256 timings are statistically unchanged. The compiler reuses its existing class count to skip the directive-collision map when there are no classes or modules to check.

The [memory probe](snapshot_memory_probe_test.go.txt) samples allocated bytes and stack growth immediately after a call, in a fresh process per shape/depth, with GC disabled during the operation. These are observed post-call measurements, not an exact instantaneous heap peak. The [baseline](stack/base.txt) and [implementation](stack/head.txt) show:

| Shape | Depth | Allocated bytes before → after | Stack growth before → after |
| --- | ---: | ---: | ---: |
| Nested JSON objects | 256 | 42,088 → 41,704 | 256 KiB → 256 KiB |
| Nested JSON objects | 8,192 | 1,458,360 → 1,457,976 | 8 MiB → 8 MiB |
| Nested JSON arrays | 256 | 27,304 → 27,304 | 256 KiB → 256 KiB |
| Nested JSON arrays | 8,192 | 891,064 → 891,064 | 8 MiB → 8 MiB |
| Nested host hashes | 256 | 217,520 → 217,008 | 256 KiB → 256 KiB |
| Nested host hashes | 8,192 | 7,140,176 → 7,139,664 | 8 MiB → 8 MiB |

The per-frame scratch prototype was rejected after doubling stack growth on deep values. The retained design keeps object rendering in the original recursive dispatcher and acquires scratch through a nonrecursive helper. Regression coverage includes nested siblings, buffer boundaries, insertion/fallback order, invalid UTF-8, cycles, depth limits, independent host mutation, aliases, and exact reserved hash capacities. The complete suites pass with estimator, environment recycling and builtin-contract verification on Go 1.26.3 and Go 1.27.1 with SIMD; vet and lint pass.

Reproduce CPU/allocation measurements with the manifest pattern and `-test.run=^$ -test.benchmem -test.cpu=1 -test.benchtime=200ms`, alternating the two revisions ten times. Copy the diagnostic probe into `internal/runtime` in disposable worktrees; run each probe subtest in a separate process with `GOGC=off`, `GOMAXPROCS=1`, and `-test.count=1 -test.v`. Native CI coverage is selected by `benchmarks/simd/memory-allocations.json`.
