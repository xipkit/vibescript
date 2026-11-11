# Mixed-text case mapping (VIBE-189)

Baseline: `3e8a8d84` on Apple M4, Rust 1.98.1, the normal release profile
(thin LTO, one codegen unit, overflow checks enabled). PR #1405 is in this
base; VIBE-188 has not landed. Both default SIMD and `--no-default-features`
builds are measured.

ASCII runs are inspected without scratch copies and converted directly into
the reserved output. A 768-byte index derived at compile time from the canonical
Unicode table handles ASCII, Latin-1 and Latin Extended-A lookups. Nonzero
Unicode mappings already mean that the character changes, so they need no byte
comparison. The unused in-place SIMD converter is removed; Rust's iterator
conversions allow the compiler to vectorize the direct copy.

The sizing and copying walks, input validation, chunk checkpoints and per-piece
step charges remain. Keeping the sizing pass reserves the full output before
copying and allows unchanged calls to retain the receiver without allocating.
No golden counters are re-recorded.

## Inputs and measurement

VIBE-185's temporary benchmark directory had been removed. `measure.py` defines
replacement fixtures: lowercase French prose, deterministically generated ASCII
words of length 1–31 separated by `é`, and lowercase ASCII-only prose. Each is
40 KiB and 3 MiB (trimming only incomplete final UTF-8 sequences). `upcase`
changes the text; `downcase` exercises the unchanged-result path. These new
inputs have their own baseline and must not be compared directly with the
original card's instruction totals.

The existing `compare` example invokes a compiled script with a string argument,
including normal metering. The script returns the converted string. Each sample
checks the output digest, steps, tracked peak bytes and retained bytes against
the first sample. The executable also verifies its final timed result
against its untimed result.

The driver interleaves binaries in a seeded random order. Retired instructions
are the difference between `/usr/bin/time -l` process counts for `n` and `3n`
calls, divided by `2n`. Both run counts exceed the harness's 32-call warmup cap,
so parsing, compilation, warmup and output encoding cancel. Elapsed time comes
from the harness's timed loop in the longer run. Repeated baseline binaries
provide same-binary controls. Raw samples retain load averages, executable and
fixture hashes, and output/accounting signatures.

## Reproduction

Build and preserve the `compare` executable from the baseline and candidate,
using separate build directories on the external disk:

```sh
CARGO_TARGET_DIR="$BUILD/simd" cargo build --locked --release --example compare
cp "$BUILD/simd/release/examples/compare" "$BUILD/candidate-simd"
CARGO_TARGET_DIR="$BUILD/portable" cargo build --locked --release --no-default-features --example compare
cp "$BUILD/portable/release/examples/compare" "$BUILD/candidate-portable"
```

Repeat those builds in a detached worktree at the baseline, preserving the
executables as `base-simd` and `base-portable`. Run from the candidate checkout:

```sh
python3 benchmarks/case-mapping/measure.py "$BUILD/measurements" \
  "base-simd=$BUILD/base-simd" "candidate-simd=$BUILD/candidate-simd" \
  "control-simd=$BUILD/base-simd" \
  "base-portable=$BUILD/base-portable" "candidate-portable=$BUILD/candidate-portable" \
  "control-portable=$BUILD/base-portable" --rounds 5
```

## Results

Five randomized rounds per case and executable on 2026-11-11. Mixed-text calls
take **27.1–46.8% less time** across both sizes and builds. ASCII controls
take **29.7–57.3% less time**. The repeated-baseline median timing
differences range from -1.1% to +3.2%. All output and accounting
signatures match. [Raw samples and hashes](results.json.gz) accompany this report.

### SIMD

