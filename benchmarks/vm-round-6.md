# VM round 6 measurements

Large addressed operations and rare cleanup leave the hot dispatch loop. A small selector calls independent update helpers; new outlined bodies do not share their native stack frame. Conversion back to general opcodes also stays out of line, so it cannot be inlined into the general dispatcher. Numeric type proofs skip dynamic overload checks, while arithmetic shares the existing implementation. Scalar three-way comparisons run inline with the same total order and charges. `Op` remains 16 bytes; no unsafe code or architecture-specific runtime was added.

**The strict ±2% M4 opcode-layout target is not met.** The dispatchers and update bodies retain identical instructions and stack reservations when an unused opcode is added, but portable metered array construction moves +5.62%; removing it reverses the effect by −5.61%. The smaller dispatch and typed arithmetic are retained because the delivered candidate passes the affected-workload 3% timing requirement on both architectures. Unchanged-kernel placement outliers are disclosed below with disassembly and controls. This is not a claim of complete layout independence.

## Method

The runtime baseline is fef329a395807d4958bdc94540c004a75cf8d90c. Two fixture commits add float comparisons, float total ordering and numeric-union controls, recorded against that baseline. The measured runtime snapshot is `e3dc09aa0e5aba2adb6227ab92bf582c3e138754`; delivery runtime commit `e24594d9` has identical source, fixtures and tests. Delivery commits separate dispatch, local-address setup, numeric proofs, independent outlined bodies and documentation.

Each host runs eight paired core and text rounds through one fixed executable path, rotating baseline/candidate portable and SIMD builds, targeting 150 ms per case. Timing and allocation instrumentation use separate binaries. RSS comes from fresh processes and includes initialization. Every full-sweep time/RSS increase over 3% receives eight 500 ms timing rounds or sixteen paired RSS samples before attribution.

- arm64: vinci, Apple M4, native offline release builds.
- x86_64: shannon, Intel Core Ultra 9 285H, pinned to performance core 2 with ASLR disabled per process.
- The shared gate lock and host gate markers reserve both hosts. All raw data lives under .cache/vm-round-6/. Official runs use {vinci,shannon}-opaque-{core,text}/; shorter diagnostics are kept separate.

## Dispatch and layout control

The following are exact measured binary symbol extents and stack prologues, identical between portable and SIMD builds. Local reservation excludes saved registers: add 112 bytes on arm64 and 48 bytes on x86_64. Both sides use Rust 1.98.1 / LLVM 22.1.8; these freshly rebuilt baseline sizes need not equal round 5's earlier disassembly.

| Host | Dispatcher | Code bytes, before → after | Local stack bytes, before → after |
|---|---|---:|---:|
| vinci | `simple::run` | 15,848 → 13,412 | 1,008 → 768 |
| shannon | `simple::run` | 20,096 → 16,992 | 968 → 744 |
| vinci | `Run::advance` | 94,000 → 92,632 | 10,320 → 10,304 |
| shannon | `Run::advance` | 115,120 → 114,928 | 10,072 → 10,072 |

M4 disassembly of the addition and removal controls finds identical normalized instructions in both dispatchers and all four outlined bodies, in both feature builds. The selector grows from 64 to 68 instructions when the probe is present. The opcode conversion is separately compiled. The probe therefore does not reshape either interpreter's match or grow its stack reservation. On x86 the two dispatchers also retain identical instructions in the additional kernel-placement control. Raw disassemblies, prologues and comparisons are in `work/{vinci,shannon}-opaque-asm/` and `work/opaque-x86-probe-audit.json`.

The native control is `2b6b858f` (addition) followed by `89feb3f9` (removal, the exact original source tree), against `e3dc09aa`. Each direction measures eight paired 500 ms rounds of 18 workloads, both metering modes and both feature builds: 72 comparisons. In the first pass, 71 of 72 stay within ±2% in each direction. The portable metered array-construction exception is repeated once with eight 1-second rounds, including both modes and builds:

