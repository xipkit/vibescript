# VM round 3 measurements

Active iterator storage, declared instance slots and borrowed literal record keys reduce per-call peaks and speed up the glue workloads on both architectures. The complete suite still has results above the requested 3% time limit, listed below; these results are not a clean suite-wide performance pass.

Measured source: `52da6c2c`, against `3161370a`. The measurement commit was subsequently reworded as `312b750b`; its tree is identical (`0f88ca5a5a721711a64126e0418006b76b9d3670`). Both hosts ran eight paired rounds of all 122 core cases, rotating four timing binaries (before/after, portable/SIMD), with a 150 ms target per case. Builds used Rust 1.98.1, the repository release profile and offline Cargo. Allocation counts and bytes come from separate instrumented binaries; RSS is each process's high-water mark, including initialization, rather than the per-call tracked budget. Each table entry is **before → after**.

- **arm64:** `vinci`, Apple M4, macOS 26.6.2.
- **x86_64:** `shannon`, Intel Core Ultra 9 285H, Linux 7.1.6. Both revisions were pinned to performance core 2. Earlier unpinned experiments are retained separately and are not used in these tables.
- Hosts were reserved through gate markers; a monitor discarded an attempt if another gate started. Raw outputs, environment records and binary hashes are in `.cache/vm-round-3/{vinci,shannon}-operators/` on the external volume.

## Changes and accounting

The first collection iteration now reserves 608 bytes (128 bytes of handles plus a 480-byte active driver), instead of four 544-byte states totaling 2,176 bytes. Rare drivers reserve only their own boxed state. This trades additional allocations for a lower simultaneous peak: nested blocks allocate 49 times instead of 16, but peak at 4,536 instead of 5,624 bytes. Unused pending input copies are omitted for array `each`, `map` and indexed map.

Declared fields use compile-time slot numbers. Linked slots preserve first-write order and distinguish absent fields from assigned `nil`; imports, identity, equality, inspection, printing and host traversal retain their behavior. Internal environments still use named fields. Sparse initialization is metered in bounded chunks. `records::Fields` retains its existing API.

Literal record indexing borrows the compiled key and removes its separate import/push instruction. Literal hashing and rare errors stay out of the dispatch loop. Scalar arithmetic keeps operator codes until a general operation or overload needs the spelling. The final numeric loops stay within 3% on both architectures, but this does not establish immunity to future code-layout changes. A scalar-argument-copy experiment was reverted because it slowed calls.

The Counter log records iterator storage, declared slots and literal indexing. Only affected counters were refreshed: 125 conformance, 9,761 language, 22 compatibility and 9,334 replay cases. All observation files remain byte-for-byte baseline; quota-driven replay outcomes retain their recorded observations. Paired counter audits found no increases. Portable and SIMD counters agree on each architecture. Clock-dependent cases retain their prior counters.

## Results above 3%

| Host | Case | Build | Time, µs | Change |
| --- | --- | --- | ---: | ---: |
| vinci | upcase_65536/metered | simd | 5.676 → 5.916 | +4.22% |
| vinci | upcase_65536/unlimited | simd | 5.582 → 5.833 | +4.49% |
| vinci | upstream_greeting/metered | simd | 0.955 → 0.994 | +4.05% |
| shannon | json_stringify_escaped_4k/metered | simd | 7.479 → 8.345 | +11.58% |
| shannon | json_stringify_escaped_4k/metered | portable | 8.132 → 8.900 | +9.45% |
| shannon | json_stringify_escaped_4k/unlimited | simd | 7.458 → 8.469 | +13.55% |
| shannon | json_stringify_escaped_4k/unlimited | portable | 8.144 → 8.881 | +9.05% |

The JSON writer and text implementation were not changed in this branch. These remaining regressions need a coordinated check after those agents' changes; they must not be hidden by the faster glue averages. The external artifact directory also retains focused confirmation runs when available.

## Profiles

