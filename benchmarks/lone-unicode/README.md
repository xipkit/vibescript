# Counting lone two-byte characters

VIBE-188 avoids starting the non-ASCII span recognizer for a single character
between ASCII words. The existing whole-block check still runs first, preserving
the long-run scanner and its alignment. When that check fails, `unicode_span`
validates and counts one two-byte character directly, returning immediately if ASCII or
the end of input follows. Otherwise the recognizer starts at the next character.
The shortcut handles only valid two-byte characters (C2–DF followed by 80–BF).
Three-/four-byte characters stay on the existing recognizer. A more general
first-character validator reduced mixed-width instruction counts further, but
slowed repeated small inputs; specializing two-byte characters preserves the
accented-letter benefit without that tradeoff. The counting fallback also uses
the existing width-only decoder, avoiding unused code-point state.
Invalid and truncated prefixes retain the original full-input fallback and
exact metering.

JSON parsing shares this scanner through `text_span`, so the measurements also
include the existing `json_parse_unicode_4k` workload (same JSON object and
payload, with a longer iteration count).

The baseline is master `4c9525fabb283b273682556c33993b675c2cbf35`, which includes
VIBE-185 / #1405. Measurements use Apple M4, macOS 27.0.1 (26A434), Rust 1.98.1,
and the unchanged release profile (thin LTO, one codegen unit, overflow checks).
SIMD uses NEON; portable uses `--no-default-features`.

The original VIBE-185 scratch directory was gone when this work resumed.
`cases.py` recreates the requested distribution with seed 188: random 1–15-byte
ASCII words, each followed by one randomly selected two-, three-, or four-byte
character, at 40,000 and 3,000,000 bytes. Truncation discards any incomplete
final character. French, Russian, and Japanese controls repeat the sentences
in the generator to about 40 KB; the Unicode control is the existing
`text/length_unicode/metered` fixture, unchanged except for iteration count.
ASCII is an additional control. These are new fixtures, so their absolute
numbers should not be compared directly with the earlier card's French prose.

## Reproduction

Preserve baseline and candidate `compare` binaries built from clean source
snapshots, once with default features and once with `--no-default-features`:

```sh
CARGO_TARGET_DIR="$BUILD/target" cargo build --locked --release --example compare
cp "$BUILD/target/release/examples/compare" "$BUILD/LABEL-simd"
CARGO_TARGET_DIR="$BUILD/target" cargo build --locked --release --example compare --no-default-features
cp "$BUILD/target/release/examples/compare" "$BUILD/LABEL-portable"
python3 benchmarks/lone-unicode/cases.py "$BUILD/cases.json"
python3 benchmarks/lone-unicode/measure.py "$BUILD/cases.json" "$BUILD/simd.json" \
  before="$BUILD/base-simd" after="$BUILD/after-simd"
python3 benchmarks/lone-unicode/measure.py "$BUILD/cases.json" "$BUILD/portable.json" \
  before="$BUILD/base-portable" after="$BUILD/after-portable"
python3 benchmarks/lone-unicode/summarize.py "$BUILD/simd.json" "$BUILD/portable.json"
python3 benchmarks/mixed-text-placement/count.py "$BUILD/cases.json" "$BUILD/instructions.json" \
  before-simd="$BUILD/base-simd" after-simd="$BUILD/after-simd" \
  before-portable="$BUILD/base-portable" after-portable="$BUILD/after-portable"
```

Each timing sweep has one warmup and ten measured rounds. Every round shuffles
the baseline, candidate, and a repeated baseline control. The harness checks
output digests, charged steps, peak tracked memory, and retained tracked memory
on every invocation. It records all samples and system load; it does not discard
slow samples. Timing excludes compilation and fixture setup. Instruction counts
are differential `/usr/bin/time -l` estimates at N and 3N calls, three repetitions,
not a disassembly count. N is 500 for the small fixtures and 40 for the 3 MB
fixture, keeping both processes at 32 warmup calls. Smaller N would introduce
unequal warmup work (tracked separately as VIBE-203). Some per-process counts
were collected while workspace validation continued. No builds or tests run concurrently with the
elapsed-time measurements.

## Additional controls

`experiments.py` varies the mixed-width input size from 4 KB to 3 MB, holds the
separator width fixed at two, three, or four bytes, and cycles four independently
seeded 40 KB inputs. The cycling case calls `length` four times per invocation
inside a Vibescript loop; its timings include that loop and all four lengths.
These use one warmup plus six measured randomized rounds and the same repeated
baseline control and accounting checks:

```sh
python3 benchmarks/lone-unicode/experiments.py "$BUILD/experiments.json"
python3 benchmarks/lone-unicode/measure.py "$BUILD/experiments.json" "$BUILD/experiments-simd.json" \
  before="$BUILD/base-simd" after="$BUILD/after-simd" --rounds 6
python3 benchmarks/lone-unicode/measure.py "$BUILD/experiments.json" "$BUILD/experiments-portable.json" \
  before="$BUILD/base-portable" after="$BUILD/after-portable" --rounds 6
```

## Correctness and validation

