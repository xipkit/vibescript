# Whitespace scan production measurements

Apple M4, darwin/arm64, Go 1.27.1 with `GOEXPERIMENT=simd`. Each measurement is one complete `Script.Call` with a 5,000,000-step quota and a 64 MiB memory quota. Inputs and compilation are outside the timer. Ten 100 ms samples rotate the original, scalar-control, and SIMD variants; benchmarks run serially.

The original variant restores the five split/trim functions from `7d1563d9cebd38b64a3bd5fb9e35000bd4a20da3`. The scalar control disables only whitespace SIMD through its compile-time constant, retaining the same toolchain and experiment flag. Production Go 1.26 and unsupported targets use the scalar implementation.

| 64 KiB input | Original | SIMD | Time change |
|---|---:|---:|---:|
| Split, 255-byte fields | 101.85 µs | 59.01 µs | -42.06% |
| Split, all whitespace | 27.68 µs | 4.61 µs | -83.36% |
| Strip, long padding | 26.20 µs | 4.56 µs | -82.60% |
| Split, short words | 1,351 µs | 1,356 µs | No significant change |
| Split, short Unicode fields | 800.3 µs | 804.4 µs | No significant change |

No short-input, Unicode, unpadded-string, or short-padding timing control regressed significantly. Splitting inputs that change between long and short fields improved 3.2–3.9%. The scalar control showed no significant timing change. These are ARM64 measurements; the guarded AVX2 implementation requires separate x86-64 performance measurements.

Scanners allocate zero heap objects. Public-call allocation counts showed no significant change. Large split workloads have small allocation noise from the surrounding runtime; this change does not claim a memory reduction. Raw samples and benchstat output are in `results/`.

The accounting harness matched 84 public split/trim snapshots across all three variants: exact minimum step and memory quotas, values and errors immediately around those limits, and deterministic cancellation polls. Existing retention tests cover detached split parts and trimmed strings separately. Pure scanner tests cover every byte, vector/probe boundaries, random bytes, Unicode, NUL, and invalid UTF-8. Long-run split tests cover projection and materialization with limits -2, -1, 0, 1, 2, 3, and 1024.

The full suite passed with Go 1.26.3 and Go 1.27.1 SIMD. The SIMD estimator/recycle oracle suite and vet passed, as did linux/amd64 SIMD and linux/386 Go 1.26 builds.

Run `python3 benchmarks/audits/2026-09-09-whitespace/measure.py /tmp/vibescript-whitespace 10` on ARM64 or AVX2-capable x86-64 with Go 1.27.1 available. The script generates overlays and binaries outside the checkout, verifies the 84 accounting snapshots, and writes repeated timings. Use benchstat on the three generated `bench-*.txt` files. Use 0 trials to run only the accounting comparison. Measurements on other machines or revisions will differ.
