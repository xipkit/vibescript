# First Rust performance iteration

Measured on Apple M4 with rustc 1.98.1 (48a229cea 2026-09-01) (Homebrew) and go version go1.27.1 darwin/arm64. Original Rust binaries are from `8403e83c4760cee2601b229cd4a94c97777414a1`; the updated runtime and harness are from `f86f4bcf123005008a3118662ae8b41fc598edf5`. Go remains Vibescript v0.70.0. The original binaries were rerun alongside the new builds, using the same fixtures and iteration counts.

## Results

Array growth is 4.1× faster and allocates 81.9× fewer bytes. The unchanged upstream countdown example also benefits from reusing storage for `out = out + [n]`. Reading a 64 KiB string's length now allocates 760 bytes per call instead of 66,320, while the execution budget still charges its retained backing capacity.

Unicode JSON parsing and stringification are 4.7× and 4.8× faster than the initial Rust implementation. ASCII JSON parsing allocates 66,929 bytes per call versus 193,902 before. The integer-loop timing remains 65.12 µs versus Go SIMD's 627.16 µs.

All times below are medians of 12 samples with accounting enabled. Allocation bytes are cumulative requested allocation volume per call, not retained memory or RSS.

| Workload | Rust before µs | Rust after µs | Go SIMD µs | Rust B/call before → after |
| --- | ---: | ---: | ---: | ---: |
| `array_growth` | 50.335 | 12.209 | 151.853 | 401,216 → 4,896 |
| `upstream_countdown` | 16.101 | 7.402 | 89.915 | 68,432 → 7,648 |
| `length_65536` | 2.905 | 1.548 | 3.936 | 66,320 → 760 |
| `length_unicode` | 84.181 | 35.016 | 22.426 | 37,648 → 760 |
| `json_parse_ascii_64k` | 7.196 | 4.189 | 10.717 | 193,902 → 66,929 |
| `json_stringify_ascii_64k` | 7.376 | 5.129 | 16.977 | 194,334 → 66,855 |
| `json_parse_unicode_4k` | 20.895 | 4.440 | 7.402 | 15,731 → 5,497 |
| `json_stringify_unicode_4k` | 21.617 | 4.542 | 7.740 | 21,657 → 5,423 |

## Remaining tradeoffs

- Unicode length improved by 2.4× but still takes 1.56× Go's time. Counting and validating UTF-8 is still a target for further work.
- Escape-heavy JSON stringification with accounting disabled takes 1.61× Go's time. Small escape operations still enter the accounting helpers frequently.
- The metered JSON transformation workload changed by +7.6% versus the initial Rust implementation; it remains 4.7× faster than Go SIMD. Storage metadata and short-string processing need further profiling before attributing this difference to a specific cause.
- With explicit SIMD disabled, escape-heavy JSON parsing changed by +10.4% versus the original portable Rust build. The default SIMD build changed by -1.1% on the same metered case.
- Hash lookup and duplicate-key replacement still scan ordered entries linearly. The 64-key benchmark does not establish large-object scalability. Adding an index while preserving order and accounting remains a structural follow-up.
- Distinct imports of the same foreign byte buffer conservatively charge separate views; cloning an already imported value shares its charge. The memory model remains different from Go's reachable-graph estimator.
- This is still a partial language implementation. Full Unicode case mapping, bignums, modules, classes, blocks, and suspended async host calls remain outside the core. x86_64 SIMD was not compiled or measured on this ARM64 machine.

## What changed

Array writes reuse uniquely owned storage and copy when aliases exist. The VM releases the overwritten local only after argument evaluation, and additive assignment combines execution and writeback. Array depth is maintained incrementally, with rescans when replacing a deepest child could reduce the depth.

Immutable byte storage is shared across calls with independent memory charges. The charge includes the full backing capacity and headers; it does not disappear because the bytes originated in the host. Returned JSON strings and character slices own their storage so small results do not retain large source documents.