| Input | Size | Method | Time before → after (µs) | Time change | Instructions before → after (millions) | Instruction change |
|---|---|---|---:|---:|---:|---:|
| French | 40 KiB | `upcase` | 155.70 → 87.25 | -44.0% | 3.251 → 2.445 | -24.8% |
| French | 40 KiB | `downcase` | 82.71 → 47.64 | -42.4% | 1.761 → 1.161 | -34.1% |
| Random | 40 KiB | `upcase` | 118.94 → 80.13 | -32.6% | 2.278 → 1.779 | -21.9% |
| Random | 40 KiB | `downcase` | 67.06 → 35.69 | -46.8% | 1.233 → 0.815 | -33.9% |
| Ascii | 40 KiB | `upcase` | 4.54 → 2.24 | -50.6% | 0.108 → 0.059 | -45.5% |
| Ascii | 40 KiB | `downcase` | 3.15 → 1.50 | -52.4% | 0.079 → 0.040 | -49.0% |
| French | 3 MiB | `upcase` | 11,951.31 → 6,680.69 | -44.1% | 248.509 → 186.652 | -24.9% |
| French | 3 MiB | `downcase` | 6,301.27 → 3,539.46 | -43.8% | 134.323 → 88.222 | -34.3% |
| Random | 3 MiB | `upcase` | 10,889.71 → 7,279.00 | -33.2% | 174.297 → 135.913 | -22.0% |
| Random | 3 MiB | `downcase` | 6,067.40 → 4,270.95 | -29.6% | 94.046 → 61.886 | -34.2% |
| Ascii | 3 MiB | `upcase` | 335.37 → 155.60 | -53.6% | 7.198 → 3.316 | -53.9% |
| Ascii | 3 MiB | `downcase` | 218.79 → 93.41 | -57.3% | 5.231 → 2.262 | -56.8% |

### Portable

| Input | Size | Method | Time before → after (µs) | Time change | Instructions before → after (millions) | Instruction change |
|---|---|---|---:|---:|---:|---:|
| French | 40 KiB | `upcase` | 145.96 → 88.05 | -39.7% | 3.140 → 2.452 | -21.9% |
| French | 40 KiB | `downcase` | 78.30 → 47.46 | -39.4% | 1.721 → 1.166 | -32.3% |
| Random | 40 KiB | `upcase` | 119.27 → 82.39 | -30.9% | 2.197 → 1.814 | -17.4% |
| Random | 40 KiB | `downcase` | 63.94 → 35.63 | -44.3% | 1.207 → 0.839 | -30.5% |
| Ascii | 40 KiB | `upcase` | 5.99 → 4.21 | -29.7% | 0.141 → 0.104 | -26.0% |
| Ascii | 40 KiB | `downcase` | 4.20 → 2.82 | -32.8% | 0.104 → 0.071 | -31.6% |
| French | 3 MiB | `upcase` | 11,149.50 → 6,743.80 | -39.5% | 240.027 → 187.260 | -22.0% |
| French | 3 MiB | `downcase` | 5,881.24 → 3,606.78 | -38.7% | 131.233 → 88.640 | -32.5% |
| Random | 3 MiB | `upcase` | 10,714.70 → 7,368.52 | -31.2% | 168.034 → 138.617 | -17.5% |
| Random | 3 MiB | `downcase` | 5,928.68 → 4,321.90 | -27.1% | 92.052 → 63.693 | -30.8% |
| Ascii | 3 MiB | `upcase` | 423.72 → 269.42 | -36.4% | 9.709 → 6.819 | -29.8% |
| Ascii | 3 MiB | `downcase` | 281.42 → 177.32 | -37.0% | 7.072 → 4.598 | -35.0% |


## Validation

- Complete workspace gate-profile test suites pass: 2,579 tests with all
  features and 2,536 without default features, including compiler-memory and
  allocation tests.

- All 11 casing unit tests pass on both builds. Coverage includes lookup against
  the canonical table for every Unicode scalar, all mapped characters and modes,
  invalid UTF-8, ASCII lane/alignment and run boundaries, bang/unchanged behavior,
  and release of output reserved before a copy fails.
- The existing walk/copy step, cancellation, deadline, expansion-reservation and
  storage-lifetime tests retain their original expected counters.
- All 130,697 stable golden accounting observations match between the unchanged
  baseline and candidate, and between SIMD and portable candidate builds.
  Of 233,902 raw baseline/candidate observations, only four differ; all four
  (`call10334`, `call33640`, `call41755`, `call41802`) are already marked `varies`.
- Strict checks against the recorded golden counters report existing drift in
  both the baseline and candidate. No counters are re-recorded. Observable
  golden checks, including all 2,867 CLI cases and 216 LSP sessions, pass.
- The CLI was also exercised directly with Unicode expansion, dotted-I lowering,
  title case, swapcase, folding and ASCII mode.
- Clippy with warnings denied and the pinned Adamantium checks pass with and
  without default features.
