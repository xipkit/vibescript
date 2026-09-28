# VM round 5 measurements

Regex literals share immutable compiled code with their script. Array appends at an unused loop tail release the previous iteration's alias before updating the array. Typed arithmetic is deferred after the measured designs exceeded the regression limit. `Op` remains 16 bytes.

## Method

The runtime baseline is `f363ac01`; `4a417e0e` adds only the three regex workloads and their independently computed observations. The measured candidate is `53ceaa429767fe6feffecc9171d8508b4c9f7279`, an unpublished measurement snapshot. Delivery commits separate the implementations, accounting updates and report. Each host runs eight paired rounds of the core and text suites, rotating baseline/candidate portable and SIMD binaries through one fixed executable path, targeting 150 ms per case. Timing and allocation instrumentation use separate binaries; RSS is measured in a fresh process and includes initialization.

- arm64: `vinci`, Apple M4, native offline release builds.
- x86_64: `shannon`, Intel Core Ultra 9 285H, pinned to performance core 2 with ASLR disabled per process (`taskset -c 2 setarch x86_64 -R`).
- Both hosts were reserved with the shared gate lock and a gate marker before building and measuring.
- Raw rounds, environments, binary revisions and analysis live under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/vm-round-5/`. Final measurements use `{vinci,shannon}-final10-{core,text}/`; earlier attempts are diagnostic and excluded.

The timings below precede the final JSON integration rebase. They describe the measured revisions above, not a new timing run of the combined stack. The final VM/compiler/value changes and fixtures remain identical; the rebased runtime is covered by a fresh accounting audit and final verification.

The added workloads parse 128 log lines, validate ID/email fields across 128 records, and match a literal in a counted loop. Existing numeric, range, array, record, glue, text and JSON workloads serve as controls. The regex fixtures have both metered and unlimited modes and Python-computed expected results.

## Before and after

SIMD, metered calls. Times are medians in microseconds; RSS is MiB. Allocation counts and tracked peak/retained bytes are per call. Each paired cell is baseline → candidate; a single value is unchanged. Portable and unlimited results are included in the raw reports and regression audit.

### arm64, vinci

| Workload | Time, µs | Change | Allocations | Tracked peak, B | Retained, B | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| regex_log_lines | 642.114 → 419.504 | -34.67% | 8,062 → 6,782 | 77,635 → 75,140 | 49,840 | 6.484 → 6.406 |
| regex_record_fields | 576.274 → 346.165 | -39.93% | 5,369 → 3,193 | 60,214 → 59,898 | 2,144 | 6.312 → 6.297 |
| regex_loop | 206.351 → 119.081 | -42.29% | 2,316 → 1,292 | 20,231 → 19,955 | 0 | 5.938 → 6.000 |
| loop_array_build | 564.901 → 105.827 | -81.27% | 1,343 → 678 | 23,664 → 23,568 | 10,752 | 6.000 → 5.922 |
| numeric_loop | 36.667 → 37.044 | +1.03% | 9 | 1,464 | 0 | 5.438 → 5.406 |
| loop_float | 45.283 → 45.007 | -0.61% | 9 | 1,464 | 0 | 5.453 → 5.391 |
| loop_range | 42.714 → 42.574 | -0.33% | 10 | 1,536 | 0 | 5.484 → 5.453 |
| range_each | 90.804 → 90.948 | +0.16% | 12 | 2,128 | 0 | 5.688 → 5.719 |
| range_map | 85.313 → 86.749 | +1.68% | 21 | 26,640 | 16,480 | 5.875 → 5.922 |
| range_reduce | 98.965 → 98.965 | -0.00% | 13 | 2,224 | 0 | 5.672 → 5.766 |

### x86_64, shannon

| Workload | Time, µs | Change | Allocations | Tracked peak, B | Retained, B | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| regex_log_lines | 1,082.044 → 758.596 | -29.89% | 8,062 → 6,782 | 77,635 → 75,140 | 49,840 | 13.473 |
| regex_record_fields | 923.427 → 609.382 | -34.01% | 5,369 → 3,193 | 60,214 → 59,898 | 2,144 | 13.473 |
| regex_loop | 334.844 → 214.373 | -35.98% | 2,316 → 1,292 | 20,231 → 19,955 | 0 | 13.473 |
| loop_array_build | 1,580.985 → 196.385 | -87.58% | 1,343 → 678 | 23,664 → 23,568 | 10,752 | 13.473 |
| numeric_loop | 71.567 → 71.915 | +0.49% | 9 | 1,464 | 0 | 13.473 |
| loop_float | 94.904 → 95.006 | +0.11% | 9 | 1,464 | 0 | 13.473 |
| loop_range | 92.002 → 91.399 | -0.66% | 10 | 1,536 | 0 | 13.473 |
| range_each | 153.283 → 154.541 | +0.82% | 12 | 2,128 | 0 | 13.473 |
| range_map | 162.326 → 161.775 | -0.34% | 21 | 26,640 | 16,480 | 13.473 |
| range_reduce | 193.841 → 193.708 | -0.07% | 13 | 2,224 | 0 | 13.473 |


## Regression audit

Every full-sweep increase over 3% received eight longer paired timing rounds (500 ms per case) or sixteen paired fresh-process RSS samples. The following tables retain all initial outliers. A dash means the result cleared in confirmation and did not need an alignment control.

The separate placement control builds the same before/after sources with `-C llvm-args=-align-all-functions=6`, then rotates original and aligned binaries through eight paired rounds (250 ms per case), plus sixteen RSS samples. These binaries are diagnostic; the production flags are unchanged. Original and aligned results are kept separate.

| Host | Case | Build | Full time | Confirmation | Aligned control |
| --- | --- | --- | ---: | ---: | ---: |
| vinci | hash_lookup_8/unlimited | portable | +3.24% | +3.53% | -0.02% |
| vinci | upcase_65536/metered | simd | +4.13% | +4.50% | -3.71% |
| vinci | upcase_65536/unlimited | simd | +4.70% | +3.93% | -3.46% |
| vinci | json_parse_escaped_4k/metered | simd | +3.93% | +4.11% | -0.49% |
| vinci | json_parse_escaped_4k/unlimited | simd | +3.99% | +4.83% | -0.04% |
| vinci | text/join/metered | portable | +4.07% | +3.57% | +0.15% |
| vinci | text/join/unlimited | portable | +4.68% | +2.40% | — |
| vinci | text/start_with/metered | simd | +3.89% | -1.07% | — |
| vinci | text/start_with/unlimited | simd | +4.26% | -2.01% | — |
| vinci | text/end_with/metered | simd | +3.81% | +0.69% | — |
| vinci | text/end_with/unlimited | simd | +3.59% | -1.28% | — |
| vinci | text/index_short_hit/metered | simd | +3.27% | -0.47% | — |
| vinci | text/index_unicode/metered | simd | +5.50% | +6.57% | -0.85% |
| vinci | text/index_unicode/unlimited | simd | +7.24% | +7.64% | +0.05% |
| shannon | json_parse_escaped_4k/metered | portable | +8.84% | +9.09% | +4.25% |
| shannon | json_parse_escaped_4k/unlimited | portable | +8.84% | +8.78% | +1.35% |
| shannon | json_stringify_escaped_4k/metered | portable | +3.70% | +2.33% | — |
| shannon | json_stringify_escaped_4k/unlimited | portable | +3.37% | +5.31% | +0.39% |
| shannon | text/regex_unanchored_miss/metered | portable | +5.33% | +6.58% | +6.29% |
| shannon | text/regex_unanchored_miss/unlimited | portable | +4.39% | +6.27% | +2.34% |
| shannon | text/regex_literal_miss/metered | portable | +7.00% | +11.90% | +3.19% |
| shannon | text/regex_literal_miss/unlimited | portable | +7.49% | +12.90% | +0.75% |

All initial RSS outliers are M4 portable results. Both unperturbed and aligned measurements below are from the four-way placement control; they show the sensitivity of process high-water marks separately from tracked allocation accounting.

| Case | Full RSS | Confirmation | Control, original | Control, aligned |
| --- | ---: | ---: | ---: | ---: |
| json_object_2048/metered | +5.00% | +4.29% | +4.28% | +0.44% |
| json_object_2048/unlimited | +5.23% | +3.12% | +2.33% | +0.88% |
| json_duplicates_512/unlimited | +4.36% | +3.80% | +3.81% | -0.24% |
| json_transform/metered | +4.17% | +3.91% | +2.98% | -0.38% |
| json_transform/unlimited | +4.18% | +2.44% | — | — |
| loop_array_build/unlimited | +3.76% | +1.05% | — | — |
| loop_string_build/metered | +5.85% | +5.15% | +2.08% | +4.96% |
| string_concat/metered | +4.84% | +5.50% | +2.36% | +2.20% |
| glue_orders_cap/metered | +3.00% | +3.24% | +1.95% | -0.22% |
| json_object_512_cap/metered | +3.19% | +3.93% | +3.93% | -0.24% |
| json_object_512_cap/unlimited | +3.44% | +3.80% | +1.57% | +0.48% |
| site_sieve_of_eratosthenes/metered | +3.12% | +2.19% | — | — |
| site_sieve_of_eratosthenes/unlimited | +3.12% | +2.58% | — | — |
| site_top_rank_per_group/unlimited | +4.31% | +3.02% | +2.25% | +1.25% |

The exact timing binaries were verified against their recorded SHA-256 hashes before disassembly. Forty selected JSON, regex search, text/index/join, hash and budget helpers in each build on each architecture have identical instruction sequences after resolving branch/import relocations and ARM page-relative addresses. No changed inlining appears in those bodies. Source equality and the raw disassemblies/differences are in `work/unchanged-helper-source.json` and `work/final10-binaries/`; the comparison program is `work/compare-asm10.py`.

The eight persistent M4 timing outliers all fall below +3% with the fixed-source alignment perturbation. The x86 writer does too; parser and regex miss results remain sensitive to placement and measurement order. These are recorded as nonblocking placement effects under the refined rule, rather than erased from the results.

The regex miss cases execute a cached literal, so unchanged search instructions alone are insufficient attribution. A separate eight-round x86 control replaces each literal with an equivalent dynamic string pattern, bypassing the new cache entirely. The dynamic path still slows by 7.92–12.20%, with identical allocations, allocated bytes, steps, tracked peak and retained bytes. Its search/compilation source is unchanged. The original literal cases in the same run slow by 6.06–14.42%; repeated identical-binary controls show additional variability, particularly for the literal scanner. This reproduces the effect without executing the changed literal path. The paired inputs, samples and allocation checks are in `shannon-regex-path-control/`.

RSS is a process high-water mark, including code pages and startup. The fixed-source perturbation alone changes baseline RSS by up to 3.93% in these selected cases. In the original-binary repeat, the previously +5.15% string-loop and +5.50% concat readings become +2.08% and +2.36%; the aligned string-loop difference is +4.96% and remains disclosed. All per-call allocation and tracked-memory metrics for these controls are identical. These process-layout effects are nonblocking under the stated RSS control rule; this is not a claim that every raw RSS comparison is below 3%.

Across all 896 final comparisons, allocation counts, allocated bytes, steps and tracked peak/retained bytes never increase. The actual changes remove regex compilation and unused array copies; no arithmetic specialization is retained. Full and confirmation data are under `{vinci,shannon}-final10-{core,text}[-confirmation]/`, and placement controls under `{vinci,shannon}-layout-control/`.

## Semantics and accounting

Host compilation builds each literal once and stores either the compiled value or a deferred pattern error. Evaluation imports an independently charged view of its source, expanded pattern and full code capacities. Errors retain their evaluation point and source position. Cold required files compile the literal on first evaluation under the original invocation limits, then detach the cached value from that invocation's budget. A mutex serializes cold initialization. Quota, cancellation and deadline failures are not cached. Tests cover concurrent cold callers, budget release, deferred errors and retry after exhaustion.

Only a direct `<<` at a provably unused loop tail receives the discard flag on `Shovel`. It removes the loop's own alias only when it points to the array being appended to; snapshots and recovery aliases keep ordinary copy-on-write behavior. Exact-capacity growth preserves returned capacity charges. Tests cover returned loop values, saved snapshots, loop expressions, nesting, `break`, `next` and rescue.

Typed arithmetic experiments preserved results, overflow promotion, NaN behavior and every original charge, but failed the performance requirement. Both separate opcodes and integer proof flags on existing arithmetic instructions were measured. The broader scalar design improved x86 floating-point loops by about 3%, while M4 floating-point loops regressed by more than 4%. Even the final restricted integer design regressed the M4 SIMD numeric loop by 3.4% and its unlimited branch loop by 13.8%. No arithmetic specialization is retained. Experimental source patches and paired timing data are in `work/numeric-experiment.patch`, `work/numeric-final-experiment.patch`, and the `*-quick4` through `*-quick9` directories.

An early array implementation also inlined update machinery into the tight interpreter. Exact-binary M4 disassembly showed its extent growing from 15,688 to 19,940 bytes, with the local stack reservation growing from 992 to 1,584 bytes. Moving the update into an out-of-line helper reduced those to 16,152 and 1,024 bytes in the last arithmetic experiment. The delivered version also removes the rejected arithmetic code. The disassemblies and measured binaries are in `work/arm-asm8/`.

The Counter log records two intentional reductions. Regex caching changes six conformance, 722 language, three compatibility and 199 replay counters; `regex_record_fields` falls from 69,947 to 41,915 steps and 60,214 to 59,898 peak bytes, with 2,144 retained bytes unchanged. Alias removal changes the two `loop_array_build` modes and replay `call52200`; array construction falls from 76,297 to 20,686 steps and 23,664 to 23,568 peak bytes, retaining 10,752 bytes. A paired audit found 933 changed successful counters and no increases. Existing observations remain byte-for-byte unchanged, including recorded quota outcomes. Arithmetic remains unchanged and requires no counter updates.

Dynamic string pattern caching is deferred: it needs a bounded eviction policy, operation-specific error handling and complete capacity charging, and would touch the regex area owned by another task. This round introduces no global cache. Other append shapes and a cheaper arithmetic design remain follow-up work.

The existing replay counter drift for `call2881` remains unrecorded: the saved baseline and candidate both report 56 steps, while the historical counter is 54. It is unrelated to these changes.

## Verification and delivery

The measured runtime passed `cargo fmt --check`, workspace/all-target Clippy with all features and with no default features (`-D warnings`), 2,027 native workspace tests, all 236,932 golden cases, portable/SIMD counter validation, and `scripts/check-wasi` (1,874 tests passed, eight expected platform exclusions, plus the sandbox witnesses). Builds used `./scripts/cargo`, offline mode and four local build jobs. Logs and exit codes are in `work/verification10.json` and `work/10-*.log`.

Delivery was first rebased onto build-speed integration `855ff8f2`, preserving the complete measured runtime. It was then rebased onto JSON integration `a4cc97a1`, which changes JSON, record and budget code. The VM/compiler/value changes and fixtures remain byte-for-byte identical to the measured snapshot; `work/json-rebase-proof.json` records that narrower comparison. The ten upstream JSON counter reductions were retained before re-recording this round's same 933 reductions. A fresh paired audit of 164,767 engine cases found no counter increases or newly completed calls, and every observation file remains unchanged. The four clock-dependent and 25 quota-dependent differences keep their prior recorded outcomes. Raw observations, the audit and the recording log are `work/json-{before,after}-observations.jsonl`, `work/json-rebase-audit.json` and `work/json-record-counters.log`.

The pre-JSON delivery `dce42c6c` passed the three-host gate with zero failures, and its exact-head WASI check passed 1,875 tests with eight expected exclusions. The JSON-rebased delivery passed all seven required local checks: 2,045 workspace tests, all 236,932 golden cases, portable/SIMD counter equality, both Clippy configurations, formatting, and WASI (1,892 tests passed with 8 expected exclusions and the sandbox witnesses). Logs and exit codes are in `work/verification-json.json` and `work/json-*.log`. The distributed delivery gate is `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/gate-all.sh mgomes/vm-round-5`, run from the main repository after the other gates finish. Its exact tested head and output are archived as `work/gate-head.txt` and `work/gate-all.log`.

Follow-up work is a cheaper typed-arithmetic design that clears both architectures, broader unused append forms, and a separately bounded dynamic-pattern cache. The rejected arithmetic experiments and the persistent unchanged-code layout effects should remain separate from those future changes.