| Array construction | Initial addition | Initial removal | Longer addition, µs | Longer removal, µs |
|---|---:|---:|---:|---:|
| portable metered | +3.65% | -2.46% | 107.850 → 113.910 (+5.62%) | 114.318 → 107.910 (-5.61%) |
| portable unlimited | +1.53% | -0.78% | 104.021 → 106.075 (+1.97%) | 104.969 → 103.808 (-1.11%) |
| simd metered | +0.39% | -0.70% | 111.487 → 108.380 (-2.79%) | 108.155 → 110.522 (+2.19%) |
| simd unlimited | +0.76% | -0.99% | 104.049 → 104.390 (+0.33%) | 104.826 → 106.435 (+1.53%) |

The exception persists and changes sign when the opcode is removed. It cannot be dismissed as a passing 2% control. In this control, the structural separation preserves interpreter instructions; it does not eliminate native placement sensitivity in the allocation-heavy array case. This remains follow-up work before accepting the stricter layout goal. The two initial controls contain 2,304 unchanged result/counter observations. Raw data is in `vinci-opaque_{probe,removed}/` and `vinci-opaque_{probe,removed}_confirm/`.

The cold-opcode control adds `Extended::LayoutProbe` and a cold, non-inlined handler with 512 bytes of scratch. The compiler can emit it only after `Finish` in a specially named synthetic function, absent from the benchmark sources. Benchmark bytecode and executed operations remain unchanged. Removing the control restores the original source tree. Earlier probes that appended unreachable bytecode to every function changed compile-time allocations and were rejected as confounded controls.

## Before and after

Eight paired rounds. All 952 portable/SIMD case comparisons across both hosts preserve steps, allocation counts, allocated bytes, tracked peak and retained bytes. A single allocation/memory value below denotes both before and after. RSS is separately measured process high-water memory. The second table for each host gives times in microseconds for every numeric-control build/metering combination.

### vinci

| Metered SIMD workload | Time µs, before → after | Change | Allocations | Allocated bytes | Tracked peak / retained bytes | RSS MiB, before → after |
|---|---:|---:|---:|---:|---:|---:|
| `numeric_loop` | 36.948 → 35.605 | -3.63% | 9 | 1512 | 1464 / 0 | 5.609 → 5.453 |
| `loop_numeric_union` | 46.712 → 46.153 | -1.20% | 9 | 1512 | 1464 / 0 | 5.594 → 5.500 |
| `loop_float` | 44.970 → 43.734 | -2.75% | 9 | 1512 | 1464 / 0 | 5.594 → 5.484 |
| `loop_float_compare` | 105.380 → 95.923 | -8.97% | 9 | 1528 | 1480 / 0 | 5.609 → 5.562 |
| `loop_float_order` | 191.918 → 139.454 | -27.34% | 9 | 1544 | 1496 / 0 | 5.734 → 5.641 |
| `loop_range` | 42.473 → 41.889 | -1.37% | 10 | 1584 | 1536 / 0 | 5.641 → 5.594 |
| `loop_branches` | 76.120 → 70.694 | -7.13% | 9 | 1512 | 1464 / 0 | 5.609 → 5.562 |
| `array_each` | 100.231 → 100.646 | +0.41% | 14 | 18712 | 18152 / 0 | 5.969 → 5.938 |
| `range_each` | 91.438 → 91.548 | +0.12% | 12 | 2176 | 2128 / 0 | 5.828 → 5.859 |
| `loop_array_build` | 105.120 → 104.988 | -0.13% | 678 | 3556096 | 23568 / 10752 | 6.156 → 5.938 |
| `array_growth` | 12.460 → 12.700 | +1.93% | 19 | 6328 | 5272 / 0 | 5.938 → 5.750 |

