# Native SIMD delivery measurements

These measurements compare master `7a3ba6469b50c5fa7b43764a70f6fc32d7fb3efa`, the shared ASCII scanner `892bc640542acba2d8db7ba93b4ec9b49cd29e76`, and integration snapshot `3c1891543e6fc02cefb1548d0d04af3dde0d7d55`. The integration snapshot combines PRs #1281, #1282, #1283, #1285, and #1286 on #1290. Lexer and formatter improvements were already in the baseline.

The snapshot predates the scalar whitespace follow-up in PR #1283. Its disabled-AVX2 trim regression is explicitly retained below; these measurements must not be presented as measurements of that follow-up.

## Ordinary embedding executables

The driver uses the public `vibes` and `vibes/value` APIs and is compiled with `go build`. It registers the same 168 public-call workloads through `testing.Main`, without compiling the runtime package's test files. Only mixed-document JSON setup changes to use a public JSON parse call before timing. Every version receives identical generated driver files. Six 100 ms samples alternate revision order, under Go 1.27.1 and one CPU. Native x86 samples are pinned to one logical CPU.

Below are medians in microseconds for the original baseline and combined snapshot, both built with `GOEXPERIMENT=simd`. String loop inputs are about 4 KiB; other rows are individual complete calls.

| Workload | Apple M4 before | Apple M4 after | AMD EPYC 9V74 before | AMD EPYC 9V74 after |
|---|---:|---:|---:|---:|
| ASCII length, 200 calls | 278.95 | 119.62 | 643.08 | 248.30 |
| ASCII index, 200 calls | 310.80 | 147.75 | 457.77 | 277.37 |
| ASCII rindex, 200 calls | 489.78 | 169.27 | 898.61 | 327.72 |
| ASCII slice, 200 calls | 347.99 | 184.15 | 575.25 | 403.14 |
| ASCII upcase, 64 KiB | 30.92 | 14.47 | 43.55 | 30.48 |
| ASCII casecmp, 64 KiB equal | 48.59 | 6.27 | 68.21 | 12.07 |
| JSON parse, 64 KiB ASCII | 69.74 | 10.09 | 71.44 | 16.98 |
| JSON generate, 64 KiB ASCII | 78.98 | 16.60 | 100.54 | 27.78 |
| Regexp.escape, 64 KiB plain | 52.80 | 6.24 | 81.77 | 25.93 |
| Strip, 64 KiB long padding | 25.90 | 4.89 | 32.67 | 11.66 |
| Split, 64 KiB long fields | 101.02 | 57.73 | 147.67 | 104.14 |
| Split, 64 KiB all whitespace | 27.50 | 4.83 | 35.44 | 11.12 |

The ARM combined snapshot has no statistically significant time regressions across the 168 workloads. Allocation medians match except fractional/one-object variation in a few large whitespace splits containing 14,000–28,000 allocations; the scanner helpers remain allocation-free. Exact accounting and cancellation contracts are tested separately.

The x86 result has material tradeoffs; it is not an across-the-board speedup:

- In the default Go 1.27 build, the combined Unicode index loop takes 10.23% longer. The ASCII scanner alone also shows Unicode length +8.44% and index +10.76% in this executable.
- With SIMD enabled, the combined Unicode case defaults take 3.89–7.98% longer and valid-Unicode casecmp? takes 11.14–12.41% longer. These retain their existing Unicode algorithms.
- SIMD dense-escape JSON parsing takes 8.18–10.62% longer. Several Unicode/mixed JSON controls take roughly 3–5% longer. Common short fields and small trim controls are included in the raw results.
- With AVX2 disabled, long-padding trim takes 14.62–27.70% longer in this snapshot. The scalar trim-mode specialization being validated in #1283 addresses the per-byte fallback predicate cost.

