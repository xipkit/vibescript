# VM round 4 measurements

Common array and numeric iteration now shares a compact, charged arena across two active nesting levels. Nested blocks return to round 2's 16 allocations, and `glue_orders` uses 2,504, below round 2's 2,509. Their tracked peaks are also lower than round 3. Comparison/branch and proven block-argument binding avoid temporary operands and dispatches while retaining each original instruction's step charge.

Three x86 JSON timing results remain above the requested 3% limit in longer confirmation runs. This is **not a clean suite-wide performance pass**; the unresolved cases and profile evidence are below.

## Method

Measured Rust source: `15b8c1c9`, against `cfc52131`. The latter adds the new fixtures to `a23d4ece` without changing Rust source. Both hosts run eight paired rounds of all 152 core cases, rotating before/after portable and SIMD binaries through one fixed executable path, with a 150 ms target per case. Timing and allocation instrumentation use separate binaries. RSS is a separate process high-water mark, including initialization; it is not the script's tracked memory budget.

- arm64: `vinci`, Apple M4. Native offline release builds.
- x86_64: `shannon`, Intel Core Ultra 9 285H. Both revisions and profiling workloads are pinned to performance core 2. Final timing runs also disable ASLR per process (`setarch x86_64 -R`); the environment records personality `00040000`.
- Each runner reserves its host with a gate marker. If another gate starts, the monitor discards the interrupted attempt and waits. Interrupted candidate runs are diagnostic artifacts, not reported measurements.
- Raw rounds, allocation records, environment details, binary hashes, profiles and analysis are under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/vm-round-4/`. Final measurements are in `{vinci,shannon}-final/`; baseline profiles are in `{vinci,shannon}-baseline/profiles/`.

The 15 added workloads cover record totals, hash bucket counts, nested `for` and block loops with `break`/`next`, `times`, indexed array iteration, array/range `map`, `select` and `reduce`, and array/string construction. Existing numeric `while` loops, array `each` and glue workloads remain controls. Python computes the new expected results independently. Ranges already stream without materialization; this round preserves that behavior. The language exposes `each_with_index` on arrays, not ranges, so the indexed fixture uses an array.

## Storage and accounting

The common driver excludes hash, grouping and window buffers. The VM holds the optional arena in one pointer; calls without common iteration allocate no arena. The allocated box includes both driver slots and all its bookkeeping. Its two-slot arena reserves 320 bytes instead of the previous first-iteration reservation of 608 bytes. Inner completion drops receiver/output references and makes the slot reusable; the full capacity stays charged until the last active arena driver exits. Deeper nesting uses a separate compact box. Return, break, rescue and failed frame entry release the corresponding live state. Reuse retains the interruption checkpoint previously supplied by each allocation.

The simple VM loop consumes a comparison followed by a conditional branch, or a plain block argument followed by store/pop, without their intermediate stack values. Each original instruction still advances its source position and charges separately. Frame cleanup counts bindings and pooled drivers during the existing release traversal. Existing arithmetic helpers retain overflow promotion, mixed-number behavior and NaN handling. `Op` remains 16 bytes, and a compile-time assertion also protects the 136-byte native frame.

The Counter log records only intentional peak reductions. Steps and retained bytes are unchanged. Paired same-checkout audits found no counter increases; clock-dependent cases retain their prior entries. Only affected counters were refreshed: 83 conformance, 3,775 language, 6 compatibility and 767 replay cases, including eight newly completed calls. Existing observations remain byte-for-byte unchanged, including recorded quota outcomes. Thirty new metered/unlimited fixture observations were added.

## Full-suite outliers and confirmation

All 304 M4 timing comparisons (152 cases, portable and SIMD) have unchanged or reduced allocation counts and tracked counters. The full sweep had one timing result above 3% and two RSS readings above 3%. Eight longer paired timing rounds (500 ms per case) and sixteen RSS samples per binary did not confirm those regressions; no confirmed M4 timing or RSS result exceeds 3%. The confirmation includes numeric loops, array iteration, nested blocks and glue controls. Its metrics and raw samples are in `vinci-confirmation/`.

| M4 case | Build | Metric | Full sweep | Confirmation |
| --- | --- | --- | ---: | ---: |
| length_mixed_65536/unlimited | portable | Time | +3.08% | +0.20% |
| loop_array_build/metered | simd | RSS | +3.15% | +0.51% |
| string_build/unlimited | simd | RSS | +3.04% | +2.15% |

The x86 full sweep has four timing results above 3%. Eight longer paired rounds confirm three JSON results; the string-length result does not persist. No x86 allocation, allocated-byte, tracked-counter or RSS increase was found. Both final suites preserve steps and retained bytes.

| x86 case | Build | Full time, µs | Change | Confirmation time, µs | Change |
| --- | --- | ---: | ---: | ---: | ---: |
| json_parse_escaped_4k/metered | portable | 9.964 → 10.659 | +6.97% | 10.030 → 10.467 | +4.35% |
| json_parse_escaped_4k/unlimited | portable | 9.962 → 10.424 | +4.63% | 10.048 → 10.359 | +3.09% |
| json_stringify_escaped_4k/unlimited | simd | 8.367 → 8.782 | +4.96% | 8.321 → 8.608 | +3.45% |
| length_65536/unlimited | simd | 1.267 → 1.307 | +3.15% | 1.272 → 1.298 | +2.01% |

JSON source is unchanged. Separate 500,000-call metered captures put portable parsing in `read_string::<false>` (65.3% → 65.2% of self samples) and SIMD writing in `write_string` (74.9% → 76.3%). There are 5,025 → 5,206 parser samples and 4,317 → 4,316 writer samples. These metered captures locate the hot code; they do not explain the unlimited writer timing regression.

Exact-binary disassembly comparison finds identical instruction sequences, after normalizing relocations, in 28 selected JSON/text/budget helper functions in each build. This includes both string readers, the writer, byte-buffer growth/copying, imports and budget charging. No changed inlining was found in those bodies. Function addresses did move: the hot portable reader starts at offset 48 → 16 modulo 64, and the SIMD writer at 48 → 32. Code placement is a hypothesis, not an established cause. Shared VM entry/layout changes also remain a possible influence.

The JSON owner should investigate these remaining controls together with round 3's writer regression before treating the strict performance requirement as closed. Raw confirmation data and profiles are in `shannon-confirmation/` and `shannon-outliers/`; verified binary hashes, normalized helper comparisons and disassemblies are in `work/linux-symbols/final-outliers/`. Earlier candidates remain in separately named diagnostic directories and are not mixed into the final tables.

## Profiles and remaining work

Samply captures cover the two arithmetic controls and nineteen iteration/glue cases, 10,000 metered calls per case. Baseline arithmetic samples were concentrated in the simple VM loop: 91.7% on M4 and 97.5% on x86. Iteration captures showed repeated driver dispatch, value copies, block entry and frame cleanup, motivating the compact reusable state and the two fusion paths.

Mac captures use `samply record --save-only --unstable-presymbolicate`. Linux uses user-only `perf record` at core 2, then `samply import`, because the installed Samply recorder rejects the host's perf policy. No host-wide settings were changed. Linux symbolication uses the exact measured ELF; raw imports, resolved symbols and helper outputs are preserved. Sample shares describe where execution was sampled; they are not timing speedups.

The M4 iteration capture has 28,447 samples before and 29,507 after. Generic iteration advancement and its result handler account for 4.0% and 1.3% of baseline self samples; the compact driver accounts for 3.0% afterward. Block entry moves from 2.9% to 2.8%, frame cleanup from 3.4% to 3.3%, and the simple VM loop from 21.8% to 21.4%.

The x86 iteration capture has 48,458 samples before and 45,991 after. Generic iteration advancement and its result handler account for 7.3% and 0.7% before; the compact driver accounts for 3.7% afterward. Frame cleanup moves from 1.1% to 0.9%, while `Buffer<Value>::extend` remains prominent at 19.4% → 20.4%.

Array construction remains a follow-up target. The loop's last result can retain the previous array and force copying during the next append. Eliminating that alias needs proof that the loop result is unused, while preserving returned values and retained-capacity accounting. Existing arithmetic helpers retain integer overflow promotion and NaN behavior; this round does not add new arithmetic opcodes. Ranges already iterate without materializing a collection.

## Validation

The measured source passed formatting, Clippy for all targets with all features and with no default features, 2,010 workspace tests, all 236,926 golden cases, and `scripts/check-wasi`. Portable/SIMD comparison checks expected values and identical counters on each measured architecture. Golden output retains the documented quota-dependent replay outcomes and clock-dependent drift, with zero failures.

After measurement, rebasing onto `2d42a247` added footprint tooling without changing runtime or golden-counter trees (`work/verification-footprint-rebase.json`). Delivery was then rebased onto `4b8c9aef`, which adds upstream authoring diagnostics, documentation and tests. The measured VM, iteration, JSON, comparison fixtures and golden counters remain unchanged (`work/verification-authoring-rebase.json`). The rebased source passes formatting, both Clippy configurations, all 2,015 workspace tests, the golden suite and WASI. Fresh portable/SIMD validation checks 107,562 shared cases, exact counter agreement and both golden builds; results are in `local-authoring-validation/` and `work/validate-authoring-result.json`. Timings above are from the measured revision; binary timings were not recollected after the authoring rebase.

The quota regression exercises every possible step limit for five loop programs, including integer overflow promotion and NaN comparisons. During fusion development, another 1,000 paired baseline/candidate probes agreed on results, error kinds, messages, source locations and steps. Pool tests cover cancellation, deadlines, nested returns, breaks, rescue, recursion-limit entry failure, live payload release and capacity charging.

Local command results are in `work/verification.json`; `work/verification-rebase.json` proves that rebasing onto the new measurement harness changed none of the verified runtime, tests or Cargo inputs; counter audit and selective recording details are in `work/final3-audit.json`, `work/final3-failures.json` and `work/record-counters-final3.log`. The final distributed gate runs against the delivery branch after this report commit. Its output and tested head are retained in `work/gate-all.log` and `work/gate-head.txt`; the completion message reports its actual result.

## vinci: Apple M4

Source `15b8c1c9` against `cfc52131`. Eight paired rounds, SIMD, metered. All table entries are before → after; time is in microseconds, memory in bytes, RSS in MiB.

| Workload | Time, µs | Change | Allocations | Peak bytes | Retained bytes | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| numeric_loop | 38.77 → 37.04 | -4.5% | 9 → 9 | 1,464 → 1,464 | 0 → 0 | 5.61 → 5.55 |
| loop_float | 47.59 → 46.10 | -3.1% | 9 → 9 | 1,464 → 1,464 | 0 → 0 | 5.62 → 5.56 |
| loop_record_totals | 138.16 → 138.38 | +0.2% | 794 → 793 | 95,456 → 95,168 | 128 → 128 | 6.48 → 6.64 |
| loop_bucket_counts | 208.75 → 207.48 | -0.6% | 1,233 → 1,232 | 96,528 → 96,240 | 1,200 → 1,200 | 6.80 → 6.80 |
| loop_nested_control | 48.48 → 46.55 | -4.0% | 31 → 31 | 1,624 → 1,624 | 0 → 0 | 5.73 → 5.77 |
| block_nested_control | 86.79 → 80.97 | -6.7% | 38 → 16 | 4,536 → 3,768 | 0 → 0 | 6.09 → 6.09 |
| loop_times | 119.25 → 108.65 | -8.9% | 12 → 11 | 2,344 → 2,056 | 0 → 0 | 5.92 → 5.94 |
| array_each | 107.95 → 102.24 | -5.3% | 15 → 14 | 18,440 → 18,152 | 0 → 0 | 6.06 → 6.03 |
| array_each_index | 142.58 → 128.97 | -9.5% | 15 → 14 | 18,456 → 18,168 | 0 → 0 | 6.03 → 6.06 |
| range_each | 101.21 → 92.86 | -8.3% | 13 → 12 | 2,416 → 2,128 | 0 → 0 | 5.92 → 5.94 |
| array_map | 103.26 → 93.53 | -9.4% | 24 → 23 | 42,952 → 42,664 | 16,480 → 16,480 | 6.16 → 6.16 |
| array_select | 111.71 → 96.70 | -13.4% | 23 → 22 | 30,664 → 30,376 | 8,288 → 8,288 | 6.12 → 6.11 |
| array_reduce | 125.27 → 109.55 | -12.5% | 16 → 15 | 18,536 → 18,248 | 0 → 0 | 6.12 → 6.08 |
| range_map | 98.30 → 85.75 | -12.8% | 22 → 21 | 26,928 → 26,640 | 16,480 → 16,480 | 6.11 → 6.14 |
| range_select | 100.18 → 88.06 | -12.1% | 21 → 20 | 14,640 → 14,352 | 8,288 → 8,288 | 6.02 → 6.09 |
| range_reduce | 114.39 → 100.52 | -12.1% | 14 → 13 | 2,512 → 2,224 | 0 → 0 | 5.97 → 5.97 |
| loop_array_build | 585.37 → 587.78 | +0.4% | 1,343 → 1,343 | 23,664 → 23,664 | 10,752 → 10,752 | 5.95 → 6.14 |
| loop_string_build | 289.57 → 282.76 | -2.4% | 5,907 → 5,906 | 47,883 → 47,595 | 5,798 → 5,798 | 6.62 → 6.42 |
| nested_blocks | 124.95 → 119.04 | -4.7% | 49 → 16 | 4,536 → 3,768 | 0 → 0 | 6.02 → 5.95 |
| glue_orders | 208.21 → 206.49 | -0.8% | 2,569 → 2,504 | 229,848 → 229,720 | 4,192 → 4,192 | 6.81 → 6.80 |
| glue_orders_cap | 252.88 → 250.22 | -1.1% | 2,873 → 2,808 | 234,674 → 234,546 | 4,192 → 4,192 | 7.03 → 6.98 |

## shannon: Intel(R) Core(TM) Ultra 9 285H

Source `15b8c1c9` against `cfc52131`. Eight paired rounds, SIMD, metered. All table entries are before → after; time is in microseconds, memory in bytes, RSS in MiB.

| Workload | Time, µs | Change | Allocations | Peak bytes | Retained bytes | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| numeric_loop | 63.21 → 59.12 | -6.5% | 9 → 9 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.48 |
| loop_float | 81.85 → 77.48 | -5.3% | 9 → 9 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.48 |
| loop_record_totals | 223.62 → 220.48 | -1.4% | 794 → 793 | 95,456 → 95,168 | 128 → 128 | 13.48 → 13.48 |
| loop_bucket_counts | 311.53 → 306.10 | -1.7% | 1,233 → 1,232 | 96,528 → 96,240 | 1,200 → 1,200 | 13.48 → 13.48 |
| loop_nested_control | 67.26 → 63.64 | -5.4% | 31 → 31 | 1,624 → 1,624 | 0 → 0 | 13.48 → 13.48 |
| block_nested_control | 117.21 → 108.59 | -7.4% | 38 → 16 | 4,536 → 3,768 | 0 → 0 | 13.48 → 13.48 |
| loop_times | 173.96 → 155.35 | -10.7% | 12 → 11 | 2,344 → 2,056 | 0 → 0 | 13.48 → 13.48 |
| array_each | 150.22 → 140.34 | -6.6% | 15 → 14 | 18,440 → 18,152 | 0 → 0 | 13.48 → 13.48 |
| array_each_index | 213.12 → 185.39 | -13.0% | 15 → 14 | 18,456 → 18,168 | 0 → 0 | 13.48 → 13.48 |
| range_each | 143.23 → 124.55 | -13.0% | 13 → 12 | 2,416 → 2,128 | 0 → 0 | 13.48 → 13.48 |
| array_map | 157.84 → 145.58 | -7.8% | 24 → 23 | 42,952 → 42,664 | 16,480 → 16,480 | 13.48 → 13.48 |
| array_select | 170.99 → 150.21 | -12.2% | 23 → 22 | 30,664 → 30,376 | 8,288 → 8,288 | 13.48 → 13.48 |
| array_reduce | 202.31 → 170.22 | -15.9% | 16 → 15 | 18,536 → 18,248 | 0 → 0 | 13.48 → 13.48 |
| range_map | 150.46 → 128.95 | -14.3% | 22 → 21 | 26,928 → 26,640 | 16,480 → 16,480 | 13.48 → 13.48 |
| range_select | 153.21 → 134.04 | -12.5% | 21 → 20 | 14,640 → 14,352 | 8,288 → 8,288 | 13.48 → 13.48 |
| range_reduce | 185.17 → 154.12 | -16.8% | 14 → 13 | 2,512 → 2,224 | 0 → 0 | 13.48 → 13.48 |
| loop_array_build | 1246.92 → 1236.12 | -0.9% | 1,343 → 1,343 | 23,664 → 23,664 | 10,752 → 10,752 | 13.48 → 13.48 |
| loop_string_build | 380.74 → 369.11 | -3.1% | 5,907 → 5,906 | 47,883 → 47,595 | 5,798 → 5,798 | 13.48 → 13.48 |
| nested_blocks | 168.30 → 157.06 | -6.7% | 49 → 16 | 4,536 → 3,768 | 0 → 0 | 13.48 → 13.48 |
| glue_orders | 284.32 → 276.87 | -2.6% | 2,569 → 2,504 | 229,848 → 229,720 | 4,192 → 4,192 | 13.48 → 13.48 |
| glue_orders_cap | 344.96 → 337.06 | -2.3% | 2,872 → 2,807 | 234,666 → 234,538 | 4,192 → 4,192 | 13.48 → 13.48 |
