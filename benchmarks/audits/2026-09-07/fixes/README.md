# Fix validation

Measurements taken on 2026-09-08 with Go 1.26.3, darwin/arm64, Apple M4. Benchmark processes ran serially with `GOMAXPROCS=1` and `-test.cpu=1`; other local tests and builds were paused.

Each before binary uses production commit `a1fb44d4ec8685665ffebfc7d6f56aa7261e8911` with the same benchmark functions as its after binary. The raw files retain every sample.

| Prefix | After commit | Runs | Time per benchmark | Selection |
| --- | --- | ---: | --- | --- |
| env | d229b891c0f9d2c86505ef8200f3d256a26a07a7 | 3 | 100ms | `BenchmarkExecutionRecursiveFib` |
| scan | f158a568a0ee02d714377bc5caf279899cd635ca | 3 | 100ms | `BenchmarkStringScan(EarlyReturn\|BlockDrain\|SparseDrain)` |
| epoch | dc16758c8bc6930ccd9f88d2c953b5d00fde79e4 | 6 | 500ms | `Benchmark(ExecutionArithmeticLoop\|HashReadLoopUnderQuota\|MemoryMemoMutationIsolation)` |

Run a compiled test binary with `-test.run='^$' -test.bench='<selection>' -test.benchmem -test.cpu=1 -test.count=<runs> -test.benchtime=<time>`. Selections were anchored at both ends.

The epoch numbers measure isolated quota checks, not concurrent application throughput. Its overflow cases demonstrate the conservative full-walk fallback. The scan tests document a bounded-table continuation fallback for patterns at Go's maximum regex tree depth; first-match return avoids that table.
