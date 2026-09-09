# Additional SIMD use cases

Experimental overlays over Vibescript `e6f1fca90c012885453a6c413c0826f6abff1f32`. Production code and `go.mod` are unchanged. These ARM64 prototypes require Go 1.27.1 and `GOEXPERIMENT=simd`; they do not implement production dispatch or an x86-64 backend.

## Method

Apple M4, darwin/arm64. Compare the unchanged runtime, ordinary Go alternatives, and ARM64 NEON kernels using the same Go toolchain and experiment flag. Ten trials rotate variant order; each benchmark runs for 100 ms. Benchmarks are serialized. Each operation measures one complete `Script.Call`, with a 5,000,000-step quota and a 64 MiB memory quota. Inputs and scripts are prepared outside the timer.

The matrix includes 16-byte, 4 KiB, and 64 KiB inputs, early and late comparison failures, valid Unicode, invalid UTF-8 at both ends, short and long whitespace runs, dense and sparse regexp metacharacters, and unchanged default casing controls. Input sizes refer to bytes. Unicode fixtures preserve valid UTF-8 and use ASCII padding for an incomplete final pattern.

Ordinary Go alternatives are eight-byte word operations for case conversion/comparison, extracted scalar spans for whitespace, and `strings.IndexAny` spans with dense-input fallback for regexp escaping. Whitespace's Go column therefore measures extraction overhead, not a proposed word-vector optimization. SIMD uses fixed stack tiles and writes into existing output buffers; it does not convert the remaining input string into a byte slice.

## Results

| 64 KiB workload | Current (µs) | Ordinary Go (µs) | SIMD (µs) |
|---|---:|---:|---:|
| `upcase(:ascii)` | 30.16 | 15.99 | 15.70 |
| `downcase(:ascii)` | 30.35 | 15.76 | 15.85 |
| `swapcase(:ascii)` | 41.38 | 15.77 | 15.40 |
| `capitalize(:ascii)` | 30.30 | 15.76 | 15.67 |
| `casecmp`: equal | 48.47 | 10.75 | 5.97 |
| 255-byte fields | 98.21 | 83.79 | 43.37 |
| All whitespace | 27.09 | 19.57 | 4.61 |
| Long padding | 25.50 | 29.62 | 4.51 |

At 64 KiB, SIMD is 8.1× faster for equal `casecmp`, 5.6× for long padding, and 2.3× for long split fields. ASCII casing is about 1.9–2.7× faster; the ordinary Go word implementation captures almost all of that 64 KiB gain. At 4 KiB, SIMD adds roughly another 9% over the Go casing baseline. Default casing and valid-UTF-8 `casecmp?` controls show no statistically significant SIMD change.

SIMD splitting regressed short-word inputs by 3.58% and short Unicode fields by 3.19% at 64 KiB (similar regressions at 4 KiB). Do not promote these scans unconditionally. Short padded 16-byte strip calls regressed 0.76–1.08%; unpadded controls stayed essentially unchanged. Full controls and unadjusted per-case significance tests are in `results/benchstat.txt`.

### Regexp needs another experiment

| 64 KiB workload | Current (µs) | Ordinary Go (µs) | SIMD (µs) median [min, max] |
|---|---:|---:|---:|
| plain | 50.81 | 18.58 | 92.75 [33.48, 113.51] |
| sparse-meta | 107.75 | 62.78 | 56.72 [27.09, 134.22] |
| dense-meta | 332.62 | 346.98 | 282.02 [274.81, 378.98] |
| unicode | 50.69 | 18.56 | 49.60 [20.03, 113.89] |

Ordinary Go span scanning cuts plain regexp-escape time by 63% and sparse escaping by 42%, but dense escaping regresses 4.32% at 64 KiB and 5.70% at 4 KiB. SIMD is unstable: its plain 64 KiB median is 82.56% slower than the original, and the wide ranges prevent treating its other median improvements as reliable. Do not promote this SIMD kernel yet.

A separate CPU profile places about 96% of samples in the SIMD size scanner. Assembly confirms native table lookup and reduction instructions, plus a redundant input-to-stack-to-vector round trip at SP+8. Stack alignment and dependency stalls remain hypotheses; they do not establish the cause of the variance. A next experiment can remove the stack copy using bounded word loads and vector insertion, then separately test fewer horizontal reductions. The profile and assembly in `results/` came from the same source in the pilot binary; profile timing is diagnostic, not part of the ten-trial estimates.