Samply captures cover `glue_orders` and `glue_orders_cap`, before and after. Final captures run 25,000 metered calls each. Percentages below are inclusive sample shares, not elapsed-time speedups. Member-call wrappers include the JSON work they invoke.

| Architecture and case | JSON document, before → after | Simple VM loop, before → after | General index → literal index |
| --- | ---: | ---: | ---: |
| M4, glue_orders | 42.4% → 46.8% | 24.1% → 20.3% | 8.4% → 6.7% |
| M4, glue_orders_cap | 37.5% → 37.6% | 20.5% → 19.1% | 6.8% → 5.6% |
| x86, glue_orders | 39.7% → 44.4% | 25.6% → 21.1% | 9.0% → 6.1% |
| x86, glue_orders_cap | 35.4% → 35.6% | 22.3% → 20.1% | 7.7% → 5.7% |

JSON parsing remains the dominant cost, especially string reading, key copying/insertion and typed traversal. That work is left to the JSON agent. The M4 `upcase_65536` diagnostic puts 43% of self samples in the text transform and 34% in memory copying.

Mac captures use `samply record --save-only --unstable-presymbolicate`. Linux's installed Samply recorder refuses `perf_event_paranoid=2`, while user-space `cycles:u` events are permitted. Linux captures therefore use user-only `perf record`, then `samply import`; no system settings or privileges were changed. On this hybrid CPU, imported core events appeared as stack-bearing markers because Samply chose the inactive Atom event first. Derived `*-cpu-core.json.gz` profiles select those core markers as the sample stream. Raw imports and Samply API symbolication responses are preserved beside them. The four Linux captures contain 7,447–9,466 samples each.

Artifacts and analysis helpers are under `.cache/vm-round-3/`: Mac profiles in `work/`, Linux profiles in `shannon-operators/profiles/`, and the exact extraction logic in `work/symbolicate_linux.py`. The Linux ELF files required to reopen profiles locally are in `work/linux-symbols/` and can be passed to Samply with `--symbol-dir`.

## Validation

The measured Rust source passed formatting, Clippy with all features and without defaults, 2,006 workspace tests, all 236,896 golden cases, portable/SIMD validation, and `scripts/check-wasi`. Golden output contains the documented quota drift and one clock-dependent counter drift, with zero failures. The final distributed gate result is reported with the branch head in the task's completion report.

## Detailed before/after tables

## vinci-operators: Apple M4

52da6c2ceb62bb12b3d9215885d0dc06709f28e0 versus 3161370a4e20c0f08d286dccd95ce8f5d0bbd10a; 8 rounds, 150.0 ms target per variant and case.