| Workload | Portable metered | Portable unlimited | SIMD metered | SIMD unlimited |
|---|---:|---:|---:|---:|
| `numeric_loop` | 36.632 → 35.373 (-3.44%) | 35.551 → 34.145 (-3.96%) | 36.948 → 35.605 (-3.63%) | 35.708 → 34.550 (-3.24%) |
| `loop_numeric_union` | 46.691 → 46.179 (-1.10%) | 45.355 → 45.112 (-0.54%) | 46.712 → 46.153 (-1.20%) | 45.331 → 44.839 (-1.08%) |
| `loop_float` | 45.028 → 43.688 (-2.97%) | 43.681 → 42.824 (-1.96%) | 44.970 → 43.734 (-2.75%) | 43.545 → 42.898 (-1.49%) |
| `loop_float_compare` | 105.543 → 96.284 (-8.77%) | 102.549 → 92.211 (-10.08%) | 105.380 → 95.923 (-8.97%) | 102.601 → 90.914 (-11.39%) |
| `loop_float_order` | 191.101 → 139.838 (-26.83%) | 189.766 → 137.640 (-27.47%) | 191.918 → 139.454 (-27.34%) | 190.418 → 138.017 (-27.52%) |
| `loop_range` | 42.962 → 42.214 (-1.74%) | 42.941 → 42.038 (-2.10%) | 42.473 → 41.889 (-1.37%) | 42.597 → 41.977 (-1.45%) |
| `loop_branches` | 76.217 → 70.580 (-7.40%) | 68.882 → 68.625 (-0.37%) | 76.120 → 70.694 (-7.13%) | 68.896 → 68.436 (-0.67%) |

### shannon

| Metered SIMD workload | Time µs, before → after | Change | Allocations | Allocated bytes | Tracked peak / retained bytes | RSS MiB, before → after |
|---|---:|---:|---:|---:|---:|---:|
| `numeric_loop` | 58.254 → 55.218 | -5.21% | 9 | 1520 | 1464 / 0 | 13.473 → 13.473 |
| `loop_numeric_union` | 78.395 → 70.187 | -10.47% | 9 | 1520 | 1464 / 0 | 13.473 → 13.473 |
| `loop_float` | 76.546 → 69.199 | -9.60% | 9 | 1520 | 1464 / 0 | 13.473 → 13.473 |
| `loop_float_compare` | 141.317 → 122.525 | -13.30% | 9 | 1536 | 1480 / 0 | 13.473 → 13.473 |
| `loop_float_order` | 232.958 → 191.503 | -17.80% | 9 | 1552 | 1496 / 0 | 13.473 → 13.473 |
| `loop_range` | 72.730 → 69.418 | -4.55% | 10 | 1592 | 1536 / 0 | 13.473 → 13.473 |
| `loop_branches` | 110.335 → 93.978 | -14.82% | 9 | 1520 | 1464 / 0 | 13.473 → 13.473 |
| `array_each` | 140.006 → 138.826 | -0.84% | 14 | 18720 | 18152 / 0 | 13.473 → 13.473 |
| `range_each` | 123.448 → 123.101 | -0.28% | 12 | 2184 | 2128 / 0 | 13.473 → 13.473 |
| `loop_array_build` | 152.977 → 145.569 | -4.84% | 678 | 3556104 | 23568 / 10752 | 13.473 → 13.473 |
| `array_growth` | 16.686 → 16.484 | -1.21% | 19 | 6336 | 5272 / 0 | 13.473 → 13.473 |

