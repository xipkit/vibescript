# JSON lone-escape placement measurements

VIBE-187 compares two source trees on native ARM64 macOS with Rust 1.98.1.
It reuses the committed VIBE-201 isolated-function relinker and the VIBE-199
output validator, instruction sampler and randomized timing runner.

From the candidate checkout:

```sh
bash benchmarks/json-lone-escapes/run.sh /absolute/base /absolute/head /absolute/build
```

The `JSON placement` PR workflow runs the same experiment on `macos-latest`.
It records the actual CPU, OS, compiler and source commits; hosted-runner
results must be identified separately from OTIS's Apple M4 measurements.
The first probe requires positive retired-instruction counts. A runner that
does not expose the counter fails explicitly, preserving the probe output.

Both default SIMD and portable builds use the release profile. Each binary
gets 32 placements, moving only `write_string` in four-byte increments through
128 bytes. Every other original symbol must stay fixed within its cohort.
All builds, relinks, output comparisons and instruction captures finish before
timing begins. The payloads are the existing ASCII-line, Unicode and lone-escape
4 KiB cases, each with 6,000 timed calls. Each of two independent seeds runs
one full warmup cycle and 15 randomized measured cycles with a repeated
baseline binary as a noise control. The shared runner rejects a whole group
when its sampled one-minute load reaches 6 and retains rejected samples.

The artifacts include full-output/accounting validation for every binary,
three differential retired-instruction samples for every case and placement,
complete timing samples, median/range summaries and per-build disassembly.
The instruction sampler uses 500 and 1,500 calls, both beyond the harness's
32-call warmup. Audit neighbouring loops in the saved assembly before accepting
a candidate; successful correctness tests alone do not establish an improvement.
No placement pad or diagnostic linker flag is used in production.