### simd, metered

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 110.50 → 106.20 | -3.9% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 5.98 → 5.97 |
| array_map_select | 197.19 → 193.72 | -1.8% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 6.06 → 6.06 |
| nested_blocks | 128.02 → 123.24 | -3.7% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 5.92 → 5.95 |
| method_calls | 107.46 → 91.36 | -15.0% | 27 → 24 | 4,853 → 4,520 | 5,023 → 4,690 | 0 → 0 | 5.89 → 5.92 |
| function_calls | 54.77 → 55.86 | +2.0% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 5.66 → 5.78 |
| record_fields | 122.10 → 116.18 | -4.8% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 6.47 → 6.41 |
| record_fields_cap | 130.66 → 124.83 | -4.5% | 907 → 905 | 93,578 → 91,866 | 100,870 → 99,094 | 0 → 0 | 6.66 → 6.67 |
| record_update | 297.59 → 296.57 | -0.3% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 5.67 → 5.80 |
| record_build | 345.24 → 329.67 | -4.5% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 6.31 → 6.39 |
| record_build_cap | 358.93 → 335.74 | -6.5% | 2,142 → 2,143 | 253,866 → 252,298 | 234,966 → 233,398 | 0 → 0 | 6.56 → 6.58 |
| records_retained | 118.54 → 115.18 | -2.8% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 6.34 → 6.48 |
| records_retained_cap | 121.90 → 118.53 | -2.8% | 1,162 → 1,162 | 132,890 → 132,890 | 122,182 → 122,182 | 115,188 → 115,188 | 6.69 → 6.73 |
| glue_orders | 216.20 → 206.35 | -4.6% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 6.73 → 6.80 |
| glue_orders_cap | 259.76 → 247.85 | -4.6% | 2,813 → 2,873 | 249,262 → 278,134 | 237,242 → 234,674 | 4,192 → 4,192 | 7.00 → 7.05 |
| json_transform_cap | 39.13 → 38.38 | -1.9% | 480 → 479 | 44,409 → 44,353 | 43,853 → 43,853 | 128 → 128 | 6.45 → 6.36 |
| json_object_512_cap | 76.68 → 76.45 | -0.3% | 1,687 → 1,687 | 111,690 → 111,690 | 92,384 → 92,384 | 76,984 → 76,984 | 6.61 → 6.64 |
| hash_lookup_cap | 107.53 → 106.96 | -0.5% | 193 → 193 | 16,442 → 16,442 | 16,002 → 16,002 | 0 → 0 | 6.12 → 6.11 |
| member_calls_cap | 60.10 → 58.72 | -2.3% | 385 → 386 | 30,578 → 29,010 | 39,532 → 37,964 | 0 → 0 | 6.27 → 6.28 |
| numeric_loop | 38.31 → 38.00 | -0.8% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.52 → 5.61 |
| loop_float | 47.47 → 46.68 | -1.7% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.50 → 5.61 |

### simd, unlimited

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 109.57 → 105.20 | -4.0% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 6.00 → 6.00 |
| array_map_select | 196.91 → 193.09 | -1.9% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 6.11 → 6.12 |
| nested_blocks | 127.13 → 123.07 | -3.2% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 5.94 → 5.91 |
| method_calls | 107.74 → 91.21 | -15.3% | 27 → 24 | 4,853 → 4,520 | 5,023 → 4,690 | 0 → 0 | 5.86 → 5.97 |
| function_calls | 54.71 → 55.97 | +2.3% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 5.64 → 5.72 |
| record_fields | 121.79 → 116.27 | -4.5% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 6.42 → 6.38 |
| record_fields_cap | 130.41 → 124.41 | -4.6% | 907 → 905 | 93,578 → 91,866 | 100,870 → 99,094 | 0 → 0 | 6.64 → 6.67 |
| record_update | 298.52 → 297.82 | -0.2% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 5.70 → 5.77 |
| record_build | 343.78 → 331.21 | -3.7% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 6.33 → 6.36 |
| record_build_cap | 356.93 → 336.69 | -5.7% | 2,142 → 2,143 | 253,866 → 252,298 | 234,966 → 233,398 | 0 → 0 | 6.53 → 6.58 |
| records_retained | 117.61 → 115.61 | -1.7% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 6.38 → 6.48 |
| records_retained_cap | 120.87 → 118.90 | -1.6% | 1,162 → 1,162 | 132,890 → 132,890 | 122,182 → 122,182 | 115,188 → 115,188 | 6.70 → 6.69 |
| glue_orders | 215.32 → 206.47 | -4.1% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 6.77 → 6.84 |
| glue_orders_cap | 258.56 → 248.40 | -3.9% | 2,813 → 2,873 | 249,262 → 278,134 | 237,242 → 234,674 | 4,192 → 4,192 | 6.98 → 6.98 |
| json_transform_cap | 39.03 → 38.20 | -2.1% | 480 → 479 | 44,409 → 44,353 | 43,853 → 43,853 | 128 → 128 | 6.47 → 6.41 |
| json_object_512_cap | 76.31 → 76.15 | -0.2% | 1,687 → 1,687 | 111,690 → 111,690 | 92,384 → 92,384 | 76,984 → 76,984 | 6.59 → 6.66 |
| hash_lookup_cap | 106.47 → 105.94 | -0.5% | 193 → 193 | 16,442 → 16,442 | 16,002 → 16,002 | 0 → 0 | 6.12 → 6.12 |
| member_calls_cap | 59.83 → 58.60 | -2.1% | 385 → 386 | 30,578 → 29,010 | 39,532 → 37,964 | 0 → 0 | 6.34 → 6.25 |
| numeric_loop | 37.58 → 37.49 | -0.2% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.47 → 5.59 |
| loop_float | 46.50 → 46.02 | -1.0% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.53 → 5.58 |

