# Performance fix results

All eight fixes are merged into master. Issues #1247–#1254 are closed.

The September 7 audit produced eight actionable issues. Each fix has before/after measurements and regression coverage. Measurements below use Apple M4, Go 1.26.3; MB/KB are decimal and MiB/KiB are binary. Timings are medians of repeated runs, with baselines identified in the linked PRs and [measurement notes](README.md).

| Finding | Representative result | Delivery |
| --- | --- | --- |
| Dead call frames retain locals | Nested helpers: 32 MiB retained → under 40 KiB after return | [#1247](https://github.com/xipkit/vibescript/issues/1247), [PR 1259](https://github.com/xipkit/vibescript/pull/1259) |
| JSON tokens retain discarded documents | 32 one-byte results: 17.0 MB retained → no measurable document retention | [#1248](https://github.com/xipkit/vibescript/issues/1248), [PR 1256](https://github.com/xipkit/vibescript/pull/1256) |
| Repeated quota graph walks in uniq | 800 hash rows, scalar-key block: 284.6 → 1.79 ms | [#1249](https://github.com/xipkit/vibescript/issues/1249), [PR 1258](https://github.com/xipkit/vibescript/pull/1258) |
| Independent declaration checking is quadratic | 400 classes: 55.6 ms / 117.6 MB → 0.512 ms / 0.504 MB | [#1250](https://github.com/xipkit/vibescript/issues/1250), [PR 1264](https://github.com/xipkit/vibescript/pull/1264) |
| Scalar locals trigger mutable-alias scans | 800 locals: 14.2 ms / 16.5 MB → 0.94 ms / 1.11 MB | [#1251](https://github.com/xipkit/vibescript/issues/1251), [PR 1257](https://github.com/xipkit/vibescript/pull/1257) |
| Block scan buffers every match | 256 KiB first-match return: 28.3 ms / 37.0 MB → 3.04 µs / 4.94 KB | [#1252](https://github.com/xipkit/vibescript/issues/1252), [PR 1260](https://github.com/xipkit/vibescript/pull/1260) |
| Calls clone unused declarations | 1,000 unused classes: 645 µs / 929.8 KB → 1.07 µs / 3.47 KB | [#1253](https://github.com/xipkit/vibescript/issues/1253), [PR 1263](https://github.com/xipkit/vibescript/pull/1263) |
| Unrelated writes invalidate quota caches | Isolated 10,000-row check after unrelated environment write: 1.726 ms → 34.2 ns, no allocations | [#1254](https://github.com/xipkit/vibescript/issues/1254), [PR 1261](https://github.com/xipkit/vibescript/pull/1261) |

The streaming scan also improves full consumption: 16 KiB of literal matches takes 11.71 → 8.72 ms and allocates 2.285 → 0.546 MB. Tight-quota tests retain the baseline's accepted matching, no-match, erased-capture and destructuring cases.

Detaching JSON tokens increases the measured parse loop from 285 to 310 µs (about 9%) and allocation from 89.2 to 90.9 KB. This trades a small per-parse cost for releasing discarded input documents. Composite equality still uses pairwise comparisons. Regex patterns at Go's maximum nesting depth may use a quota-bounded table for continuation; first-match return still avoids it. Nanosecond cache timings measure isolated estimator work, not concurrent application throughput.

The original [audit report](../README.md) records screening data, excluded duplicates and the original findings. Raw final measurements are in this directory.

The combined implementation passed full tests, the estimator/recycle oracle, focused race tests, vet, lint, formatting and the unchanged benchmark gate. Short calls use 3,472 B against the 3,500 B limit. [Validation evidence](validation/README.md) records the tested commits and source tree; [delivery evidence](validation/delivery.json) records all eight completed PR gates.

Merged master [`d61e439f`](https://github.com/xipkit/vibescript/commit/d61e439ff89792a0210d0a56cd8af6668b337b92) exactly matches the final tested source tree, including upstream checker changes that landed during delivery.
