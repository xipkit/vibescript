**JSON SIMD is useful for long ASCII string values, with unchanged accounting and allocation counts in this experiment.** On an Apple M4, parsing a 64 KiB ASCII value was about 7.9 times faster and generating it about 4.3 times faster. Parsing ASCII with occasional escapes improved about 5.4 times. Existing small-token JSON benchmarks were unchanged within measurement uncertainty.

This is a reproducible ARM64 experiment based on [`e6f1fca90c012885453a6c413c0826f6abff1f32`](https://github.com/xipkit/vibescript/tree/e6f1fca90c012885453a6c413c0826f6abff1f32), using Go 1.27.1 and `GOEXPERIMENT=simd`. Production source files and `go.mod` are unchanged. Go overlays supply the experimental scanner, its integration, and copies of the original parser/string encoder for differential testing. Go's SIMD APIs remain experimental. [Go 1.27 release notes](https://go.dev/doc/go1.27#simd).

The measurements compare three implementations under the same compiler and experiment flag:

- **Original:** the current runtime.
- **Scalar spans:** ordinary byte scanning plus copying safe ASCII spans during escape decoding, with the same fallback policy as SIMD.
- **SIMD spans:** ARM64 NEON scanning plus that same span-copying integration.

Each main sample is one complete `Script.Call` parsing or generating an object with a string field and an integer field. Step and memory quotas remain enabled: 5,000,000 steps and 64 MiB. Inputs are prepared outside the timed region. Ten repeated trials rotate implementation order, with 100 ms per benchmark per trial. Times are medians per call.

| 64 KiB string workload | Original | Scalar spans | SIMD spans | Overall speedup |
|---|---:|---:|---:|---:|
| Parse ASCII | 83.11 µs | 38.04 µs | 10.50 µs | 7.9× |
| Generate ASCII | 77.88 µs | 60.57 µs | 18.06 µs | 4.3× |
| Parse occasional escapes | 250.53 µs | 81.40 µs | 46.42 µs | 5.4× |
| Generate occasional escapes | 273.57 µs | 257.02 µs | 230.38 µs | 1.2× |

Occasional escapes means one newline after every 63 ASCII letters in the value. JSON input length therefore differs from decoded string length. Dense-escape controls use repeated `a\n\t"\\`; Unicode controls contain valid multibyte characters, and malformed UTF-8 is measured separately.

The scalar comparison matters: span copying alone removes much of escape decoding's per-rune overhead. SIMD adds another 3.6× speedup over scalar spans for plain ASCII parsing, 3.4× for plain ASCII generation, and 1.8× for parsing occasional escapes. These combined gains should not all be attributed to SIMD instructions.

At 4 KiB, ASCII parsing improved from 8.37 to 3.60 µs and generation from 9.12 to 5.46 µs. Small 16-byte values and the existing 80-iteration small-token JSON benchmarks had no statistically significant change. The main matrix's Unicode, dense-escape, HTML-escaping, and malformed-UTF-8 controls also had no statistically significant slowdown in the final SIMD version. All headline time comparisons have p<0.001, n=10; this establishes local benchmark differences, not a general application-wide speedup.

Additional ten-trial controls use an ASCII prefix followed by Unicode: 64 prefix bytes for the 4 KiB/64 KiB cases, and 8 prefix bytes for the 16-byte case. The larger inputs exercise continuation after entering the fast path; the small input exercises entry fallback. SIMD showed no statistically significant slowdown in the larger mixed-text controls. The scalar-only variant did regress 5.4% on the 64 KiB generation case, so it needs separate tuning before adoption as a standalone optimization. [Mixed-text comparison](results/mixed-benchstat.txt).

Original ASCII parsing had wider timing variation than the other variants (about ±21% at 64 KiB). A separate ten-trial, 200 ms confirmation after clearing disk space reproduced the result: 82.66 µs original, 37.65 µs scalar spans, and 10.39 µs SIMD. The conclusion does not depend on the fastest or slowest original sample. See [main comparison](results/benchstat.txt) and [ASCII confirmation](results/confirm-benchstat.txt).

Allocation counts are identical across all three variants in the main matrix. At 64 KiB, ASCII parsing uses 37 allocations and about 69.7 KiB allocated per call; generation uses 35 allocations and about 149.9 KiB. Occasional-escape parsing uses 38 allocations and generation 50. The scanner itself has zero heap allocations. This reduces CPU time while preserving output ownership and retained-memory behavior; it is not a reduction in output-buffer memory.

The kernel classifies 16 bytes at a time using signed-byte comparisons and masks. Parser spans stop at a quote, backslash, control byte, or high bit. Generation also stops at `<`, `>`, and `&`. Existing scalar code handles escapes, UTF-8, replacement characters, and U+2028/U+2029. Each SIMD load comes from a bounded 16-byte local array, avoiding the input-sized string-to-byte copies found in the earlier ASCII experiment.

An unrestricted first prototype regressed on densely escaped and Unicode text. The final version requires a safe initial 16-byte ASCII prefix before entering the vector path. Short or unsuitable prefixes use the complete original function, with its original signature. After a long run, a short run or Unicode byte transfers the remaining work to a scalar continuation. Escape decoding only uses span copying when its initial unescaped prefix is long enough; otherwise it retains the original loop. This conservative policy deliberately gives up some possible gains to avoid repeated vector setup on unsuitable data.

The integration preserves every existing accounting operation. JSON parsing keeps token cloning and the builder-capacity preflight. Generation keeps each original projected-output check, including conservative six-byte escape projections. Charges still reflect logical input/output work, not the number of SIMD instructions. The scanner neither allocates dynamic scratch nor publishes or mutates script values. It preserves existing cancellation checkpoints; it does not establish a new latency bound for an individual long scan.

Validation passed:

- Every byte value at SIMD boundary offsets, all-safe lengths through 257 bytes, malformed UTF-8, controls, HTML characters, and 1,000 deterministic random buffers; both scanners allocate zero heap objects for a 16 KiB span.
- Differential parser and string-encoder tests against the original code, comparing values, exact errors, parser positions, retained charges, step counters, latched quota failures, output charges, cancellation polls, and scratch/section cleanup.
- Exact charged-step boundaries and minimum passing memory limits, including one byte below and above each threshold; deterministic cancellation checks.
- The full repository test suite, then the full runtime suite with `VIBES_ESTIMATOR_VERIFY=1` and `VIBES_ENV_RECYCLE_VERIFY=1`. This includes source-document retention and builder-rounding guards.
- Thirty seconds of differential fuzzing, **500,111 executions**, with no failure. Inputs are bounded to 4 KiB and include a seed combining long ASCII, escapes, malformed UTF-8, and scalar continuation.
- `go vet ./...`; the scalar-span differential tests also pass on Go 1.26.3 without the SIMD experiment.

The initial full-suite build exhausted available disk space. Removing 4 GiB of Go build-cache entries older than 24 hours and superseded experiment binaries allowed validation to finish. All reported final test logs are from successful runs. The benchmark rerun above confirms the ASCII finding after cleanup.

There is a separate performance opportunity in heavily escaped generation. The current `JSON.stringify` builtin does not declare nonmutation, so every escaped-character output projection can re-walk the unchanged execution graph. In a baseline dense-escape CPU profile, `memoryEstimator.env` accounted for 58% cumulative CPU. The 64 KiB dense-escape case takes about 9 ms, with little SIMD benefit, even though escaping expands this particular input only about 1.8 times. Auditing a nonmutation declaration could allow existing estimator memoization while retaining every projection and charge. Calls beneath an undeclared outer builtin must keep their conservative behavior. This experiment does not change that accounting path. See [profile](results/dense-stringify-top.txt) and [follow-up analysis](dense-stringify-issue.md).

Before production adoption, the prototype needs a maintainable implementation with scalar/build fallbacks, supported-platform coverage, and measurements on x86-64. The current numbers establish ARM64 results only. Further workload sampling should include realistic mixed documents, short keys, many tiny values, and text whose character distribution changes within a token. The generated fallback copies are an experiment mechanism, not a proposed source organization.

To reproduce the experiment on ARM64:

```sh
python3 benchmarks/audits/2026-09-08-json-simd/measure.py /tmp/vibescript-json-simd-rerun
```

The runner creates overlays, validates all three variants, runs repeated measurements, and then runs the full suite, accounting oracles, fuzzing, and vet. The current fixture also includes a mixed-Unicode case added after the main matrix. Its additional measurements are retained separately. Recreate the comparison with `benchstat` over the generated `bench-original.txt`, `bench-scalar.txt`, and `bench-simd.txt`; the tool version used here is `golang.org/x/perf v0.0.0-20260908200009-22c9c6c9d4da`.