### portable, metered

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 110.87 → 106.39 | -4.0% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 5.95 → 6.05 |
| array_map_select | 197.51 → 194.76 | -1.4% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 6.05 → 6.12 |
| nested_blocks | 127.85 → 123.67 | -3.3% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 5.94 → 6.00 |
| method_calls | 105.78 → 92.22 | -12.8% | 27 → 24 | 4,853 → 4,520 | 5,023 → 4,690 | 0 → 0 | 5.94 → 5.94 |
| function_calls | 54.77 → 55.34 | +1.0% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 5.73 → 5.73 |
| record_fields | 122.43 → 117.11 | -4.3% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 6.41 → 6.45 |
| record_fields_cap | 131.32 → 125.59 | -4.4% | 907 → 905 | 93,578 → 91,866 | 100,870 → 99,094 | 0 → 0 | 6.61 → 6.64 |
| record_update | 298.22 → 297.51 | -0.2% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 5.81 → 5.78 |
| record_build | 342.28 → 329.38 | -3.8% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 6.39 → 6.36 |
| record_build_cap | 355.86 → 338.43 | -4.9% | 2,142 → 2,143 | 253,866 → 252,298 | 234,966 → 233,398 | 0 → 0 | 6.58 → 6.58 |
| records_retained | 117.96 → 115.54 | -2.1% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 6.50 → 6.53 |
| records_retained_cap | 120.79 → 118.86 | -1.6% | 1,162 → 1,162 | 132,890 → 132,890 | 122,182 → 122,182 | 115,188 → 115,188 | 6.75 → 6.67 |
| glue_orders | 217.65 → 206.55 | -5.1% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 6.78 → 6.88 |
| glue_orders_cap | 258.70 → 248.88 | -3.8% | 2,813 → 2,873 | 249,262 → 278,134 | 237,242 → 234,674 | 4,192 → 4,192 | 7.12 → 7.05 |
| json_transform_cap | 39.30 → 38.33 | -2.5% | 480 → 479 | 44,409 → 44,353 | 43,853 → 43,853 | 128 → 128 | 6.48 → 6.39 |
| json_object_512_cap | 76.78 → 77.06 | +0.4% | 1,687 → 1,687 | 111,690 → 111,690 | 92,384 → 92,384 | 76,984 → 76,984 | 6.66 → 6.64 |
| hash_lookup_cap | 107.44 → 106.92 | -0.5% | 193 → 193 | 16,442 → 16,442 | 16,002 → 16,002 | 0 → 0 | 6.17 → 6.14 |
| member_calls_cap | 60.14 → 58.65 | -2.5% | 385 → 386 | 30,578 → 29,010 | 39,532 → 37,964 | 0 → 0 | 6.30 → 6.28 |
| numeric_loop | 38.30 → 38.42 | +0.3% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.55 → 5.53 |
| loop_float | 47.45 → 47.66 | +0.4% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.58 → 5.58 |

