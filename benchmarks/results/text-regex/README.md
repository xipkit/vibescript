# Glue text and regex performance

No runtime optimization met the required regression limit. The delivered branch adds benchmarks and measurement evidence; its entire `src` tree is identical to parent `a23d4ece`. The rejected runtime patch is preserved outside Git for follow-up. The speedups below are **experimental results, not improvements shipped by this branch**.

## Coverage and method

The 33 new workloads run in both metered and unlimited modes. They cover email and ID validation; captures; anchored and unanchored hits and misses; literal and dynamic patterns; string and block `sub`/`gsub`; `scan`; string splitting and joining; stripping; case conversion; prefix, suffix and substring searches; formatting; mixed interpolation; concatenation loops; and large Unicode lengths. Short, Unicode, overlapping and reverse searches cover the fallbacks. Expected results are specified independently of the implementation.

Regex separators are unsupported: `string.split` expects `string?`, and a regex separator produces V0101. No language extension is included.

The latest experiment was measured against `3161370a` on Vinci (Apple M4 arm64) and Shannon (x86_64 Linux). Eight paired rounds used a 150 ms target per case. There are 752 comparisons: 66 text and 122 core variants, two builds and two hosts. The core suite checks unrelated VM and JSON workloads. Runs wait for other gates and compilers and monitor interference. Linux uses core 2 affinity and per-process ASLR disabled. Both versions are built in the same checkout path. A fixed executable entry path keeps argv comparable; environment records include affinity, revision and binary hashes.

## Rejected optimization

The strongest candidate batches long ASCII `index`/`rindex` mismatches with byte KMP and an out-of-line SSE2 scanner. It preserves scratch allocations and every accounting checkpoint. Short, Unicode and invalid-byte subjects retain the original rune loop. Its final form enables the fast path only on x86 SIMD builds.

All 376 x86 comparisons pass the timing, allocation, accounting and RSS limits. The ARM matrix has 13 raw metric failures. Repeated RSS measurements confirm four concatenation increases of roughly 5%, even though the ARM SIMD executable's code section is byte-for-byte identical to the baseline. Sixteen samples per version at the same physical executable pathname did not remove those increases:

| Vinci workload | Baseline RSS, bytes | Candidate RSS, bytes | Change |
| --- | ---: | ---: | ---: |
| text/concat_loop, metered | 6,062,080 | 6,406,144 | +5.68% |
| text/concat_loop, unlimited | 6,078,464 | 6,389,760 | +5.12% |
| string_concat, metered | 6,176,768 | 6,488,064 | +5.04% |
| string_concat, unlimited | 6,193,152 | 6,496,256 | +4.89% |

The independent baseline-self controls stay within 0.28% for these RSS checks. This violates the requested 3% limit, so the optimization is excluded. Local memory-map and launch-path diagnostics did not establish a safe fix. Allocation counts/bytes and tracked steps/peak/retained bytes remain exactly equal in all 752 candidate comparisons.

The following raw eight-round tables describe that **rejected x86-only candidate**. The complete matrix, including every failing row, is retained in `x86-results/results.csv` under the artifact directory below.

## arm64 results (Vinci, SIMD, metered)

| Workload | Time, µs | Allocations | Allocated bytes | Tracked peak bytes | Retained bytes | Peak RSS, KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| regex_email | 200.545 → 203.925 | 1684 → 1684 | 462280 → 462280 | 19322 → 19322 | 2144 → 2144 | 6240 → 6368 |
| regex_anchored_miss | 386.157 → 385.990 | 25 → 25 | 5660 → 5660 | 20261 → 20261 | 0 → 0 | 6096 → 6176 |
| regex_unanchored_miss | 18.654 → 17.047 | 21 → 21 | 3408 → 3408 | 18977 → 18977 | 0 → 0 | 6128 → 6112 |
| regex_literal_miss | 17.651 → 17.507 | 23 → 23 | 7368 → 7368 | 21986 → 21986 | 0 → 0 | 6112 → 6208 |
| include | 6.565 → 6.588 | 12 → 12 | 1088 → 1088 | 9333 → 9333 | 0 → 0 | 5824 → 5856 |
| index | 19.019 → 19.050 | 15 → 15 | 2020 → 2020 | 10265 → 10265 | 0 → 0 | 6048 → 6064 |
| index_short_hit | 0.693 → 0.698 | 15 → 15 | 1972 → 1972 | 2022 → 2022 | 0 → 0 | 5968 → 6016 |
| index_unicode | 35.853 → 35.837 | 15 → 15 | 2020 → 2020 | 20497 → 20497 | 0 → 0 | 6064 → 6064 |
| index_overlap | 19.178 → 19.210 | 15 → 15 | 2032 → 2032 | 10265 → 10265 | 0 → 0 | 6032 → 6080 |
| rindex | 37.363 → 37.384 | 15 → 15 | 2020 → 2020 | 18462 → 18462 | 0 → 0 | 6096 → 6080 |
| join | 4.012 → 4.040 | 87 → 87 | 8073 → 8073 | 10086 → 10086 | 992 → 992 | 5920 → 5968 |
| strip | 0.651 → 0.656 | 12 → 12 | 9247 → 9247 | 17435 → 17435 | 8287 → 8287 | 6112 → 6208 |
| length_unicode | 11.070 → 11.055 | 9 → 9 | 960 → 960 | 74680 → 74680 | 0 → 0 | 5936 → 5952 |
| length_mixed | 50.785 → 50.951 | 9 → 9 | 960 → 960 | 54200 → 54200 | 0 → 0 | 5936 → 5920 |

