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

## Declaration and checker measurements

These pairs alternate before/after order between repetitions. Local test and build processes were paused. An earlier declaration timing batch showed high variance and was discarded; the files below are the paired rerun. Allocation counts also have deterministic regression coverage.

- `declarations-stack`: six paired 200ms samples, counts 0 and 1,000 for `BenchmarkCallUnusedDeclarations`; before `349d70675d5692bda3496ee9cbe6cfcdea9e4b63`, after `ffbdc588b9474d03ccc1064a1ee3119b0091b5fc`.
- `checker-stack`: three paired 100ms samples for `BenchmarkCheckIndependentDeclarations`; before `ffbdc588b9474d03ccc1064a1ee3119b0091b5fc`, after `6939d258a5f135b0e1ce9fe0706e0b9030557e3d`.

Each pair uses identical benchmark functions. These baselines include the preceding fixes in the PR stack, so each comparison isolates its own change.

## Final allocation layout

CI caught a 3,504 B short-call allocation against its 3,500 B budget. Moving one `Env` field removes padding and reduces the call to 3,472 B without changing the budget. The following paired reruns supersede the declaration/checker stack measurements above for the delivered heads:

- `declarations-final`: six paired 200ms samples, counts 0 and 1,000; before `349d70675d5692bda3496ee9cbe6cfcdea9e4b63`, after `ab4a60111476bd126fac92d5689ce688e2278518`.
- `checker-final`: three paired 100ms samples, counts 100, 200 and 400; before `ab4a60111476bd126fac92d5689ce688e2278518`, after `900a1a3b549f80cff40ed0544db221f4c39222de`.

Both use the benchmark functions named above, with identical source in each pair. Before/after order alternates between repetitions. Other task-owned tests and builds were paused; ordinary macOS background activity continued.