### portable, unlimited

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 109.79 → 105.64 | -3.8% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 5.95 → 6.03 |
| array_map_select | 197.76 → 194.36 | -1.7% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 6.06 → 6.14 |
| nested_blocks | 127.02 → 122.74 | -3.4% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 5.92 → 5.97 |
| method_calls | 107.52 → 91.97 | -14.5% | 27 → 24 | 4,853 → 4,520 | 5,023 → 4,690 | 0 → 0 | 5.94 → 5.94 |
| function_calls | 54.57 → 55.63 | +2.0% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 5.69 → 5.72 |
| record_fields | 122.13 → 117.08 | -4.1% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 6.39 → 6.53 |
| record_fields_cap | 131.28 → 125.42 | -4.5% | 907 → 905 | 93,578 → 91,866 | 100,870 → 99,094 | 0 → 0 | 6.64 → 6.67 |
| record_update | 299.50 → 297.30 | -0.7% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 5.75 → 5.75 |
| record_build | 342.65 → 330.88 | -3.4% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 6.39 → 6.36 |
| record_build_cap | 357.09 → 338.72 | -5.1% | 2,142 → 2,143 | 253,866 → 252,298 | 234,966 → 233,398 | 0 → 0 | 6.58 → 6.56 |
| records_retained | 117.16 → 116.95 | -0.2% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 6.50 → 6.52 |
| records_retained_cap | 119.94 → 119.71 | -0.2% | 1,162 → 1,162 | 132,890 → 132,890 | 122,182 → 122,182 | 115,188 → 115,188 | 6.80 → 6.66 |
| glue_orders | 216.99 → 206.87 | -4.7% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 6.72 → 6.92 |
| glue_orders_cap | 258.36 → 249.51 | -3.4% | 2,813 → 2,873 | 249,262 → 278,134 | 237,242 → 234,674 | 4,192 → 4,192 | 7.06 → 7.05 |
| json_transform_cap | 39.01 → 38.02 | -2.5% | 480 → 479 | 44,409 → 44,353 | 43,853 → 43,853 | 128 → 128 | 6.50 → 6.41 |
| json_object_512_cap | 76.60 → 76.78 | +0.2% | 1,687 → 1,687 | 111,690 → 111,690 | 92,384 → 92,384 | 76,984 → 76,984 | 6.67 → 6.56 |
| hash_lookup_cap | 106.75 → 105.39 | -1.3% | 193 → 193 | 16,442 → 16,442 | 16,002 → 16,002 | 0 → 0 | 6.12 → 6.14 |
| member_calls_cap | 59.74 → 58.22 | -2.5% | 385 → 386 | 30,578 → 29,010 | 39,532 → 37,964 | 0 → 0 | 6.31 → 6.30 |
| numeric_loop | 37.77 → 37.97 | +0.5% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.56 → 5.56 |
| loop_float | 46.53 → 47.17 | +1.4% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 5.56 → 5.53 |

Cases above +3%: [["upcase_65536/metered", "simd", 4.22], ["upcase_65536/unlimited", "simd", 4.49], ["upstream_greeting/metered", "simd", 4.05]] 

## shannon-operators: Intel(R) Core(TM) Ultra 9 285H

52da6c2ceb62bb12b3d9215885d0dc06709f28e0 versus 3161370a4e20c0f08d286dccd95ce8f5d0bbd10a; 8 rounds, 150.0 ms target per variant and case.