Unicode scans validate sequence widths and batch accounting between bounded spans. JSON copies valid spans together, reserves unescaped strings once, and leaves room for closing delimiters after large string values. Short escape paths avoid speculative vector scans and repeated checks of the same delimiter.

## Validation and method

- 162 shared cases match independently computed expected outputs in all six builds, including 46 calls from ten unchanged upstream files. New cases cover self-aliasing arrays, argument evaluation, additive assignments, malformed UTF-8, and vector/chunk boundaries.
- The UTF-8 decoder is checked against every Unicode scalar value and combinations of invalid leading, continuation, and truncated bytes. Tests also cover quota exhaustion during Unicode work, spare input capacity, cross-call charge lifetime, array depth changes, and reclamation of large JSON sources.
- Portable and SIMD builds report identical accounting within each Rust revision. Step counts can change across revisions because instruction fusion and removal of copying change the work performed. Quotas, recursion limits, cancellation, and latched exhaustion remain enabled and tested.
- Formatting, Clippy with warnings denied, debug/release tests, portable/SIMD tests, Go vet, and the Tokio cancellation/worker-permit tests pass. The final verification artifact records CLI and provenance checks.
- 24 workloads run with accounting enabled and disabled. Six builds rotate through all positions twice over 12 rounds. Pilot measurements choose a common iteration count per case. Compilation, setup, and serialization are outside timing; argument import and execution are inside. Final timed outputs are checked.
- Timing uses uninstrumented Rust binaries. Separate binaries count allocations with a system-allocator wrapper. Go uses runtime.MemStats; its size-class accounting is not identical to Rust's requested-layout accounting. Process peak RSS includes the complete harness and fixture, and is a coarse single-process measurement.

| Variant | Peak RSS MiB |
| --- | ---: |
| go-portable | 14.34 |
| go-simd | 14.75 |
| rust-before-portable | 5.20 |
| rust-before-simd | 5.20 |
| rust-portable | 4.94 |
| rust-simd | 4.91 |

## Reproduce

Build the initial Rust comparison binaries from commit `8403e83c4760cee2601b229cd4a94c97777414a1` using its `scripts/compare.py --validate-only`. Preserve the four `rust-*` timing/allocation binaries together with a `revision` file containing that full commit hash. Keep the checkout, caches, and binaries on the external volume. Then run:

```sh
./scripts/check
python3 scripts/compare.py --baseline /path/to/preserved/binaries --rounds 12
python3 scripts/report-followup.py benchmarks/results/<run-directory>
```