## x86_64 results (Shannon, SIMD, metered)

| Workload | Time, µs | Allocations | Allocated bytes | Tracked peak bytes | Retained bytes | Peak RSS, KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| regex_email | 254.588 → 253.266 | 1684 → 1684 | 462280 → 462280 | 19322 → 19322 | 2144 → 2144 | 13788 → 13788 |
| regex_anchored_miss | 527.889 → 529.713 | 25 → 25 | 5660 → 5660 | 20261 → 20261 | 0 → 0 | 13788 → 13788 |
| regex_unanchored_miss | 23.942 → 22.894 | 21 → 21 | 3408 → 3408 | 18977 → 18977 | 0 → 0 | 13788 → 13788 |
| regex_literal_miss | 24.784 → 23.890 | 23 → 23 | 7368 → 7368 | 21986 → 21986 | 0 → 0 | 13788 → 13788 |
| include | 5.168 → 5.165 | 12 → 12 | 1088 → 1088 | 9333 → 9333 | 0 → 0 | 13788 → 13788 |
| index | 15.769 → 1.232 | 15 → 15 | 2020 → 2020 | 10265 → 10265 | 0 → 0 | 13788 → 13788 |
| index_short_hit | 0.983 → 0.984 | 15 → 15 | 1972 → 1972 | 2022 → 2022 | 0 → 0 | 13788 → 13788 |
| index_unicode | 26.668 → 26.816 | 15 → 15 | 2020 → 2020 | 20497 → 20497 | 0 → 0 | 13788 → 13788 |
| index_overlap | 21.655 → 19.855 | 15 → 15 | 2032 → 2032 | 10265 → 10265 | 0 → 0 | 13788 → 13788 |
| rindex | 30.475 → 1.466 | 15 → 15 | 2020 → 2020 | 18462 → 18462 | 0 → 0 | 13788 → 13788 |
| join | 9.131 → 9.121 | 87 → 87 | 8073 → 8073 | 10086 → 10086 | 992 → 992 | 13788 → 13788 |
| strip | 0.857 → 0.871 | 12 → 12 | 9247 → 9247 | 17435 → 17435 | 8287 → 8287 | 13788 → 13788 |
| length_unicode | 10.819 → 10.802 | 9 → 9 | 960 → 960 | 74680 → 74680 | 0 → 0 | 13788 → 13788 |
| length_mixed | 45.367 → 45.346 | 9 → 9 | 960 → 960 | 54200 → 54200 | 0 → 0 | 13788 → 13788 |

## Profiles and other experiments

Profiling preceded implementation. The local Mac Samply profile contains 36,551 samples: regex search accounts for 27.18% inclusively, literal scanning for 4.88% of leaf samples, substring search for 2.57%, Unicode-span scanning for 4.21%, and allocator growth for 7.79% inclusively. These overlapping categories are not additive. Vinci supplies the arm64 timings; the local profile is not presented as a Vinci benchmark.

Shannon's baseline profile contains 28,062 samples: regex search is 20.14% inclusive, literal scanning 2.00% at the leaf, substring search 2.97%, rune decoding 6.00%, and repeated regex compilation 6.64% inclusive. Direct Samply recording was blocked by the host's perf policy. A user CPU-clock perf recording was imported into Samply without changing host settings; symbols were resolved against the exact recorded ELF binaries.

Regex-prefix scans, direct ASCII decoding, trimming, matcher-buffer pushes and join changes were also investigated. Direct decoding slowed portable literal matching. Eager join preallocation changed quota-error precedence; the corrected version preserved accounting but regressed x86 JSON controls. Whole-search outlining and standard-library substring search did not pass the full regression matrix. Earlier successes on f7d8079 were not carried forward as evidence after rebasing onto the VM changes in 3161370a.

At the profiled baseline, regex literals retain constant source text, but `Op::Regex` compiles it when evaluated. Caching compilation crosses VM/compiler ownership and changes accounting, so it needs separate coordination. Capture-free storage and alternate automata remain deferred.

## Accounting and verification

No Counter log entry or golden counter update is needed: the delivered runtime is unchanged. The rejected candidate's paired audit covered 197,856 engine cases with zero counter changes and no deterministic observation changes. Four replay observations contain clocks or UUIDs. All its local checks and both hosts' explicit portable/SIMD validation passed; it was rejected for performance.

The final branch is checked again with formatting, both Clippy configurations, workspace tests, all goldens, portable/SIMD validation and WASI. Exact commands and exit codes are recorded in `final/local-verification.json`; the final three-host gate result is recorded in `gate.log` for the delivery head. These external artifacts and the delivery report carry the final verification status.

## Artifacts and next steps

All large local artifacts are under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/mgomes/text-regex/`. The final deliverable does not depend on files in the coordinator's scratchpad.

- `historical-f7-report/profiles/`: Samply profiles, symbol evidence and initial profile results.
- `x86-results/`: complete latest candidate matrices, environments, raw regression list and repeated controls.
- `x86/`: exact candidate source identity, local checks, counter audit and code-section comparisons.
- `rejected-runtime.patch`: the unshipped candidate, applicable to parent 3161370a.
- `final/`: verification of the restored, unchanged runtime; `gate.log`: final branch gate.
- Earlier rejected experiments remain in `experiments/`, `scalar-results/`, `isolated-results/` and `final-results/`.

Further optimization should first explain the reproducible ARM RSS increase across these binary builds, or pursue regex compilation caching with the VM owner. The preserved x86 patch gives a measured starting point, but must not be merged as an accepted cross-architecture optimization without resolving the regression.