| Workload | Portable metered | Portable unlimited | SIMD metered | SIMD unlimited |
|---|---:|---:|---:|---:|
| `numeric_loop` | 58.430 → 55.526 (-4.97%) | 58.117 → 55.236 (-4.96%) | 58.254 → 55.218 (-5.21%) | 58.060 → 55.162 (-4.99%) |
| `loop_numeric_union` | 78.457 → 70.526 (-10.11%) | 77.981 → 70.022 (-10.21%) | 78.395 → 70.187 (-10.47%) | 78.012 → 69.939 (-10.35%) |
| `loop_float` | 76.199 → 69.220 (-9.16%) | 75.993 → 69.246 (-8.88%) | 76.546 → 69.199 (-9.60%) | 76.299 → 69.294 (-9.18%) |
| `loop_float_compare` | 140.015 → 122.822 (-12.28%) | 139.702 → 122.130 (-12.58%) | 141.317 → 122.525 (-13.30%) | 139.643 → 121.505 (-12.99%) |
| `loop_float_order` | 232.644 → 192.081 (-17.44%) | 231.710 → 190.437 (-17.81%) | 232.958 → 191.503 (-17.80%) | 231.841 → 189.694 (-18.18%) |
| `loop_range` | 72.744 → 69.543 (-4.40%) | 72.538 → 69.076 (-4.77%) | 72.730 → 69.418 (-4.55%) | 72.385 → 69.122 (-4.51%) |
| `loop_branches` | 110.221 → 94.296 (-14.45%) | 110.137 → 94.035 (-14.62%) | 110.335 → 93.978 (-14.82%) | 109.970 → 93.846 (-14.66%) |


## Regression audit

Every full-sweep increase above 3% receives a longer confirmation. All core VM timing cases pass. The persistent timing exceptions are portable M4 regex search and x86 escaped JSON writing/SIMD regex search. All 39 inspected JSON, regex, text and budget helpers have identical normalized machine instructions in each build on each architecture; their source is unchanged. Native relocation operands are resolved before comparison. The sources audit is `work/opaque-unchanged-kernel-sources.json`.

| Host | Case | Build | Metric | Full sweep | Confirmation |
|---|---|---|---|---:|---:|
| vinci | `text/regex_unanchored_miss/metered` | portable | Time | +12.06% | +10.45% |
| vinci | `glue_orders_cap/unlimited` | portable | RSS | +3.67% | +2.29% |
| vinci | `loop_string_build/metered` | portable | RSS | +5.46% | +0.00% |
| vinci | `loop_string_build/unlimited` | portable | RSS | +4.96% | +0.00% |
| vinci | `string_concat/unlimited` | portable | RSS | +4.21% | +3.12% |
| vinci | `text/concat_loop/unlimited` | portable | RSS | +4.58% | +1.84% |
| vinci | `text/upcase/metered` | simd | Time | +3.22% | +1.93% |
| shannon | `json_stringify_escaped_4k/metered` | portable | Time | +23.75% | +18.86% |
| shannon | `json_stringify_escaped_4k/unlimited` | portable | Time | +24.50% | +21.95% |
| shannon | `json_stringify_escaped_4k/metered` | simd | Time | +9.95% | +6.84% |
| shannon | `json_stringify_escaped_4k/unlimited` | simd | Time | +8.08% | +8.07% |
| shannon | `text/regex_unanchored_miss/metered` | simd | Time | +11.33% | +12.12% |
| shannon | `text/regex_unanchored_miss/unlimited` | simd | Time | +11.28% | +10.79% |

The unused-opcode control supplies a separate placement check for these kernel cases. The benchmark programs and executed dispatcher/handler instructions remain unchanged. All x86 search and writer bodies inspected in this control are also identical (30 functions including both dispatchers, across both builds). Eight paired 500 ms rounds give:

| Host | Case | Build | Add unused opcode | Remove it |
|---|---|---|---:|---:|
| vinci | `text/regex_unanchored_miss/metered` | portable | +0.21% | +6.14% |
| shannon | `json_stringify_escaped_4k/metered` | portable | -17.02% | +18.34% |
| shannon | `json_stringify_escaped_4k/unlimited` | portable | -15.10% | +18.91% |
| shannon | `json_stringify_escaped_4k/metered` | simd | -8.66% | +9.75% |
| shannon | `json_stringify_escaped_4k/unlimited` | simd | -7.01% | +6.60% |
| shannon | `text/regex_unanchored_miss/metered` | simd | -9.66% | +9.85% |
| shannon | `text/regex_unanchored_miss/unlimited` | simd | -8.77% | +9.79% |