[Raw results](results/2026-09-12-performance/summary.json), [environment and binary hashes](results/2026-09-12-performance/environment.json), [test log](results/2026-09-12-performance/validation.log), and [final verification](results/2026-09-12-performance/verification.json) preserve the evidence. The original timing binary hashes match the [initial report's manifest](results/2026-09-12-m4/environment.json).

## Complete timings: metered

Median µs/call; lower is better.

| Workload | Go portable | Go SIMD | Rust before portable | Rust before SIMD | Rust after portable | Rust after SIMD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `numeric_loop` | 632.740 | 627.164 | 64.968 | 64.751 | 65.715 | 65.121 |
| `function_calls` | 673.331 | 677.654 | 69.117 | 68.350 | 67.405 | 67.354 |
| `array_sum` | 58.034 | 58.452 | 13.004 | 12.998 | 13.391 | 13.553 |
| `array_growth` | 153.372 | 151.853 | 49.834 | 50.335 | 12.193 | 12.209 |
| `hash_lookup` | 901.123 | 882.047 | 382.771 | 388.228 | 364.282 | 366.189 |
| `length_16` | 1.316 | 1.308 | 0.198 | 0.192 | 0.170 | 0.168 |
| `upcase_16` | 1.760 | 1.739 | 0.288 | 0.286 | 0.266 | 0.263 |
| `length_4096` | 1.677 | 1.504 | 1.317 | 0.339 | 1.224 | 0.255 |
| `upcase_4096` | 2.955 | 2.676 | 0.477 | 0.503 | 0.406 | 0.407 |
| `length_65536` | 6.648 | 3.936 | 18.200 | 2.905 | 16.819 | 1.548 |
| `upcase_65536` | 16.647 | 16.806 | 4.264 | 4.676 | 3.104 | 3.274 |
| `strip_65536` | 6.007 | 5.895 | 3.227 | 3.162 | 1.567 | 1.594 |
| `length_unicode` | 23.206 | 22.426 | 78.265 | 84.181 | 34.857 | 35.016 |
| `length_mixed_65536` | 22.933 | 20.319 | 18.224 | 2.937 | 16.847 | 1.563 |
| `json_parse_ascii_64k` | 41.249 | 10.717 | 37.169 | 7.196 | 34.759 | 4.189 |
| `json_stringify_ascii_64k` | 63.278 | 16.977 | 31.051 | 7.376 | 28.561 | 5.129 |
| `json_parse_escaped_4k` | 22.705 | 22.597 | 19.074 | 21.534 | 21.062 | 21.304 |
| `json_stringify_escaped_4k` | 99.528 | 99.046 | 39.262 | 40.474 | 39.031 | 39.483 |
| `json_parse_unicode_4k` | 7.582 | 7.402 | 20.002 | 20.895 | 4.493 | 4.440 |
| `json_stringify_unicode_4k` | 7.601 | 7.740 | 21.001 | 21.617 | 4.498 | 4.542 |
| `json_transform` | 182.183 | 183.020 | 35.348 | 36.021 | 38.778 | 38.770 |
| `upstream_fibonacci` | 479.232 | 473.298 | 42.043 | 42.047 | 42.439 | 42.039 |
| `upstream_countdown` | 90.028 | 89.915 | 15.760 | 16.101 | 7.580 | 7.402 |
| `upstream_greeting` | 3.430 | 3.407 | 0.612 | 0.619 | 0.608 | 0.605 |

## Complete timings: unlimited

Median µs/call; lower is better.

| Workload | Go portable | Go SIMD | Rust before portable | Rust before SIMD | Rust after portable | Rust after SIMD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `numeric_loop` | 303.134 | 302.676 | 64.512 | 64.952 | 65.182 | 65.464 |
| `function_calls` | 301.013 | 298.567 | 68.989 | 69.599 | 66.964 | 67.218 |
| `array_sum` | 14.472 | 14.398 | 13.004 | 12.907 | 13.514 | 13.455 |
| `array_growth` | 62.084 | 62.271 | 49.147 | 49.695 | 12.098 | 12.145 |
| `hash_lookup` | 369.047 | 368.505 | 382.811 | 383.778 | 338.374 | 337.495 |
| `length_16` | 0.743 | 0.740 | 0.189 | 0.181 | 0.144 | 0.142 |
| `upcase_16` | 0.834 | 0.831 | 0.272 | 0.270 | 0.205 | 0.207 |
| `length_4096` | 1.070 | 0.896 | 1.299 | 0.338 | 1.194 | 0.232 |
| `upcase_4096` | 1.973 | 1.684 | 0.456 | 0.474 | 0.361 | 0.364 |
| `length_65536` | 5.906 | 3.296 | 18.162 | 2.859 | 16.768 | 1.522 |
| `upcase_65536` | 15.656 | 15.607 | 4.228 | 4.650 | 3.041 | 3.212 |
| `strip_65536` | 5.131 | 5.077 | 3.166 | 3.096 | 1.525 | 1.565 |
| `length_unicode` | 21.976 | 21.891 | 77.693 | 83.060 | 34.782 | 34.766 |
| `length_mixed_65536` | 22.378 | 19.701 | 18.276 | 2.885 | 16.817 | 1.520 |
| `json_parse_ascii_64k` | 38.995 | 8.444 | 36.981 | 7.081 | 34.742 | 4.150 |
| `json_stringify_ascii_64k` | 61.142 | 15.176 | 30.939 | 7.334 | 28.493 | 5.059 |
| `json_parse_escaped_4k` | 19.665 | 19.984 | 19.143 | 21.941 | 21.045 | 21.429 |
| `json_stringify_escaped_4k` | 25.562 | 24.812 | 39.675 | 40.879 | 39.015 | 39.886 |
| `json_parse_unicode_4k` | 5.334 | 5.317 | 19.917 | 20.892 | 4.461 | 4.406 |
| `json_stringify_unicode_4k` | 5.776 | 5.848 | 20.917 | 21.792 | 4.478 | 4.453 |
| `json_transform` | 69.078 | 70.277 | 34.441 | 35.139 | 36.585 | 36.978 |
| `upstream_fibonacci` | 173.071 | 171.927 | 41.711 | 41.698 | 41.636 | 41.634 |
| `upstream_countdown` | 18.251 | 18.355 | 15.567 | 15.937 | 7.267 | 7.216 |
| `upstream_greeting` | 1.421 | 1.421 | 0.601 | 0.635 | 0.492 | 0.507 |

## Complete metered allocations

| Workload | Go B/call | Rust before B/call | Rust after B/call | Go allocations | Rust before allocations | Rust after allocations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `numeric_loop` | 67,552 | 736 | 736 | 2013.0 | 5.0 | 5.0 |
| `function_calls` | 20,368 | 16,736 | 16,736 | 524.0 | 505.0 | 505.0 |
| `array_sum` | 36,512 | 16,784 | 16,784 | 16.0 | 7.0 | 7.0 |
| `array_growth` | 25,424 | 401,216 | 4,896 | 408.0 | 389.0 | 14.0 |
| `hash_lookup` | 93,368 | 91,176 | 62,448 | 3028.0 | 2135.0 | 1071.0 |
| `length_16` | 3,504 | 800 | 760 | 11.0 | 7.0 | 6.0 |
| `upcase_16` | 3,584 | 981 | 928 | 15.0 | 11.0 | 10.0 |
| `length_4096` | 3,504 | 4,880 | 760 | 11.0 | 7.0 | 6.0 |
| `upcase_4096` | 11,760 | 9,141 | 5,008 | 16.0 | 11.0 | 10.0 |
| `length_65536` | 3,504 | 66,320 | 760 | 11.0 | 7.0 | 6.0 |
| `upcase_65536` | 134,640 | 132,021 | 66,448 | 16.0 | 11.0 | 10.0 |
| `strip_65536` | 69,072 | 131,940 | 66,392 | 14.0 | 9.0 | 9.0 |
| `length_unicode` | 3,504 | 37,648 | 760 | 11.0 | 7.0 | 6.0 |
| `length_mixed_65536` | 3,504 | 66,321 | 760 | 11.0 | 7.0 | 6.0 |
| `json_parse_ascii_64k` | 71,080 | 193,902 | 66,929 | 37.0 | 19.0 | 17.0 |
| `json_stringify_ascii_64k` | 153,136 | 194,334 | 66,855 | 34.0 | 22.0 | 16.0 |
| `json_parse_escaped_4k` | 15,272 | 25,147 | 17,770 | 38.0 | 26.0 | 28.0 |
| `json_stringify_escaped_4k` | 47,664 | 21,653 | 13,915 | 42.0 | 25.0 | 17.0 |
| `json_parse_unicode_4k` | 10,408 | 15,731 | 5,497 | 37.0 | 26.0 | 17.0 |
| `json_stringify_unicode_4k` | 15,408 | 21,657 | 5,423 | 34.0 | 25.0 | 16.0 |
| `json_transform` | 83,816 | 60,975 | 60,678 | 1187.0 | 918.0 | 1172.0 |
| `upstream_fibonacci` | 13,448 | 9,152 | 9,152 | 69.0 | 470.0 | 470.0 |
| `upstream_countdown` | 13,648 | 68,432 | 7,648 | 179.0 | 255.0 | 113.0 |
| `upstream_greeting` | 4,256 | 1,366 | 1,307 | 30.0 | 23.0 | 22.0 |