The decoder and span tests cover invalid/truncated prefixes, including
independent third- and fourth-byte failures. More than 2.3 million mixed-input checks compare character
count, validity, and steps with the character-at-a-time reference. The existing
exact-limit, chunk-boundary, cancellation, and deadline tests also pass.

Final runtime checks passed with both feature sets: 840 unit and 1,281 integration
tests with all features, and 826 unit and 1,252 integration tests without default
features, using the optimized `gate` profile. Workspace Clippy and Adamantium
pass with both feature sets; formatting and diff checks pass. Each benchmark
binary also executes the actual `input.length` and `JSON.parse` programs and
checks their results against the independently generated expectations.

## Unmodified release binaries

Ten measured randomized rounds follow one warmup round. Times below are
medians in microseconds; changes are medians of paired after/before ratios,
so they need not equal the ratio of the displayed medians.

| Workload | SIMD before → after (µs) | Paired change | Portable before → after (µs) | Paired change |
|---|---:|---:|---:|---:|
| mixed_40000 | 18.084 → 16.521 | -7.8% | 17.736 → 16.530 | -6.2% |
| mixed_3000000 | 2378.241 → 2226.227 | -6.2% | 2392.371 → 2222.131 | -6.7% |
| french | 17.812 → 12.324 | -30.6% | 18.432 → 13.460 | -26.8% |
| russian | 26.433 → 24.429 | -7.4% | 25.141 → 23.302 | -7.4% |
| japanese | 8.765 → 8.705 | -0.7% | 10.955 → 10.847 | -0.7% |
| ascii_40000 | 0.739 → 0.730 | +0.2% | 1.365 → 1.395 | +1.7% |
| text/length_unicode/metered | 11.467 → 11.448 | +0.0% | 19.248 → 19.236 | -0.3% |
| json_parse_unicode_4k | 1.827 → 1.811 | +0.1% | 2.260 → 2.279 | +1.1% |

The repeated-baseline controls were within 1.2% on every workload. The 40 KB
and 3 MB mixed-width inputs improve on both builds. Russian improves; Japanese,
`text/length_unicode`, and JSON remain within about 1% of baseline. The portable
ASCII shift (+1.7%) is close to its same-binary control (+1.2%).

## Retired instructions per call

| Workload | SIMD before → after | Change | Portable before → after | Change |
|---|---:|---:|---:|---:|
| mixed_40000 | 351,980 → 338,483 | -3.8% | 351,736 → 338,394 | -3.8% |
| mixed_3000000 | 25,711,577 → 24,629,827 | -4.2% | 25,715,465 → 24,713,279 | -3.9% |
| french | 356,633 → 244,582 | -31.4% | 361,053 → 249,041 | -31.0% |
| russian | 631,667 → 617,001 | -2.3% | 599,251 → 584,653 | -2.4% |
| japanese | 229,229 → 231,292 | +0.9% | 256,158 → 258,395 | +0.9% |
| ascii_40000 | 19,597 → 19,584 | -0.1% | 34,375 → 34,508 | +0.4% |
| text/length_unicode/metered | 240,095 → 240,258 | +0.1% | 345,248 → 350,748 | +1.6% |
| json_parse_unicode_4k | 40,815 → 40,833 | +0.0% | 46,453 → 46,550 | +0.2% |

## Size and width controls

Median paired changes from six measured rounds; controls repeat the baseline.

| Workload | SIMD change | Control | Portable change | Control |
|---|---:|---:|---:|---:|
| size_4000 | -5.6% | -0.4% | -5.5% | +0.4% |
| size_10000 | -7.3% | -0.8% | -6.0% | +0.6% |
| size_20000 | -7.8% | -1.2% | -6.7% | +0.8% |
| size_40000 | -8.0% | -0.9% | -6.6% | +0.9% |
| size_80000 | -7.6% | -1.2% | -5.5% | -0.4% |
| size_160000 | -4.7% | +0.2% | -6.6% | -2.1% |
| size_3000000 | -6.7% | +0.2% | -6.1% | +0.5% |
| width_2 | -27.9% | +0.3% | -27.5% | -0.0% |
| width_3 | +0.1% | -0.3% | +0.3% | -0.1% |
| width_4 | +3.3% | -1.0% | +3.9% | -0.1% |
| four_inputs_40000 | -5.8% | +1.3% | +1.5% | +1.0% |

All mixed-input sizes improve in this sweep, and fixed three-byte separators
remain flat. Fixed four-byte separators cost 3–4% more. They still traverse the recognizer
after the added two-byte eligibility check. The four-input cycle
improves on SIMD; the portable result (+1.5%) is close to its control (+1.0%).
This is an accented-letter optimization, not a uniform speedup for every
Unicode distribution. Timings are from one M4, not a cross-machine guarantee.

## Raw evidence

`evidence.json.gz` contains all primary and additional timing samples,
per-process instruction counts, validation results, test/lint summaries,
binary/fixture/source/generator hashes, and the build environment. Timing keys
are `timing_simd`, `timing_portable`, `experiments_simd`, and
`experiments_portable`; extract one to a JSON file and pass it to `summarize.py`.
The instruction-count warmup issue is tracked separately as VIBE-203.
