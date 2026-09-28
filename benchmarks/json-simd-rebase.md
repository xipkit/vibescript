# JSON SIMD on the typed VM

The JSON branch was rebased onto `mgomes/rust-language` at `6bad12f`, which includes the typed VM. This report supersedes the pre-VM timings in the [regression and profile report](json-simd-followup.md).

## Integration

All 13 commits replayed without textual conflicts. Upstream's VM, bytecode, shared literals, smaller initial buffers, `records::Fields`, documentation and golden observations were retained. The JSON and scanner source is identical to the pre-rebase branch; this pass adds no accounting changes and does not adopt `records::Fields`.

The golden counter files started byte-for-byte from upstream. Upstream and rebased golden harnesses were run from the same checkout to distinguish intentional JSON storage reductions from counters affected by checkout-path length. Only the selected JSON counter changes were re-recorded, with observations preserved. The [Counter log](../tests/golden/README.md#counter-log) records the reason.

At the pinned upstream revision, `compare.py` still invokes `/usr/bin/time -l` and there is no tracked `scripts/rss.py`. The branch keeps one Python RSS helper and calls it once per case. It normalizes Linux KiB to bytes, retains Darwin's byte units, and propagates the child's exit status.

## Measurements

Darwin (Apple M4), upstream `6bad12f` → rebased JSON runtime `e83d53f`. Later commits contain only counters, documentation and evidence. The measured runtime is unchanged. Both runs used the same binary hashes, the same generated 1,048,536-byte packet (the array variant omits its wrapper), four rotating rounds, and a 75 ms target with at least 20 calls per sample. No gate was running before or after either run.

A second complete four-round run checked a +4.3% unlimited SIMD standalone-stringify result in the first run. The repeat measured +0.3%; combining all eight samples gives +2.7%. No rounds or workloads were discarded. The tables use the median of all eight samples; both separate runs and their per-run changes are retained in the [complete CSV](results/json-simd-rebase/all-cases.csv) and evidence archives.

Metered time, ms/call:

| 1 MiB workload | Portable | SIMD |
| --- | ---: | ---: |
| Parse | 8.498 → 6.727 (-20.8%) | 8.740 → 6.715 (-23.2%) |
| Parse as shape | 10.232 → 8.186 (-20.0%) | 10.158 → 8.229 (-19.0%) |
| Parse as array<shape> | 10.485 → 7.965 (-24.0%) | 10.383 → 7.933 (-23.6%) |
| Project then stringify | 13.117 → 10.805 (-17.6%) | 13.083 → 10.863 (-17.0%) |
| Standalone stringify | 8.254 → 8.157 (-1.2%) | 8.109 → 8.154 (+0.6%) |

Unlimited time, ms/call:

| 1 MiB workload | Portable | SIMD |
| --- | ---: | ---: |
| Parse | 9.286 → 6.775 (-27.0%) | 8.764 → 6.899 (-21.3%) |
| Parse as shape | 10.113 → 8.141 (-19.5%) | 10.145 → 7.901 (-22.1%) |
| Parse as array<shape> | 10.129 → 7.915 (-21.9%) | 10.180 → 8.003 (-21.4%) |
| Project then stringify | 13.218 → 10.689 (-19.1%) | 13.688 → 10.538 (-23.0%) |
| Standalone stringify | 8.114 → 8.127 (+0.2%) | 8.071 → 8.289 (+2.7%) |

The parse, typed-parse and projection gains remain on top of the typed VM. Standalone stringify remains within the approximate 3% band in the combined samples. Steps, allocation counts and tracked bytes agree between portable and SIMD builds; steps also equal upstream for every measured case.

Memory for the metered parse case (MiB; allocations are counts). Allocation and tracked-memory counters are identical in both runs; RSS is the median of two fresh-process observations and includes the harness:

| Build | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: |
| portable | 301,404 → 147,540 | 15.27 → 10.36 | 14.26 → 9.35 | 67.09 → 48.03 |
| simd | 301,404 → 147,540 | 15.27 → 10.36 | 14.26 → 9.35 | 66.70 → 47.70 |

The archived drivers select the five 1 MiB workloads from `scripts/json_fixtures.py` and invoke the existing `compare.py` harness. Fresh upstream builds supply the baseline; the final run uses `--skip-build` with identical binaries. Both runs retain timing rounds, pilots, allocation records, RSS readings, input fixtures, compiler/environment metadata and build logs: [first run](results/json-simd-rebase/arm64-evidence.tgz), [repeat](results/json-simd-rebase/arm64-repeat-evidence.tgz).

## Counters and verification

The paired engine audit covers 233,803 cases. Only ten recorded peak counters decrease: eight mixed-depth JSON cases and both transform modes. Steps and retained bytes are unchanged. Four clock/UUID observations vary as their existing goldens explicitly allow; no unexpected observation differs. All observation files and all other counter files remain byte-for-byte upstream. [Every changed counter](results/json-simd-rebase/counter-changes.json) includes upstream and rebased values.

Local fmt, both all-target Clippy configurations with warnings denied, all 1,998 native tests, and all eight golden corpora passed. Portable/SIMD validation passed all 107,448 cases with exact step, peak-memory and retained-memory equality. WASI passed 1,847 tests, both Clippy configurations, and the CLI/filesystem witnesses. [Local verification evidence](results/json-simd-rebase/verification-local.tgz) retains the commands and logs. The prescribed three-host gate will run on the final report commit; its exact head and result will be recorded in the delivery message.
