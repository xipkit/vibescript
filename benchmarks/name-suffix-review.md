# Callable-name review verification

PR #1319's three findings are fixed: `?=` repairs require an adjacent name;
callable publication shares method-spelling validation with the parser; and
aliases validate both names in bare and symbolic forms. Quoted-symbol fixes
replace the literal, preserving escaped source offsets. Generated setters and
exported module functions use the same validation. Host diagnostic labels
remain separate from published names; string data keys retain punctuation.

## Measurements

Before: `e06808245e24e8058b4d6d06e2356c0db3acb3d4`, the pre-review executable
code (the later pre-review head `a2cf8930` adds only tests and documentation).
Measured after: `a9baf423f`, before the README-only rebase and a documentation
clarification. The rebased code is identical. Eight rotating paired rounds of
`scripts/compare.py --rounds 8 --target-ms 200 --baseline <before> --suite
<parse|core> --out <dir>` cover eight parser and 172 runtime cases, with both
portable and SIMD builds. Hosts were reserved with the shared gate lock and
checked for competing gates. Allocation instrumentation uses separate builds.

Hosts: `darwin` (Apple M4, arm64 macOS) and `shannon` (Intel Core Ultra 9 285H,
x86_64 Linux). Original measurements use normal scheduler placement. Focused
x86 repeats pin core 2, disable ASLR, and rotate the same four binaries for
eight rounds. RSS repeats use fresh processes.

All allocation counts, requested bytes, steps, tracked peak bytes and retained
bytes are unchanged before/after across all cases on each architecture.
Portable and SIMD tracked counters agree. Arm64 timing increases peak at 2.85%;
x86 parser timing increases peak at 2.79%. The x86 text-code placement
exceptions and two repeated outliers are reported below.

Times are medians in microseconds; arrows show before → after. Allocation
counts/requested bytes and tracked peak/retained bytes without arrows are
unchanged in both builds. Parser tracked counters are zero. Tables show all
parser cases plus representative runtime and outlier cases; raw summaries
retain all 172 runtime cases and both accounting modes.

### arm64

| Case | SIMD µs | Δ | Portable µs | Δ | Allocations / requested B | Tracked peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| parse massive | 538.50 → 539.00 | +0.09% | 538.83 → 541.32 | +0.46% | 3,872 / 2,326,986 | 0 / 0 | 4.734 → 4.688 | 4.734 → 4.609 |
| parse bitwise_operations | 320.25 → 320.89 | +0.20% | 321.77 → 321.60 | -0.05% | 2,535 / 1,097,539 | 0 / 0 | 5.344 → 5.156 | 5.281 → 5.188 |
| parse wide_call | 23122.23 → 23041.45 | -0.35% | 23163.55 → 23228.43 | +0.28% | 200,106 / 79,781,772 | 0 / 0 | 437.312 → 437.156 | 437.328 → 437.125 |
| parse wide_shape | 3129.05 → 3110.19 | -0.60% | 3136.62 → 3132.37 | -0.14% | 28,241 / 14,170,081 | 0 / 0 | 10.016 → 9.812 | 9.969 → 9.859 |
| parse large_enum | 618.86 → 604.58 | -2.31% | 619.19 → 619.39 | +0.03% | 75 / 349,848 | 0 / 0 | 3.922 → 3.859 | 3.875 → 3.844 |
| parse large_literal | 239.44 → 236.81 | -1.10% | 241.59 → 240.49 | -0.46% | 157 / 124,585 | 0 / 0 | 4.969 → 4.844 | 4.938 → 4.844 |
| parse pathological | 5410.08 → 5390.19 | -0.37% | 5348.83 → 5407.72 | +1.10% | 777 / 19,168,951 | 0 / 0 | 11.078 → 10.906 | 11.094 → 11.016 |
| parse method_names | 3317.37 → 3304.98 | -0.37% | 3309.41 → 3300.92 | -0.26% | 25,570 / 12,748,240 | 0 / 0 | 10.094 → 9.828 | 9.938 → 9.906 |
| json_parse_escaped_4k | 8.10 → 8.10 | -0.02% | 8.15 → 8.14 | -0.12% | 39 / 19,802 | 23,319 / 8,857 | 6.141 → 6.141 | 6.125 → 5.984 |
| json_stringify_escaped_4k | 6.88 → 6.87 | -0.15% | 7.43 → 7.42 | -0.13% | 29 / 15,438 | 22,674 / 8,338 | 6.109 → 6.094 | 6.094 → 5.969 |
| record_update | 300.82 → 300.42 | -0.13% | 302.16 → 300.20 | -0.65% | 2,015 / 34,472 | 2,530 / 0 | 5.797 → 5.828 | 5.781 → 5.781 |
| glue_orders | 193.30 → 193.39 | +0.05% | 196.51 → 195.87 | -0.32% | 1,751 / 141,364 | 151,416 / 4,192 | 6.828 → 6.672 | 6.812 → 6.641 |
| glue_orders_cap | 236.23 → 237.17 | +0.40% | 239.36 → 238.49 | -0.36% | 2,055 / 168,822 | 156,242 / 4,192 | 7.031 → 7.016 | 7.031 → 6.969 |

### x86_64

