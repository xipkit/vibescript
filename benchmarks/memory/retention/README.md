# Memory retention fixes

Baseline: `10ebb92c9967d0bad8a4b008071c882bcae9b511`. Measured implementation: `11d86c7a8fca9d94d8f041b49750241df9c82bd7` (later changes only preallocate a test assertion buffer and publish this evidence). Go 1.26.3, Apple M4, darwin/arm64.

The new regression tests fail on the baseline and pass on the implementation. Each heap test forces GC and keeps the result or execution alive. Measurements run serially within each test process; small negative deltas reflect allocator noise.

| Retained result | Before | After |
| --- | ---: | ---: |
| 24 one-byte regex matches | 12,772,736 B | approximately zero |
| 12 borrowed patterns, cache capacity 64 | 12,731,424 B | 48,760 B |
| 24 deleted one-MiB hash keys, active call | 25,217,768 B | 46,160 B |
| Popped receiver, depth 16, active call | 16,786,296 B | 8,600 B |
| Completed rescue, depth 16, active call | 4,223,992 B | 12,208 B |

The regex-cache test also retains evicted regexps, covering expression ownership beyond the LRU entry. The receiver tests cover normal returns and exceptional unwinds, both within and beyond the inline stack. Hash tests cover repeated deletes and reconciliation after live-map edits.

[CPU and allocation comparison](benchstat.txt) uses ten alternating 200 ms samples per revision at GOMAXPROCS=1. Regex partial matches now allocate an owned result and preflight its peak memory: 3.111 to 3.406 microseconds, 25 to 26 allocations in the complete-call fixture. Cache misses add one small pattern allocation. Cache hits remain allocation-free; their microbenchmark moved from 6.668 to 7.242 ns. Full matches, misses, method dispatch, hash deletion and rescue loops had no statistically significant timing change in this run.

The full suite passed with estimator verification, environment recycling verification, and builtin contract verification on Go 1.26.3 and Go 1.27.1 with SIMD. Native ARM64 and x86 benchmark fixtures are registered in `benchmarks/simd/memory-retention.json`.

Reproduce heap checks with the tests named in the logs. To reproduce the CPU comparison, copy `internal/runtime/memory_retention_benchmark_test.go` to the baseline and build both runtime test binaries with one toolchain. Run the pattern recorded in [manifest.json](manifest.json) with `-test.run=^$ -test.benchmem -test.cpu=1 -test.benchtime=200ms`, alternating revisions ten times.
