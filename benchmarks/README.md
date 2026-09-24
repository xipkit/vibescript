# Rust core performance comparison

Measured on Apple M4 (macOS-26.5.2-arm64-arm-64bit-Mach-O) with `rustc 1.98.1 (48a229cea 2026-09-01) (Homebrew)` and `go version go1.27.1 darwin/arm64`. Go uses Vibescript v0.70.0; the Rust implementation and harness are pinned at `8403e83c4760cee2601b229cd4a94c97777414a1`. [Raw results and environment](results/2026-09-12-m4/environment.json).

## Findings

The Rust bytecode core is faster on the integer loop (9.8×) and the unchanged upstream Fibonacci example (11.8×) with accounting enabled. This compares a small bytecode VM with the full Go interpreter, so the result includes architecture, feature coverage, and accounting differences. It does not isolate the effect of the implementation language.

For a 64 KiB ASCII payload, Rust with SIMD parses JSON in 7.26 µs versus Go with SIMD at 10.64 µs, and stringifies it in 7.36 µs versus 17.02 µs. Within Rust, enabling the explicit SIMD scanners improves these calls by 5.1× and 4.2×. Go's SIMD experiment improves the same calls by 3.9× and 3.7×.

Unicode is a weakness of this first Rust decoder: Unicode length is 3.7× slower than Go with SIMD, and Unicode JSON parsing/stringification are 2.8×/2.8× slower. Explicit SIMD case conversion also takes 1.11× the time of the portable Rust loop on the 64 KiB upcase case. There is no general benefit from manually vectorizing every operation; the portable build still permits LLVM auto-vectorization.

Memory results are mixed. The Rust integer loop allocates 736 bytes per call versus Go's 67,552. However, repeated array growth allocates 401,216 versus 25,424 bytes (15.8× more), and ASCII JSON parsing allocates 2.7× more. Fast execution alone would conceal these costs.

## Selected calls with accounting enabled

Median of 8 samples. Lower time and allocation volume are better. Bytes are cumulative allocation volume per call, not retained heap or RSS.

| Workload | Go SIMD µs | Rust SIMD µs | Go B/call | Rust B/call |
| --- | ---: | ---: | ---: | ---: |
| `numeric_loop` | 633.53 | 64.83 | 67,552 | 736 |
| `array_growth` | 152.77 | 49.80 | 25,424 | 401,216 |
| `length_65536` | 3.91 | 2.89 | 3,504 | 66,320 |
| `length_unicode` | 22.95 | 85.11 | 3,504 | 37,648 |
| `upcase_65536` | 16.79 | 4.70 | 134,640 | 132,021 |
| `json_parse_ascii_64k` | 10.64 | 7.26 | 71,081 | 193,902 |
| `json_stringify_ascii_64k` | 17.02 | 7.36 | 153,136 | 194,334 |
| `json_parse_unicode_4k` | 7.49 | 20.97 | 10,408 | 15,731 |
| `json_transform` | 185.35 | 35.91 | 83,816 | 60,975 |
| `upstream_fibonacci` | 497.50 | 42.15 | 13,448 | 9,152 |
| `upstream_countdown` | 89.15 | 16.02 | 13,648 | 68,432 |

## Process memory

One separate process per variant runs the entire 48-case measurement fixture for 100 calls per case. These are macOS maximum resident set sizes from `/usr/bin/time -l`, including runtime, fixture decoding, compiled code, and harness overhead. They are coarse process measurements, not per-call memory limits or a language-wide memory comparison.

| Variant | Peak RSS MiB |
| --- | ---: |
| go-portable | 14.72 |
| go-simd | 14.58 |
| rust-portable | 5.30 |
| rust-simd | 5.41 |

## Method and validation

- Four builds on the same native ARM64 machine: Go 1.27.1 without the SIMD experiment, Go 1.27.1 with `GOEXPERIMENT=simd`, Rust portable scanners, and Rust explicit NEON scanners. Rust uses release optimization, thin LTO, and one codegen unit. No `target-cpu=native` override was added. x86_64 SSE2 code is present but was not compiled or measured on this machine.
- 145 shared invocations match independently computed expected results on all four builds. These include 46 invocations of ten complete, unchanged upstream `.vibe` files. SHA-256 hashes pin those files. Three original examples also appear in the benchmark suite.
- 24 workloads run with accounting enabled and disabled, for 48 measured cases. Enabled limits are five million steps, 64 MiB, and 256 frames; disabled runs retain a recursion limit and cooperative cancellation. Rust and Go charge different units and use different memory models, so equal numeric limits are not equivalent sandbox policies.
- Compilation, fixture decoding, argument construction, output encoding, and validation are outside the timer. Per-call argument import, execution, result construction, and normal per-call runtime work are included. Both harnesses retain the last result, and verify it against the initial result after timing. Results are also checked against expected outputs before measurement.
- Pilot runs select one iteration count shared by all variants for each case. Eight rounds rotate execution order so every build occupies every position twice. Go uses `GOMAXPROCS=1`; Rust executes one synchronous call at a time. The raw samples preserve min/max and run order; small percentage differences should be treated cautiously.
- Rust allocation measurement uses separate instrumented binaries wrapping the system allocator; its timing numbers are not used in the timing tables. Rust counts requested layout bytes and treats reallocation as another allocation. Go uses `runtime.MemStats.TotalAlloc` and `Mallocs`, including its size-class behavior. These are useful allocation-volume comparisons but not identical physical-memory accounting.
- Rust SIMD and portable builds report identical steps, peak tracked capacity, and retained tracked capacity on all shared invocations. Separate tests cover step/memory/recursion/deadline exhaustion, cancellation before argument import and during execution, ignored host quota errors, temporary/frame reclamation, JSON source retention, and Tokio worker-permit lifetime after cancellation.
- Debug/release tests, portable/SIMD tests, Clippy with warnings denied, formatting, Go vet, and CLI examples pass. This remains a partial interpreter: classes, blocks, typing, bignums, modules, most standard-library methods, and native async host callbacks are outside the implemented core.

