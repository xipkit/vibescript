# Startup signature parser

First-compilation peak heap falls 13.4%, latency falls 10.7–12.0%, and 3,740
allocations disappear on the measured baseline. Retained heap is unchanged.

Signature tokens borrow their source text until the owning table is built. Each
token stores one byte offset; diagnostic lines and byte columns are reconstructed
only on errors. The returned table remains owned and the process-wide builtin
table remains lazy. LF, CRLF, UTF-8 byte columns and synthetic EOF locations retain
their previous behavior. Optimization level, LTO and runtime sources are unchanged.

## Measurement

Measured baseline `2d42a247141eda7df76c6c9a04329cb6c996b5eb` against candidate
`5e1230fb368d728187cc2357c00ca857e1081dd4`. Both versions were built at the same
checkout pathname with matching compiler identities. Darwin uses arm64 Homebrew
Rust; Shannon uses x86_64 Arch Rust, pinned to core 2. Both hosts were reserved
through the shared gate queue and checked for interference.

The footprint workload registers four host functions and four capabilities,
compiles 100 distinct site programs (159,089 source bytes), and calls them 1,000
times. There are 64 alternating fresh-process pairs, with separate timing/RSS
and allocation builds. Time is the median of balanced two-round AB/BA means.
Allocations and requested bytes are cumulative; live/peak heap excludes allocator
overhead and native stacks. The core suite uses eight alternating rounds.

## darwin

| Timed work | Before | After | Change |
| --- | ---: | ---: | ---: |
| engine new | 2.792 µs | 3.239 µs | +16.04% |
| compiled 1 | 847.719 µs | 745.833 µs | -12.02% |
| compiled 10 | 2,157.104 µs | 2,188.271 µs | +1.44% |
| compiled 100 | 20,075.979 µs | 20,075.480 µs | -0.00% |
| calls 1000 | 85,254.645 µs | 85,629.208 µs | +0.44% |

| Checkpoint | RSS bytes, before → after | Live heap bytes | Peak heap bytes, before → after | Allocations, before → after | Requested bytes, before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| process start | 1,605,632 → 1,622,016 | 612 | 612 → 612 | 3 → 3 | 612 → 612 |
| engine new | 1,654,784 → 1,687,552 | 892 | 892 → 892 | 7 → 7 | 892 → 892 |
| registered | 2,588,672 → 2,572,288 | 16,296 | 16,392 → 16,392 | 209 → 209 | 23,584 → 23,584 |
| compiled 1 | 5,898,240 → 5,849,088 | 508,899 | 1,105,810 → 957,512 | 13,607 → 9,867 | 2,398,648 → 2,115,905 |
| compiled 10 | 7,061,504 → 7,077,888 | 693,118 | 1,105,810 → 957,512 | 35,106 → 31,366 | 6,779,023 → 6,496,280 |
| compiled 100 | 10,018,816 → 10,108,928 | 2,493,848 | 2,810,900 → 2,810,900 | 257,151 → 253,411 | 51,487,963 → 51,205,220 |
| calls 1000 | 11,255,808 → 11,583,488 | 2,496,723 | 2,810,900 → 2,810,900 | 1,477,100 → 1,473,360 | 147,457,650 → 147,174,907 |
| scripts dropped | 11,255,808 → 11,583,488 | 495,627 | 2,810,900 → 2,810,900 | 1,477,100 → 1,473,360 | 147,457,650 → 147,174,907 |

Live heap is identical before and after at every checkpoint. Stripped `vibes`:
6,299,296 → 6,315,824 bytes.

## shannon

| Timed work | Before | After | Change |
| --- | ---: | ---: | ---: |
| engine new | 11.835 µs | 11.378 µs | -3.86% |
| compiled 1 | 1,047.018 µs | 934.829 µs | -10.72% |
| compiled 10 | 2,786.590 µs | 2,797.756 µs | +0.40% |
| compiled 100 | 27,389.568 µs | 27,508.049 µs | +0.43% |
| calls 1000 | 118,952.971 µs | 121,284.068 µs | +1.96% |

| Checkpoint | RSS bytes, before → after | Live heap bytes | Peak heap bytes, before → after | Allocations, before → after | Requested bytes, before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| process start | 3,205,120 → 3,201,024 | 548 | 548 → 548 | 2 → 2 | 548 → 548 |
| engine new | 3,481,600 → 3,475,456 | 820 | 820 → 820 | 6 → 6 | 820 → 820 |
| registered | 5,328,896 → 5,339,136 | 16,224 | 16,312 → 16,312 | 208 → 208 | 23,480 → 23,480 |
| compiled 1 | 8,108,032 → 8,075,264 | 508,875 | 1,105,738 → 957,440 | 13,606 → 9,866 | 2,399,192 → 2,116,449 |
| compiled 10 | 8,497,152 → 8,493,056 | 693,166 | 1,105,738 → 957,440 | 35,105 → 31,365 | 6,784,863 → 6,502,120 |
| compiled 100 | 10,991,616 → 11,018,240 | 2,494,760 | 2,811,892 → 2,811,892 | 257,150 → 253,410 | 51,547,883 → 51,265,140 |
| calls 1000 | 11,923,456 → 11,937,792 | 2,497,635 | 2,811,892 → 2,811,892 | 1,476,099 → 1,472,359 | 147,445,570 → 147,162,827 |
| scripts dropped | 11,923,456 → 11,937,792 | 495,595 | 2,811,892 → 2,811,892 | 1,476,099 → 1,472,359 | 147,445,570 → 147,162,827 |