Source and flags are recorded in each manifest. The native run is [34357310500](https://github.com/xipkit/vibescript/actions/runs/34357310500). Driver generation and execution are in [the diagnostic commit](https://github.com/xipkit/vibescript/tree/5c952809/scripts).

## Function layout investigation

[Run 34356436980](https://github.com/xipkit/vibescript/actions/runs/34356436980) measures the ASCII, JSON, regexp, and combined changes with the default layout and twelve independent Go linker layouts (`-ldflags=-randlayout=1` through `12`), under matched default/SIMD flags. Statistical layout comparisons exclude the default seed, use one calibrated 100 ms sample per independent layout, and include matching fixture hashes and benchmark names.

The earlier fixed-binary SIMD Unicode-length regression of 30.9% does not remain significant across the twelve ASCII layouts. Default mixed-index prefixes still show 7.22–9.26% regressions. The combined SIMD layout comparison retains a 5.87% dense-escape parse regression; its default build has dense-escape parse regressions of 7.70–8.25%. Twelve layouts do not establish zero cost for every unrelated control.

A separate pinned Go 1.27.1 AMD64 compiler audit finds all 15 selected Unicode helpers and length callbacks instruction-identical between the ASCII and regexp revisions, under each flag set. The ASCII revision's default/SIMD versions differ in those helpers only in the classifier call target. The baseline-to-ASCII change itself also changes inlining and register allocation, so not every observed delta can be assigned to layout alone. [Go issue 8717](https://github.com/golang/go/issues/8717) describes related effects in other programs; it is supporting context, not proof of this program's cause.

The practical conclusion is to report workload and build context alongside gains, preserve the scalar contracts, and distinguish an algorithmic regression from a change in generated-code placement. No production padding or linker alignment overrides were added.

## Evidence

`evidence.tar.gz` contains complete ordinary-executable raw samples, manifests, benchmark comparisons, and the ASCII/combined layout samples. Reproduce the ordinary executable with `scripts/simd_embedding_probe.py` and `scripts/simd_embedding_measure.py` at the diagnostic commit above. The layout driver is at commit `27b6963cc525c7ca437f367dbb077d084e531b09` in `scripts/simd_layout_diagnostic.py`. The diagnostic workflow branch is experimental infrastructure and is not part of the implementation PRs.

Archive SHA-256: `e5f66583dc9aa215339b171ab277a74ce4d99b03a4cb9b0d408800b97f02b4f8`.

## Retained whitespace fallback

[Run 34366852190](https://github.com/xipkit/vibescript/actions/runs/34366852190) adds a focused ordinary-executable check on AMD EPYC 7763. It uses the same generated driver, times 38 whitespace/string controls, and collects six alternating 100 ms samples under default, SIMD, and AVX2-disabled flags. Its `base` revision is the original `7a3ba646`; `ascii` is the retained combined snapshot `64bb2d46`, including scalar trim-mode specialization; `head` is a later dispatch experiment `e487e2d6` that was rejected. The manifest records these roles and full SHAs.

The retained snapshot is byte-identical to PR #1283 head `2a798d45fe46cc5261c13bfb41b40a399278fec1`, tree `d62a8737ee28d585eec29f3ff90e26d6680d527f`. Its complete Go 1.26.3 and Go 1.27.1 SIMD suites passed with both `VIBES_ESTIMATOR_VERIFY=1` and `VIBES_ENV_RECYCLE_VERIFY=1`.

| 64 KiB complete call | Original baseline | Retained snapshot |
|---|---:|---:|
| SIMD long-padding trim | 35.99 µs | 13.53 µs |
| SIMD split long fields | 173.29 µs | 127.75 µs |
| SIMD split all whitespace | 37.66 µs | 13.20 µs |
| AVX2-disabled long-padding trim | 35.55 µs | 35.61 µs |

Allocation counts match for these calls. Small AVX2-disabled trim cases cost 1–2.2% more; 64 KiB trim is statistically unchanged. The later dispatch experiment made that long trim 28.09% slower (35.61 → 45.61 µs), so its extra helper/dispatch code was removed. The retained implementation specializes the trim-mode predicate outside each scalar loop. This native check supplements the earlier M4 forced-scalar validation.

Build/workload tradeoffs remain: the retained snapshot's SIMD Unicode index and rindex controls are 16.47% and 5.39% slower on this host, while Unicode length and slice improve. The default-build comparison has no significant regressions of 3% or more among these 38 controls. These are distinct builds and CPUs from the first report; do not attribute every delta to one algorithm. Remaining Unicode and dense-escape costs are tracked in [#1291](https://github.com/xipkit/vibescript/issues/1291).

`whitespace-final-evidence.tar.gz` includes the complete focused native data, driver, comparisons, local scalar-specialization data, full accounting-oracle logs, and publication tree proof. Archive SHA-256: `95e56f6fc9a36a92f9023dc7d94782bd6a611aa89ea572e7f70cb094ae8fa070`.

## Final PR acceptance and delivery

[PR run 34368472397](https://github.com/xipkit/vibescript/actions/runs/34368472397) compares final head `2a798d45` with merged base `be8c5a7c`. It validates 155 cases per flag set: the whitespace changes also select the ASCII, case-conversion, and comparison profiles because they share `members_string.go`. Both native full suites, accounting oracles, feature-dispatch checks, and all six benchmark samples passed. The AMD64 runner is an Intel Xeon 6973P-C; the ARM64 runner is an Apple M1 virtual machine.

| 64 KiB whitespace call | Intel SIMD before | Intel SIMD after | ARM SIMD before | ARM SIMD after |
|---|---:|---:|---:|---:|
| Long-padding trim | 26.71 µs | 10.84 µs | 42.23 µs | 9.94 µs |
| Split long fields | 132.77 µs | 95.03 µs | 193.0 µs | 113.5 µs |
| Split all whitespace | 30.55 µs | 9.01 µs | 45.92 µs | 9.77 µs |

Intel AVX2-disabled long trim is statistically unchanged at 26.68 → 26.89 µs. No whitespace workload shows a significant slowdown in this final native run. Large split allocation counts have the previously observed fractional/one-object variation; scanner helpers remain allocation-free and exact accounting snapshots pass.

The complete artifacts retain the slower controls too. Intel mixed-string length controls show 5–13% differences in the SIMD build. The noisy ARM VM shows 19.39% for 4 KiB ASCII swapcase and 20.65% for a 64 KiB invalid-tail comparison control. These operations retain their existing algorithms in this PR; the observed build/runner sensitivity is a reason to keep the full samples, not claim a universal speedup.

`pr-1283-native-evidence.tar.gz` preserves the complete final native profiles, environment, raw samples, and comparisons. Archive SHA-256: `380589f13025509f7848f3d173da97525237baa44b93b2e35bacaedf217b5e23`.

PR #1283 merged as `c31ea6888de0502612b07bcb759fa88b83f5e77d` after a clean review of the exact head, all 12 checks passing, and no unresolved review threads. Issues #1270–#1278 are closed, and the prior accounting PR #1268 is merged. The final master tree is exactly `d62a8737ee28d585eec29f3ff90e26d6680d527f`, matching the fully tested `64bb2d46` snapshot. `delivery-receipt.json` records all eleven implementation/prerequisite PRs and the nine issue closures. The initial receipt captured automatic post-merge CI reruns in progress; the post-merge verification below records their final results.

## Post-merge verification

All 12 post-merge checks on `c31ea6888de0502612b07bcb759fa88b83f5e77d` are successful, including [CI](https://github.com/xipkit/vibescript/actions/runs/34375639325/attempts/2), [native SIMD](https://github.com/xipkit/vibescript/actions/runs/34375639350), and [benchmarks](https://github.com/xipkit/vibescript/actions/runs/34375639185). The source tree still matches the fully validated snapshot. A fresh GitHub audit also confirmed that all eleven delivery PRs had a clean review of their exact final commit before merging, passing head checks, and no unresolved threads; all nine batch issues remain closed.

The first coverage attempt timed out in the unchanged `TestCLIContractRunExplicitZeroQuota` subprocess. Three local atomic-coverage repetitions passed, and the hosted rerun passed without source changes, reaching 83.9% coverage against a 75.0% minimum. The entire CLI package completed in 19.174 seconds on that rerun. The intermittent timeout and a proposal to reduce unnecessary fixture work are tracked separately in [#1292](https://github.com/xipkit/vibescript/issues/1292).

The refreshed `delivery-receipt.json` records the successful final checks. [Post-merge evidence](postmerge-evidence.tar.gz) includes both coverage attempts, the final CI job results, the independent review audit, the local test observations, and the receipt.

Archive SHA-256: `24a8af15d863d40714f0681446e120ba6166529b4c5340f87ff16e33211fecc5`.