## Next performance work

1. Share immutable host byte buffers while charging their retained storage. The current argument importer copies strings; even a length call allocates a full input copy, while Go shares string storage.
2. Reuse unshared array storage for updates with copy-on-write behavior when aliases exist. The current immutable implementation copies arrays on every push or indexed write, which explains the array-growth allocation volume and quadratic copying.
3. Improve valid-Unicode scans and JSON span handling while preserving invalid-byte behavior and chunk checkpoints. The current decoder repeatedly classifies and validates individual runes.
4. Inspect code generation before adding more explicit SIMD case-conversion paths. Keep the portable control: the existing Rust loop is already competitive with the manually vectorized implementation on this machine.

These are measured follow-up targets, not claims that a full Rust port will retain the same advantages. [Rust SIMD documentation](https://doc.rust-lang.org/std/arch/index.html) explains the distinction between explicit intrinsics and compiler auto-vectorization; [Go build documentation](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/building.md) describes the optional experiment.

## Reproduce

```sh
./scripts/check
python3 scripts/compare.py --with-go --rounds 8
python3 scripts/report.py benchmarks/results/<run-directory>
```

The recorded run is in [`results/2026-09-12-m4`](results/2026-09-12-m4/summary.json). Build hashes and module provenance are in `environment.json`; `validation-*.jsonl`, `round-*.jsonl`, `allocations-*.jsonl`, and `rss-*.txt` preserve the underlying evidence. The [final audit](results/2026-09-12-m4/verification.json) records source and binary checks, upstream file verification, and CLI results; [the validation log](results/2026-09-12-m4/validation.log) preserves the full local test gate.

## Complete timings: metered

Median µs/call; lower is better.

| Workload | Go portable | Go SIMD | Rust portable | Rust SIMD |
| --- | ---: | ---: | ---: | ---: |
| `numeric_loop` | 608.512 | 633.533 | 64.418 | 64.826 |
| `function_calls` | 687.235 | 678.522 | 69.326 | 69.552 |
| `array_sum` | 58.496 | 58.036 | 13.076 | 12.969 |
| `array_growth` | 149.719 | 152.769 | 49.725 | 49.803 |
| `hash_lookup` | 855.396 | 891.612 | 381.683 | 383.269 |
| `length_16` | 1.314 | 1.317 | 0.201 | 0.185 |
| `upcase_16` | 1.763 | 1.737 | 0.290 | 0.275 |
| `length_4096` | 1.673 | 1.507 | 1.307 | 0.335 |
| `upcase_4096` | 2.919 | 2.650 | 0.470 | 0.489 |
| `length_65536` | 6.525 | 3.913 | 18.169 | 2.889 |
| `upcase_65536` | 16.564 | 16.793 | 4.225 | 4.705 |
| `strip_65536` | 5.851 | 5.935 | 3.092 | 3.172 |
| `length_unicode` | 22.546 | 22.955 | 79.522 | 85.112 |
| `length_mixed_65536` | 23.041 | 20.334 | 18.292 | 2.923 |
| `json_parse_ascii_64k` | 41.174 | 10.638 | 37.160 | 7.261 |
| `json_stringify_ascii_64k` | 62.844 | 17.016 | 31.037 | 7.359 |
| `json_parse_escaped_4k` | 22.686 | 21.958 | 19.006 | 22.263 |
| `json_stringify_escaped_4k` | 100.703 | 99.648 | 38.940 | 40.636 |
| `json_parse_unicode_4k` | 7.498 | 7.488 | 20.162 | 20.970 |
| `json_stringify_unicode_4k` | 7.760 | 7.703 | 21.383 | 21.848 |
| `json_transform` | 184.409 | 185.346 | 35.363 | 35.909 |
| `upstream_fibonacci` | 499.020 | 497.503 | 42.174 | 42.150 |
| `upstream_countdown` | 90.701 | 89.146 | 15.812 | 16.021 |
| `upstream_greeting` | 3.441 | 3.397 | 0.611 | 0.620 |

## Complete timings: unlimited

Median µs/call; lower is better.

| Workload | Go portable | Go SIMD | Rust portable | Rust SIMD |
| --- | ---: | ---: | ---: | ---: |
| `numeric_loop` | 292.373 | 306.204 | 64.280 | 64.384 |
| `function_calls` | 304.235 | 300.547 | 69.248 | 68.988 |
| `array_sum` | 14.289 | 14.411 | 13.100 | 12.932 |
| `array_growth` | 61.082 | 62.653 | 49.118 | 49.121 |
| `hash_lookup` | 347.744 | 368.293 | 378.074 | 379.892 |
| `length_16` | 0.730 | 0.725 | 0.186 | 0.176 |
| `upcase_16` | 0.827 | 0.823 | 0.272 | 0.260 |
| `length_4096` | 1.064 | 0.896 | 1.291 | 0.328 |
| `upcase_4096` | 1.927 | 1.690 | 0.455 | 0.475 |
| `length_65536` | 5.910 | 3.273 | 18.144 | 2.848 |
| `upcase_65536` | 15.521 | 15.604 | 4.204 | 4.653 |
| `strip_65536` | 5.053 | 5.042 | 3.157 | 3.167 |
| `length_unicode` | 21.639 | 22.132 | 79.170 | 84.051 |
| `length_mixed_65536` | 22.511 | 19.750 | 18.159 | 2.910 |
| `json_parse_ascii_64k` | 38.880 | 8.362 | 37.083 | 7.138 |
| `json_stringify_ascii_64k` | 61.402 | 15.015 | 30.894 | 7.298 |
| `json_parse_escaped_4k` | 19.941 | 19.464 | 19.129 | 22.148 |
| `json_stringify_escaped_4k` | 26.087 | 24.805 | 39.166 | 40.686 |
| `json_parse_unicode_4k` | 5.396 | 5.246 | 20.025 | 20.758 |
| `json_stringify_unicode_4k` | 6.011 | 5.743 | 20.954 | 21.970 |
| `json_transform` | 70.148 | 70.841 | 34.608 | 35.068 |
| `upstream_fibonacci` | 174.662 | 172.240 | 41.548 | 41.822 |
| `upstream_countdown` | 18.662 | 18.319 | 15.457 | 15.948 |
| `upstream_greeting` | 1.447 | 1.415 | 0.605 | 0.639 |

## Complete allocations with accounting enabled

| Workload | Go B/call | Rust B/call | Go allocations/call | Rust allocations/call |
| --- | ---: | ---: | ---: | ---: |
| `numeric_loop` | 67,552 | 736 | 2013.0 | 5.0 |
| `function_calls` | 20,368 | 16,736 | 524.0 | 505.0 |
| `array_sum` | 36,512 | 16,784 | 16.0 | 7.0 |
| `array_growth` | 25,424 | 401,216 | 408.0 | 389.0 |
| `hash_lookup` | 93,368 | 91,176 | 3028.0 | 2135.0 |
| `length_16` | 3,504 | 800 | 11.0 | 7.0 |
| `upcase_16` | 3,584 | 981 | 15.0 | 11.0 |
| `length_4096` | 3,504 | 4,880 | 11.0 | 7.0 |
| `upcase_4096` | 11,760 | 9,141 | 16.0 | 11.0 |
| `length_65536` | 3,504 | 66,320 | 11.0 | 7.0 |
| `upcase_65536` | 134,640 | 132,021 | 16.0 | 11.0 |
| `strip_65536` | 69,072 | 131,940 | 14.0 | 9.0 |
| `length_unicode` | 3,504 | 37,648 | 11.0 | 7.0 |
| `length_mixed_65536` | 3,504 | 66,321 | 11.0 | 7.0 |
| `json_parse_ascii_64k` | 71,081 | 193,902 | 37.0 | 19.0 |
| `json_stringify_ascii_64k` | 153,136 | 194,334 | 34.0 | 22.0 |
| `json_parse_escaped_4k` | 15,272 | 25,147 | 38.0 | 26.0 |
| `json_stringify_escaped_4k` | 47,664 | 21,653 | 42.0 | 25.0 |
| `json_parse_unicode_4k` | 10,408 | 15,731 | 37.0 | 26.0 |
| `json_stringify_unicode_4k` | 15,408 | 21,657 | 34.0 | 25.0 |
| `json_transform` | 83,816 | 60,975 | 1187.0 | 918.0 |
| `upstream_fibonacci` | 13,448 | 9,152 | 69.0 | 470.0 |
| `upstream_countdown` | 13,648 | 68,432 | 179.0 | 255.0 |
| `upstream_greeting` | 4,256 | 1,366 | 30.0 | 23.0 |