Live heap is identical before and after at every checkpoint. Stripped `vibes`:
7,375,888 → 7,376,704 bytes.


## Runtime outlier attribution

The core run measures 122 cases per flavor and validates 107,532 cases per flavor
on each host. There are no baseline counter differences, allocation regressions,
or RSS regressions. Every positive timing outlier above 3% is listed below.

| Host | Flavor | Case | Original delta | Single-executable control |
| --- | --- | --- | ---: | ---: |
| darwin | simd | `json_parse_ascii_64k/metered` | +3.11% | -1.28% |
| darwin | simd | `json_stringify_escaped_4k/metered` | +8.44% | +0.00% |
| darwin | simd | `json_stringify_escaped_4k/unlimited` | +8.36% | -0.02% |
| shannon | portable | `json_stringify_escaped_4k/metered` | +16.72% | -2.63% |
| shannon | portable | `json_stringify_escaped_4k/unlimited` | +17.28% | -3.07% |
| shannon | portable | `record_update/unlimited` | +3.45% | +0.94% |
| shannon | portable | `record_build_cap/unlimited` | +5.13% | -0.19% |
| shannon | simd | `loop_range/metered` | +3.25% | +0.62% |
| shannon | simd | `upstream_fibonacci/metered` | +4.23% | +0.65% |
| shannon | simd | `site_sieve_of_eratosthenes/unlimited` | +4.27% | -0.14% |

The control links both original and borrowed parsers into one executable per
flavor. Equal-length environment values select the parser before compilation.
Both modes use the identical executable hash and pathname. Each outlying runtime
case is recompiled 16 times per round over eight alternating rounds; a call counter
asserts zero signature parses during every timed loop. The service control uses
64 alternating fresh-process pairs. This retains the parser’s different temporary
allocation behavior while holding runtime instructions and their layout fixed.

Original-binary disassembly also checks the code the outliers execute. On Darwin,
`write_string`, metered `read_string`, the JSON scanner, both VM dispatch functions,
and `Script::call_with_keywords` have matching instructions after resolving
imports and ADRP/low12 relocations. Their
SIMD literals, escape and hexadecimal tables, and VM jump table match. On Shannon,
`write_string`, `Script::call_with_keywords`, `Run::advance`, and the simple VM
match after resolving GOT calls and RIP-relative addresses, in both build flavors.
The writer’s read-only constants match. These are relocation-normalized matches;
the original whole executables are not byte-identical.

Darwin’s constructor is a separate startup outlier (+16.04%, 2.792 → 3.239 µs).
It runs before signature parsing. Its 303-line normalized instruction stream and
successful-path constants match, and the control measures −0.46% (2.271 → 2.260 µs).

All original regressions above 3% disappear in the identical-executable controls.
The largest remaining increase among those cases is 0.94%. The controls preserve
first-compile gains of 12.62% on Darwin and 9.26% on Shannon, with the same 13.41%
peak-heap reduction. The 1,000-call controls change by +0.08% and −0.44%; their RSS
changes by 0.00% and −0.02%. No timing or RSS control has an increase above 3%.
Together with the original instruction comparisons, this classifies the original
outliers as layout-sensitive measurements outside the changed parser path under
the revised regression rule.

Control executable SHA-256 prefixes (both parser modes use each identical file):

| Host | Portable | SIMD | Service timing |
| --- | --- | --- | --- |
| darwin | `70e3a8e5e6bff55b` | `08383cadcf7baa1dd` | `52d5427c870137bf` |
| shannon | `04e736abc75806bb` | `42fb896cc9f94372` | `af1d7a15b4ba27d8` |

## Integration

The final branch was rebased onto `4b8c9aef8b`, which adds authoring diagnostics
and signature documentation. The tables above retain the exact measured baseline
and candidate; they do not attribute the later upstream documentation's storage
to this patch. The parser implementation and diagnostic-position tests are
unchanged by the rebase. Full local checks run again on the integrated tree.

## Verification and artifacts

Formatting, Clippy with all features and without default features, 2,012 workspace
tests, golden observations, portable/SIMD counter parity, and WASI pass. Signature
diagnostic tests cover line endings, UTF-8 byte columns and synthetic EOF positions.
The existing isolated allocation and retention guards pass. No Counter log entry
is needed: steps and tracked peak/retained bytes match the baseline exactly.

Raw results, controls, binaries, compiler identities, disassembly, and full local
logs remain under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/startup-parser/`.
`results/<host>/raw-results.tar.gz` preserves the remote run; `evidence/<host>/`
contains original-binary instruction comparisons. Only this summary is committed
as a measurement artifact.