| Case | SIMD µs | Δ | Portable µs | Δ | Allocations / requested B | Tracked peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| parse massive | 620.17 → 624.93 | +0.77% | 624.76 → 618.86 | -0.94% | 3,872 / 2,326,986 | 0 / 0 | 13.320 → 13.363 | 13.352 → 13.355 |
| parse bitwise_operations | 388.84 → 391.88 | +0.78% | 386.60 → 385.31 | -0.33% | 2,535 / 1,097,539 | 0 / 0 | 13.320 → 13.355 | 13.355 → 13.348 |
| parse wide_call | 27258.59 → 28020.16 | +2.79% | 27137.48 → 27501.87 | +1.34% | 200,106 / 79,781,772 | 0 / 0 | 42.285 → 42.574 | 44.484 → 44.645 |
| parse wide_shape | 3484.02 → 3513.15 | +0.84% | 3505.24 → 3491.97 | -0.38% | 28,241 / 14,170,081 | 0 / 0 | 13.625 → 13.355 | 13.363 → 13.324 |
| parse large_enum | 780.12 → 773.76 | -0.82% | 779.92 → 781.50 | +0.20% | 75 / 349,848 | 0 / 0 | 13.355 → 13.359 | 13.355 → 13.355 |
| parse large_literal | 203.72 → 203.93 | +0.10% | 205.84 → 203.75 | -1.01% | 157 / 124,585 | 0 / 0 | 13.355 → 13.359 | 13.246 → 13.332 |
| parse pathological | 5138.40 → 5228.70 | +1.76% | 5197.17 → 5096.39 | -1.94% | 777 / 19,168,943 | 0 / 0 | 13.242 → 14.824 | 13.367 → 13.371 |
| parse method_names | 3968.26 → 3986.98 | +0.47% | 3953.03 → 3965.04 | +0.30% | 25,570 / 12,748,240 | 0 / 0 | 13.359 → 13.348 | 13.355 → 13.355 |
| json_parse_escaped_4k | 7.56 → 8.09 | +6.95% | 7.79 → 8.06 | +3.46% | 39 / 19,810 | 23,319 / 8,857 | 13.480 → 13.477 | 13.484 → 13.477 |
| json_stringify_escaped_4k | 8.13 → 7.91 | -2.77% | 7.97 → 12.09 | +51.77% | 29 / 15,446 | 22,674 / 8,338 | 13.480 → 13.480 | 13.469 → 13.480 |
| record_update | 384.93 → 398.33 | +3.48% | 384.43 → 379.81 | -1.20% | 2,015 / 34,480 | 2,530 / 0 | 13.457 → 13.477 | 13.480 → 13.461 |
| glue_orders | 255.49 → 252.69 | -1.10% | 258.21 → 261.54 | +1.29% | 1,751 / 141,372 | 151,416 / 4,192 | 13.477 → 13.480 | 13.492 → 13.477 |
| glue_orders_cap | 316.04 → 313.14 | -0.92% | 318.17 → 318.51 | +0.11% | 2,054 / 168,766 | 156,234 / 4,192 | 13.480 → 13.477 | 13.480 → 13.484 |

## Repeated outliers and code placement

The first x86 SIMD `record_update/metered` result was +3.48%. Eight focused,
pinned rounds instead measure 403.33 → 402.60 µs (-0.18%); portable measures
401.94 → 401.55 µs (-0.10%). The initial single-process x86 pathological-parser
RSS sample rose 11.95%. Eight rotating fresh-process samples give 14.094 →
14.133 MiB (+0.28%) in both builds. Original samples are retained above.

The x86 escaped-JSON differences persist in the pinned repeat:

| Case | SIMD µs | Δ | Portable µs | Δ |
| --- | ---: | ---: | ---: | ---: |
| json_parse_escaped_4k/metered | 7.65 → 8.10 | +5.89% | 7.79 → 8.11 | +4.15% |
| json_parse_escaped_4k/unlimited | 7.58 → 8.07 | +6.44% | 7.77 → 8.09 | +4.15% |
| json_stringify_escaped_4k/metered | 8.24 → 8.13 | -1.32% | 8.08 → 12.32 | +52.52% |
| json_stringify_escaped_4k/unlimited | 8.30 → 8.18 | -1.47% | 7.92 → 12.28 | +55.04% |

These are the task's code-placement exception, supported by disassembly rather
than omitted from the result. The JSON writer, parser string readers/decoders,
and scan helpers have unchanged sources and instruction sequences in both
build flavors. The audit compares 20 functions in each flavor, resolving
relocated addresses and LLVM private-symbol identifiers; every sequence
matches. In portable, the 963-instruction `json::writer::write_string` moves
from `0x5bc090` to `0x5beb00`, retaining its 4,179-byte body. Callable-name
validation does not execute in these text loops. Allocations, requested bytes,
tracked counters and RSS remain unchanged or within 3%. Disassembly, executable
copies and normalized instruction hashes are retained under `assembly/`.

## Observations and verification

- All 2,110 workspace tests, formatting and both Clippy configurations pass.
- All eight golden corpora pass. The one newly rejected parse mutation forms
  `coach?=`; its V0003 observation is recorded in `tests/golden/README.md`.
- The 37,227-case parse sweep has exactly that one expected first-error change,
  with no other acceptance, diagnostic, crash, hang or invalid-span changes.
  Its unfiltered exit is 1 because it reports the intentional difference.
- A paired audit of 197,906 runtime cases finds no stable observation or counter
  changes. Four pre-existing time/UUID-dependent cases are excluded, as they
  are by the golden checker. No new Counter log entries or counter recordings.
- Portable/SIMD validation passes all 107,682 shared cases with identical
  counters; WASI tests, CLI and filesystem witnesses pass.
- The final distributed gate is recorded separately in `gate-all.log`.

Raw evidence stays outside Git under
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/name-suffix/review/`: local check
logs, `counter-audit.json`, `sweep/`, per-host `parse-summary.json` and
`core-summary.json`, `shannon/repeat-summary.json`, and `assembly/`. Full
per-round remote outputs are under each host clone's
`.cache/name-suffix/review/{parse,core,repeat}/` and copied into local per-host
`raw/` directories. No raw output or executable is committed.
