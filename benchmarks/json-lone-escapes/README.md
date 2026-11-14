# JSON lone-escape placement measurements

VIBE-187 compares two source trees on native ARM64 macOS with Rust 1.98.1.
It reuses the committed VIBE-201 isolated-function relinker and the VIBE-199
output validator, instruction sampler and randomized timing runner.

From the candidate checkout:

```sh
bash benchmarks/json-lone-escapes/run.sh /absolute/base /absolute/head /absolute/build
```

The `JSON placement` PR workflow uses `--prepare-only` on `macos-latest` to
build, relink and validate every placement, including metered accounting.
GitHub's Apple M1 virtual machines do not expose retired-instruction counters.
The full experiment therefore runs separately on a physical host such as OTIS.
Its first probe requires positive retired-instruction counts; a host that does
not expose the counter fails explicitly, preserving the probe output.
`scope.txt` records the selected mode, and `environment.txt` records the actual
CPU, OS, compiler and source commits. A passing hosted validation job supplies
no performance evidence and does not satisfy the full experiment's acceptance
criteria.

Both default SIMD and portable builds use the release profile. Each of the
four builds gets two independent 32-placement sweeps: move `write_string`
alone, then move `Output::put_run` alone, in four-byte increments through
128 bytes. Every other original symbol must stay fixed within its cohort.
There are 256 relinked executables. Matched comparisons use the moved
function's actual address modulo 128, rather than assuming equal order-file
pad indices imply equal phases. Every entry records both functions' addresses.
All builds, relinks, output comparisons and instruction captures finish before
timing begins. The payloads are the existing ASCII-line, Unicode and lone-escape
4 KiB cases, each with 6,000 timed calls. Each of two independent seeds runs
one full warmup cycle and 15 randomized measured cycles with a repeated
baseline binary as a noise control. The shared runner rejects a whole group
when its sampled one-minute load reaches 6 and retains rejected samples.

The artifacts include full-output/accounting validation for every binary,
three differential retired-instruction samples for every case and placement,
complete timing samples, median/range summaries and per-build disassembly.
The `*-phases.json` summaries retain every phase's mean and its paired change
against the baseline, plus the descriptive first-harmonic amplitude. Both axes
and both seeds must be reported separately alongside the same-binary control;
a smaller writer band with a comparable new helper band is transferred
sensitivity. Independent sweeps do not establish robustness across arbitrary
joint writer/helper layouts. A repeatable helper effect requires an additional
crossed control before making a broader claim.
The instruction sampler uses 3,000 and 9,000 calls, both beyond the harness's
32-call warmup. Audit neighbouring loops in the saved assembly before accepting
a candidate; successful correctness tests alone do not establish an improvement.
No placement pad or diagnostic linker flag is used in production.

## Runtime candidate

Nearby escapes now enter the existing buffered escape loop before the first
in-place append. The buffer accepts complete ordinary ASCII spans between
escapes, instead of only single ordinary bytes. Its cursor, output length and
deferred step count stay local until the block is appended. Long ordinary runs
keep the outer writer's scan/copy path, and the existing one-byte fast path
remains in the buffer. A span that does not fit, or continues into Unicode,
stays whole for its scanner so its append charge is preserved.
The probe looks at only the available buffer room plus one byte; it does not
rescan an entire long span merely to discover that it cannot fit.

This is a source-level intervention in the span/escape loop. It does not
identify a branch-predictor mechanism. The earlier scalar-buffer prototype was
rejected because it replaced the ordinary-run scanner and regressed ASCII
lines. Acceptance of the current candidate still requires complete quiet-host
M4 placement measurements and a neighbouring-loop assembly audit; the
uncontrolled x86 diagnostics do not establish either.