### simd, metered

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 156.08 → 150.05 | -3.9% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 13.48 → 13.48 |
| array_map_select | 299.33 → 296.79 | -0.8% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 13.48 → 13.47 |
| nested_blocks | 173.71 → 166.69 | -4.0% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 13.48 → 13.48 |
| method_calls | 159.07 → 140.18 | -11.9% | 26 → 23 | 4,781 → 4,448 | 5,015 → 4,682 | 0 → 0 | 13.44 → 13.48 |
| function_calls | 70.55 → 71.27 | +1.0% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 13.38 → 13.48 |
| record_fields | 204.24 → 190.41 | -6.8% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 13.48 → 13.47 |
| record_fields_cap | 213.72 → 199.92 | -6.5% | 906 → 904 | 93,514 → 91,802 | 100,862 → 99,086 | 0 → 0 | 13.48 → 13.48 |
| record_update | 395.95 → 403.20 | +1.8% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 13.47 → 13.47 |
| record_build | 550.27 → 521.08 | -5.3% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 13.37 → 13.48 |
| record_build_cap | 560.79 → 528.16 | -5.8% | 2,141 → 2,142 | 253,802 → 252,234 | 234,958 → 233,390 | 0 → 0 | 13.48 → 13.48 |
| records_retained | 178.99 → 177.76 | -0.7% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 13.48 → 13.47 |
| records_retained_cap | 183.54 → 183.75 | +0.1% | 1,161 → 1,161 | 132,826 → 132,826 | 122,174 → 122,174 | 115,188 → 115,188 | 13.45 → 13.48 |
| glue_orders | 304.67 → 285.03 | -6.4% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 13.48 → 13.47 |
| glue_orders_cap | 365.64 → 346.50 | -5.2% | 2,812 → 2,872 | 249,198 → 278,070 | 237,234 → 234,666 | 4,192 → 4,192 | 13.48 → 13.48 |
| json_transform_cap | 54.10 → 52.61 | -2.8% | 479 → 478 | 44,345 → 44,289 | 43,845 → 43,845 | 128 → 128 | 13.48 → 13.48 |
| json_object_512_cap | 106.35 → 106.43 | +0.1% | 1,686 → 1,686 | 111,626 → 111,626 | 92,376 → 92,376 | 76,984 → 76,984 | 13.48 → 13.44 |
| hash_lookup_cap | 160.09 → 160.75 | +0.4% | 192 → 192 | 16,378 → 16,378 | 15,994 → 15,994 | 0 → 0 | 13.46 → 13.47 |
| member_calls_cap | 102.22 → 98.95 | -3.2% | 384 → 385 | 30,514 → 28,946 | 39,524 → 37,956 | 0 → 0 | 13.48 → 13.48 |
| numeric_loop | 62.28 → 62.21 | -0.1% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.48 |
| loop_float | 82.10 → 80.28 | -2.2% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.47 → 13.48 |

### simd, unlimited

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 155.98 → 150.04 | -3.8% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 13.47 → 13.48 |
| array_map_select | 295.92 → 295.44 | -0.2% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 13.46 → 13.48 |
| nested_blocks | 172.99 → 167.18 | -3.4% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 13.48 → 13.48 |
| method_calls | 159.03 → 139.20 | -12.5% | 26 → 23 | 4,781 → 4,448 | 5,015 → 4,682 | 0 → 0 | 13.48 → 13.49 |
| function_calls | 70.62 → 70.92 | +0.4% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 13.48 → 13.45 |
| record_fields | 206.15 → 189.97 | -7.8% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 13.48 → 13.48 |
| record_fields_cap | 213.23 → 199.26 | -6.6% | 906 → 904 | 93,514 → 91,802 | 100,862 → 99,086 | 0 → 0 | 13.48 → 13.37 |
| record_update | 398.36 → 395.99 | -0.6% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 13.47 → 13.46 |
| record_build | 550.26 → 520.53 | -5.4% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 13.48 → 13.48 |
| record_build_cap | 559.30 → 528.18 | -5.6% | 2,141 → 2,142 | 253,802 → 252,234 | 234,958 → 233,390 | 0 → 0 | 13.46 → 13.46 |
| records_retained | 180.03 → 177.84 | -1.2% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 13.47 → 13.46 |
| records_retained_cap | 183.34 → 183.42 | +0.0% | 1,161 → 1,161 | 132,826 → 132,826 | 122,174 → 122,174 | 115,188 → 115,188 | 13.44 → 13.48 |
| glue_orders | 303.88 → 284.75 | -6.3% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 13.48 → 13.47 |
| glue_orders_cap | 365.17 → 345.99 | -5.3% | 2,812 → 2,872 | 249,198 → 278,070 | 237,234 → 234,666 | 4,192 → 4,192 | 13.48 → 13.48 |
| json_transform_cap | 54.19 → 52.46 | -3.2% | 479 → 478 | 44,345 → 44,289 | 43,845 → 43,845 | 128 → 128 | 13.48 → 13.37 |
| json_object_512_cap | 105.53 → 105.87 | +0.3% | 1,686 → 1,686 | 111,626 → 111,626 | 92,376 → 92,376 | 76,984 → 76,984 | 13.48 → 13.48 |
| hash_lookup_cap | 159.85 → 160.66 | +0.5% | 192 → 192 | 16,378 → 16,378 | 15,994 → 15,994 | 0 → 0 | 13.48 → 13.48 |
| member_calls_cap | 102.25 → 98.33 | -3.8% | 384 → 385 | 30,514 → 28,946 | 39,524 → 37,956 | 0 → 0 | 13.48 → 13.47 |
| numeric_loop | 62.17 → 61.86 | -0.5% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.37 → 13.47 |
| loop_float | 80.01 → 80.12 | +0.1% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.37 → 13.47 |

