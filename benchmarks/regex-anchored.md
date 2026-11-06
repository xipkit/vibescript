# Text-anchored regex search

`Search::find` used to start a thread at every position even when every
alternative required `\A` or non-multiline `^`. An anchored miss now tries
position zero and stops when its threads finish. Starting beyond zero returns
no match immediately.

The prefix proof follows saves, splits and other assertions until it reaches
`BeginText`, a consuming instruction, or acceptance. Every branch must reach
`BeginText` first; a branch that can consume or accept without it prevents the
optimization. Visited marks bound nullable cycles, and dead branches are safe.
Existing threads still run to completion, preserving match priority, greediness
and captures. Multiline `^` and mixed anchored/unanchored alternatives retain
the normal search.

## Measurement

OTIS, Apple M4, macOS arm64, Rust 1.98.1. Baseline `3ec2f9a6` and the candidate
use the release profile with default SIMD features. Eight alternating paired
rounds target 50 ms per case after warmup (at most 50,000 iterations), using the
same executable entry path.
The 34 cases include the existing text/regex fixtures and anchored-offset,
all-anchored-alternative and mixed-alternative misses, in metered and unlimited
modes. Both binaries satisfy every independent fixture expectation; timed
result digests must agree. Timings below are medians in microseconds.

| Workload | Metered before → after, µs | Unlimited before → after, µs |
| --- | ---: | ---: |
| `regex_email` | 115.313 → 90.261 | 114.374 → 88.565 |
| `regex_id` | 72.777 → 55.301 | 72.521 → 55.293 |
| `regex_captures` | 4.104 → 4.126 | 4.030 → 4.060 |
| `regex_anchored_hit` | 1.392 → 1.197 | 1.386 → 1.208 |
| `regex_anchored_miss` | 399.295 → 0.789 | 399.469 → 0.787 |
| `regex_unanchored_hit` | 1.477 → 1.488 | 1.487 → 1.490 |
| `regex_unanchored_miss` | 6.839 → 6.837 | 6.829 → 6.845 |
| `regex_literal_miss` | 6.910 → 6.911 | 6.889 → 6.917 |
| `regex_dynamic` | 1.611 → 1.642 | 1.603 → 1.635 |
| `regex_sub_string` | 1.863 → 1.928 | 1.871 → 1.929 |
| `regex_sub_block` | 1.833 → 1.869 | 1.841 → 1.854 |
| `regex_gsub_string` | 54.195 → 54.072 | 54.285 → 54.167 |
| `regex_gsub_block` | 45.401 → 45.059 | 45.260 → 44.988 |
| `regex_scan` | 54.558 → 54.563 | 54.544 → 54.385 |
| `regex_anchored_offset_miss` | 396.979 → 0.686 | 397.113 → 0.700 |
| `regex_anchored_alternatives_miss` | 524.086 → 0.846 | 521.722 → 0.867 |
| `regex_mixed_alternatives_miss` | 713.147 → 708.376 | 712.129 → 707.954 |

The anchored miss improves about 506 times. Neighboring unanchored workloads
range from a 0.8% reduction to a 3.5% increase in these shared-host measurements.

## Counters and correctness

The initial prefix instruction reuses its existing literal-prefix charge.
Additional proof traversal charges work and accounts for temporary branch
storage; skipped search positions stop charging. The 16 KiB anchored miss drops
from 32,789 to 21 steps, the nonzero-offset miss from 32,787 to 19, and the
all-anchored-alternative miss from 65,562 to 28. The mixed alternative pays two
extra proof steps; the unanchored and literal misses keep their existing counts.
Targeted `--cases` recording
updates these existing counter records:

| Corpus | Changed records |
| --- | ---: |
| conformance | 4 |
| language | 7,259 |
| compatibility | 2 |
| replay | 1,761 |

Of 9,026 changed records, 1,259 decrease steps, 7,766 increase steps and 397
increase peak bytes. Retained bytes never change. Both modes of `regex_loop`
drop from 12,057 to 9,241 steps; both modes of `regex_record_fields` drop from
41,915 to 35,167. The [Counter log](../tests/golden/README.md#counter-log) records
the accounting decision.

A paired same-checkout audit covers 197,917 engine cases. Only the four known
clock-dependent replay observations vary; every stored observation remains
byte-for-byte unchanged. Existing quota drift is present in the baseline too.
Differential tests compare captures and all byte offsets with the unoptimized
thread machine, including Unicode, invalid UTF-8, empty matches, nullable loops,
alternative order and multiline mode. Separate tests check constant-work misses,
step limits and cancellation. A real CLI run verifies the 16 KiB miss, a hit,
nonzero offset, captures, scan, substitution and multiline matching together
under a 1,000-step quota.