The x86 shifts reverse with the probe. M4 regex samples are less reciprocal: across modes/builds, addition ranges from −8.56% to +7.63%, and removal from −2.00% to +7.14%. These controls and unchanged-source/instruction evidence support the permitted placement exception; the original regressions remain in the table. Full control samples and binary hashes are in `{vinci,shannon}-opaque_kernels_{probe,removed}/`.

The M4 portable concat RSS reading remains +3.12% in the first confirmation. A further sixteen-round, four-way control measures the baseline, candidate, unused-opcode probe and independently rebuilt removal through one executable path. Their median RSS values are 6,389,760, 6,471,680, 6,520,832 and 6,479,872 bytes: +1.28%, +2.05% and +1.41% relative to baseline. Thus the initial >3% process high-water reading does not persist in that control. All returned results and tracked counters match. Raw samples are in `vinci-opaque-rss-control/`. No remaining confirmed allocation, tracked-memory or RSS regression is attributable to the runtime change.

## Semantics and accounting

The checker intersects numeric facts across repeated checks. A proof can remove the instance-overload probe; unions and unchecked expressions retain it. Integer representation checks remain necessary because int includes big integers. Overflow promotion, division errors and floating-point arithmetic reuse the existing operators. Scalar `<=>` uses the existing float total order, including NaN and signed zero, and retains the comparison charge.

Short-circuit comparison fusion charges `Dup`, the conditional branch and `Pop` separately. Its temporary booleans cannot reserve stack storage because the comparison just removed two operands. Outlined operations retain their original instruction locations and accounting.

A paired audit of 197,906 baseline engine cases finds zero step, peak or retained-byte changes. Four known clock-dependent observations vary; stable observations do not change. Six new benchmark modes have counters recorded from the original baseline. No existing counter record is changed and no Counter log entry is needed. An additional 2,412 arithmetic and 1,777 quota probes match the baseline byte for byte. Permanent differential tests compare proved and generic bytecode at every step boundary and around memory boundaries.

Larger typed designs were rejected: duplicated operator bodies, secondary numeric dispatch, borrowed-stack operands, literal fusion and separate quota-mode loops all exceeded the M4 regression limit despite several x86 gains. The retained proof flag avoids duplicating arithmetic dispatch. Function-pointer dispatch was also rejected: both passing the opcode payload and reading it in the callee regressed M4 float/union/block loops by 3–5%. The retained selector uses direct calls and passes address operands already decoded. Re-reading those operands in each helper regressed x86 record updates by 4.2–4.9% and was rejected. The unused-opcode audit also found that the general dispatcher inlined the opcode conversion; keeping that conversion out of line prevents the new variant from reshaping the general opcode match. Moving local-address setup out of line was rejected after the full M4 push benchmark confirmed a 5–6% regression; that small, frequent operation remains inline. Raw snapshots, disassembly and paired diagnostics remain under work/ and the per-host directories.

## Verification

The exact measured source passes formatting; workspace/all-target Clippy with all features and no default features, with warnings denied; all 2,072 native workspace tests; 236,946 golden cases; portable/SIMD counter validation; and WASI (1,914 tests passed, eight platform exclusions, plus the preopen witness checks). Logs are `work/opaque-{clippy,clippy-portable,tests,golden,validation,wasi}.log`; paired semantic audits are in `work/opaque-audit.json`, `work/opaque-numeric.jsonl` and `work/opaque-numeric-quota.jsonl`. The branch-fusion probe adds 1,090 identical baseline/candidate quota observations.

The existing golden counter records are byte-for-byte unchanged; six records for the new fixture modes were added from the baseline. Counter log entries: none.

The final distributed gate runs from the main repository after the delivery commits. Its output and tested head are retained in `work/final-gate.log` and `work/gate-head.txt`; the delivery message reports its actual result. `mgomes/rust-language` remains at `fef329a395807d4958bdc94540c004a75cf8d90c`; no rebase is required.

The remaining priority is the array-construction placement sensitivity shown by the reversible control. Broader unused-tail `push`, `concat` and `+=` forms are deferred.
