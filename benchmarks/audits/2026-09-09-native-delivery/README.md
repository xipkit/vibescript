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