### portable, metered

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 155.80 → 151.01 | -3.1% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 13.47 → 13.48 |
| array_map_select | 296.74 → 295.54 | -0.4% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 13.47 → 13.48 |
| nested_blocks | 173.92 → 167.89 | -3.5% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 13.47 → 13.45 |
| method_calls | 159.11 → 140.12 | -11.9% | 26 → 23 | 4,781 → 4,448 | 5,015 → 4,682 | 0 → 0 | 13.48 → 13.48 |
| function_calls | 70.51 → 71.17 | +0.9% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 13.37 → 13.48 |
| record_fields | 203.65 → 190.35 | -6.5% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 13.48 → 13.46 |
| record_fields_cap | 213.17 → 200.13 | -6.1% | 906 → 904 | 93,514 → 91,802 | 100,862 → 99,086 | 0 → 0 | 13.48 → 13.47 |
| record_update | 396.66 → 393.72 | -0.7% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 13.47 → 13.48 |
| record_build | 553.57 → 521.46 | -5.8% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 13.48 → 13.48 |
| record_build_cap | 562.12 → 531.31 | -5.5% | 2,141 → 2,142 | 253,802 → 252,234 | 234,958 → 233,390 | 0 → 0 | 13.37 → 13.48 |
| records_retained | 178.76 → 177.84 | -0.5% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 13.46 → 13.47 |
| records_retained_cap | 185.52 → 184.40 | -0.6% | 1,161 → 1,161 | 132,826 → 132,826 | 122,174 → 122,174 | 115,188 → 115,188 | 13.48 → 13.48 |
| glue_orders | 308.72 → 289.31 | -6.3% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 13.48 → 13.48 |
| glue_orders_cap | 371.03 → 349.94 | -5.7% | 2,812 → 2,872 | 249,198 → 278,070 | 237,234 → 234,666 | 4,192 → 4,192 | 13.48 → 13.46 |
| json_transform_cap | 55.52 → 54.49 | -1.9% | 479 → 478 | 44,345 → 44,289 | 43,845 → 43,845 | 128 → 128 | 13.37 → 13.45 |
| json_object_512_cap | 106.33 → 106.94 | +0.6% | 1,686 → 1,686 | 111,626 → 111,626 | 92,376 → 92,376 | 76,984 → 76,984 | 13.48 → 13.46 |
| hash_lookup_cap | 160.53 → 161.59 | +0.7% | 192 → 192 | 16,378 → 16,378 | 15,994 → 15,994 | 0 → 0 | 13.48 → 13.46 |
| member_calls_cap | 102.66 → 99.38 | -3.2% | 384 → 385 | 30,514 → 28,946 | 39,524 → 37,956 | 0 → 0 | 13.44 → 13.46 |
| numeric_loop | 61.84 → 62.17 | +0.5% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.46 |
| loop_float | 80.23 → 80.16 | -0.1% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.46 |