### Memory

Case transforms retain their two-buffer allocation shape. The scan/compare kernels allocate zero heap objects. Public-call allocation medians match exactly in 114 of 116 cases. The two large, many-field split cases fluctuate slightly; short Unicode fields measured one extra allocation (14,668 versus 14,667) and 0.39% more B/op, a small statistically significant regression that also needs to be avoided. There is no demonstrated retained-memory reduction. Exact logical memory quota thresholds still match across all 301 differential cases.


## Scope and accounting

- Casing replacements affect `upcase(:ascii)`, `downcase(:ascii)`, `swapcase(:ascii)`, and `capitalize(:ascii)`, plus existing invalid-UTF-8 fallbacks. Default valid-UTF-8 casing still uses the original Unicode implementation, even when the text contains only ASCII. Every non-ASCII byte remains unchanged in explicit ASCII mode.
- `casecmp` keeps byte ordering after folding only A–Z. `casecmp?` retains Unicode simple folding for valid UTF-8 and uses the optimized byte comparison only for invalid UTF-8. Punctuation ordering, length differences, and first-mismatch behavior are preserved.
- Split projection and both materialization paths share exactly the same whitespace boundaries. Split recognizes bytes 9–13 and space; strip additionally recognizes NUL. Unicode spaces remain field content or stop trimming. Result cloning and detached substring ownership are unchanged.
- Regexp size projection retains overflow and output-limit checks. Quoting preserves every original output write at a metacharacter. `escape`, `quote`, and `union` retain their wrappers, charges, capacity checks, and compile behavior.
- All logical step charges, scratch projections, memory checks, cancellation checkpoints, and result publication stay in the original wrappers. Fixed stack temporaries do not introduce input-sized scratch allocations.

The differential harness compares 301 deterministic public-call snapshots across all three binaries. Each snapshot records exact minimum passing step and memory quotas, limits immediately below and above them, output digests, error types and text, and deterministic cancellation poll counts. Fifteen `Regexp.union` cases explicitly use a warm compile cache in all variants. Pure-helper tests cover all byte values, vector boundaries, random byte strings, projection limits, existing builder prefixes, and allocation parity.

Full repository tests, the full runtime suite with both `VIBES_ESTIMATOR_VERIFY=1` and `VIBES_ENV_RECYCLE_VERIFY=1`, and `go vet ./...` passed under the combined SIMD overlay. All three variants passed the added helper/accounting tests. Command logs and the validation summary are in `results/`.

## Reproduction

On ARM64 with the Go toolchain available:

```sh
python3 benchmarks/audits/2026-09-08-simd-use-cases/measure.py /tmp/vibescript-use-cases 10 100ms
python3 benchmarks/audits/2026-09-08-simd-use-cases/validate.py /tmp/vibescript-use-cases
benchstat /tmp/vibescript-use-cases/bench-original.txt /tmp/vibescript-use-cases/bench-go.txt /tmp/vibescript-use-cases/bench-simd.txt
```

`prepare.py` copies original helper bodies into test-only reference functions and maps `.go.txt` fixtures into the runtime package using Go overlays. It changes only pure helpers and character-scan loops in the two source files. The helper references are regenerated from the checked-out baseline; do not apply these overlays to another revision without revalidating the mapping.

Before production adoption, add ordinary-Go fallbacks, explicit experiment/build selection and CPU dispatch, independent x86-64 measurements, and representative application workloads. Preserve exact quota thresholds and rerun the short/early-exit controls. These results do not establish a memory reduction or a benefit for every input distribution.

## Follow-on candidates

The compiler's literal scanners (`internal/parser/lexer.go`, `readDoubleQuotedString` and `readSingleQuotedString`) decode and append ordinary runes individually. Safe ASCII spans could help compilation, but position tracking, interpolation, NUL, and invalid UTF-8 require a separate experiment. This has not been benchmarked here.

Literal portions of `format` and `strftime` can first use `strings.IndexByte` to find `%` and copy spans. Go already supplies an ARM64 SIMD byte-search implementation. Preserve formatter error/projection order and `strftime`'s 4,096-byte accounting checkpoints. These have not been benchmarked here and do not require new SIMD kernels as a first step.

Packed numeric reduction is a poor initial target: arrays hold heterogeneous tagged values, and `sum` preserves ordered arithmetic, big-integer promotion, per-element steps, and intermediate accumulator memory checks.