### portable, unlimited

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| array_each | 154.65 → 150.78 | -2.5% | 14 → 15 | 20,568 → 19,000 | 20,008 → 18,440 | 0 → 0 | 13.47 → 13.47 |
| array_map_select | 295.60 → 294.95 | -0.2% | 31 → 33 | 69,608 → 68,520 | 48,712 → 47,144 | 0 → 0 | 13.49 → 13.47 |
| nested_blocks | 172.08 → 166.73 | -3.1% | 16 → 49 | 6,280 → 20,072 | 5,624 → 4,536 | 0 → 0 | 13.46 → 13.48 |
| method_calls | 159.38 → 139.38 | -12.5% | 26 → 23 | 4,781 → 4,448 | 5,015 → 4,682 | 0 → 0 | 13.46 → 13.44 |
| function_calls | 70.22 → 70.76 | +0.8% | 10 → 10 | 1,592 → 1,592 | 1,544 → 1,544 | 0 → 0 | 13.47 → 13.48 |
| record_fields | 204.19 → 189.89 | -7.0% | 794 → 792 | 86,056 → 84,344 | 96,044 → 94,268 | 0 → 0 | 13.48 → 13.48 |
| record_fields_cap | 213.10 → 200.03 | -6.1% | 906 → 904 | 93,514 → 91,802 | 100,862 → 99,086 | 0 → 0 | 13.48 → 13.48 |
| record_update | 401.09 → 392.26 | -2.2% | 2,015 → 2,015 | 34,472 → 34,472 | 2,530 → 2,530 | 0 → 0 | 13.48 → 13.49 |
| record_build | 551.86 → 519.62 | -5.8% | 2,030 → 2,031 | 246,376 → 244,808 | 230,140 → 228,572 | 0 → 0 | 13.46 → 13.47 |
| record_build_cap | 562.02 → 528.36 | -6.0% | 2,141 → 2,142 | 253,802 → 252,234 | 234,958 → 233,390 | 0 → 0 | 13.48 → 13.47 |
| records_retained | 178.96 → 177.22 | -1.0% | 1,050 → 1,050 | 125,400 → 125,400 | 117,356 → 117,356 | 115,188 → 115,188 | 13.47 → 13.46 |
| records_retained_cap | 185.09 → 184.03 | -0.6% | 1,161 → 1,161 | 132,826 → 132,826 | 122,174 → 122,174 | 115,188 → 115,188 | 13.37 → 13.48 |
| glue_orders | 307.91 → 288.46 | -6.3% | 2,509 → 2,569 | 221,804 → 250,676 | 232,416 → 229,848 | 4,192 → 4,192 | 13.47 → 13.48 |
| glue_orders_cap | 370.01 → 348.62 | -5.8% | 2,812 → 2,872 | 249,198 → 278,070 | 237,234 → 234,666 | 4,192 → 4,192 | 13.49 → 13.37 |
| json_transform_cap | 55.51 → 54.44 | -1.9% | 479 → 478 | 44,345 → 44,289 | 43,845 → 43,845 | 128 → 128 | 13.48 → 13.44 |
| json_object_512_cap | 105.57 → 106.37 | +0.8% | 1,686 → 1,686 | 111,626 → 111,626 | 92,376 → 92,376 | 76,984 → 76,984 | 13.48 → 13.48 |
| hash_lookup_cap | 160.46 → 160.97 | +0.3% | 192 → 192 | 16,378 → 16,378 | 15,994 → 15,994 | 0 → 0 | 13.45 → 13.37 |
| member_calls_cap | 102.56 → 98.82 | -3.7% | 384 → 385 | 30,514 → 28,946 | 39,524 → 37,956 | 0 → 0 | 13.47 → 13.48 |
| numeric_loop | 61.77 → 61.82 | +0.1% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.47 → 13.48 |
| loop_float | 79.86 → 80.08 | +0.3% | 9 → 9 | 1,512 → 1,512 | 1,464 → 1,464 | 0 → 0 | 13.48 → 13.46 |

Cases above +3%: [["json_stringify_escaped_4k/metered", "simd", 11.58], ["json_stringify_escaped_4k/metered", "portable", 9.45], ["json_stringify_escaped_4k/unlimited", "simd", 13.55], ["json_stringify_escaped_4k/unlimited", "portable", 9.05]] 

