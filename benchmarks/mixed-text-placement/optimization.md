# VIBE-200: shorten NEON byte classification

The ARM64 SIMD scanner classifies control and non-ASCII bytes with one signed
comparison instead of two unsigned comparisons and an OR. Interpreted as
signed bytes, 128–255 are negative; together with 0–31 they are exactly the
values below 32. The quote, backslash and HTML-escape tests are unchanged.
The shared scanner and its one/two-byte shortcut remain intact.

Flynn recommended measuring this equivalent fold before changing dispatch.
The preliminary eight-placement test improved the 900 KB random case by
5.4–6.4%, with same-binary controls within 0.3%. No lookahead or rune-dispatch
rewrite was needed. The final `write_text` has 400 static instructions rather than
402. The mask-to-pointer dependency still exists: this is not a claim that
the stall or branch-miss mechanism inferred in VIBE-199 has been measured.

## Sources and controls

Measured on OTIS, Apple M4, Rust 1.98.1, normal release settings: thin LTO,
one codegen unit, opt-level 3, debug level 1 and overflow checks enabled.
Wasmtime is 48.0.2. All build output stays outside the checkout, and timing
runs do not overlap local builds or tests. Scheduling, PIE and ASLR remain
at their defaults; the linker controls static symbol addresses.

| Label | Source | Role |
|---|---|---|
| `legacy` | `79a9cee36fc5613c9e6673c0cfc9964bdf83189c` | Pre-VIBE-180 reference |
| `original` | `d9e3b9e7d79a561df374c3ecac0f3f14919a822a` | Corrected VIBE-180 reference |
| `head` | `9ead14d5` | VIBE-200 branch baseline |
| `signed` | `641fb461` | Same baseline with only the signed NEON comparison |

The old reference binaries and captured linker inputs were copied from
VIBE-199 and verified against its handoff hashes. Its committed tools,
fixtures and evidence were imported byte-for-byte before this work. The
intervening changes between `original` and `head` are not attributed to this
optimization: `head` versus `signed` isolates the runtime edit.

Both placement experiments use 32 isolated writer placements, four bytes
apart, plus 32 whole-code front pads, sixteen bytes apart. The relinker
asserts that every other symbol stays fixed in the isolated sweep and that
every text symbol moves by the requested amount in the front sweep. No
placement is selected for its timing result.

The two-way cohort is `head`, `signed`, and a same-binary `head` control.
The four-way cohort adds `legacy` and `original`, retaining the same control.
They use three and two measured rounds respectively, each after one full
warmup round. Every invocation also warms up 32 calls. Placement and variant
order are randomized with recorded seeds. Each complete group is rejected
if any sampled one-minute load is at least 6; rejected groups remain in the
evidence. Output digest, steps, peak and retained tracked bytes are checked
on every invocation.

The original focused counts remain 32,000 / 8,000 / 8,000 / 4,800 for fixed
words, random runs, prose and French. The 900 KB Russian guard runs 240
iterations at every placement. Two-way and four-way estimates are reported
separately, never pooled. Times are the median of per-placement minima;
paired changes are medians of matched invocation ratios.

## Two-way placement result

Three measured rounds give 96 matched groups per sweep. No group was rejected;
the peak sampled load was 3.20. All five signatures matched on every invocation.

| Sweep | Input | Head µs (range) | Signed µs (range) | Paired change | Control |
|---|---|---:|---:|---:|---:|
| local | Fixed words, 4 KB | 3.255 (3.055–3.421) | 3.148 (2.999–3.286) | -3.06% | +0.13% |
| local | Random 1–15, 40 KB | 29.190 (28.653–29.812) | 27.597 (27.303–28.163) | -5.62% | +0.38% |
| local | Prose, 40 KB | 24.366 (23.600–25.533) | 23.077 (22.147–24.314) | -5.62% | +0.55% |
| local | French, 40 KB | 24.712 (23.802–26.700) | 23.380 (22.622–25.029) | -5.83% | +0.14% |
| local | Russian, 900 KB | 1293.593 (1251.003–1333.768) | 1278.177 (1250.356–1324.942) | -0.39% | +0.00% |
| front | Fixed words, 4 KB | 3.168 (3.013–3.378) | 3.150 (3.017–3.266) | -1.39% | +0.06% |
| front | Random 1–15, 40 KB | 29.114 (28.749–29.658) | 27.579 (27.045–28.511) | -5.16% | -0.21% |
| front | Prose, 40 KB | 24.341 (23.569–25.380) | 22.791 (22.087–23.965) | -5.97% | -0.17% |
| front | French, 40 KB | 24.526 (23.780–26.222) | 23.117 (22.526–24.319) | -6.55% | +0.80% |
| front | Russian, 900 KB | 1278.438 (1251.097–1309.812) | 1266.934 (1250.020–1312.106) | +0.19% | -0.08% |

The random/prose gain appears in both placement modes. The one/two-byte
shortcut is unchanged, and the Russian guard stays within 0.4% of the head.
The single-layout Russian result (+2.13%, control +0.82%) does not survive
the placement comparison. Fixed-word improvements are smaller, especially
under front padding.

## Four-way placement result

Two measured rounds give 64 matched groups per sweep. No group was rejected;
the peak sampled load was 3.66. These results include both preserved references
in every group and are not pooled with the two-way experiment.

| Sweep | Input | Head µs (range) | Signed µs (range) | Paired change | Control |
|---|---|---:|---:|---:|---:|
| local | Fixed words, 4 KB | 3.218 (2.980–3.435) | 3.135 (3.009–3.260) | -3.08% | +0.38% |
| local | Random 1–15, 40 KB | 29.184 (28.700–29.717) | 27.477 (27.263–28.318) | -5.40% | -0.27% |
| local | Prose, 40 KB | 24.454 (23.552–25.575) | 22.799 (22.084–25.273) | -6.00% | -0.02% |
| local | French, 40 KB | 24.738 (23.887–27.418) | 23.262 (22.396–25.280) | -6.51% | +0.21% |
| local | Russian, 900 KB | 1292.625 (1253.175–1327.594) | 1288.652 (1248.250–1360.240) | -0.16% | -0.23% |
| front | Fixed words, 4 KB | 3.175 (3.040–3.358) | 3.180 (2.957–3.290) | -1.08% | +0.04% |
| front | Random 1–15, 40 KB | 29.202 (28.195–29.882) | 27.455 (27.065–28.376) | -5.10% | -0.12% |
| front | Prose, 40 KB | 24.160 (23.365–26.080) | 22.936 (22.139–24.917) | -5.83% | +0.10% |
| front | French, 40 KB | 24.647 (23.977–26.061) | 23.248 (22.531–27.442) | -4.31% | -0.07% |
| front | Russian, 900 KB | 1263.707 (1245.794–1313.206) | 1269.947 (1246.808–1315.411) | +0.50% | +0.04% |

Within this same four-way cohort, the preserved and new variants compare to
the legacy source as follows. These are directly matched ratios, not sums or
differences of percentages from different experiments.

| Sweep | Input | Original / legacy | Head / legacy | Signed / legacy |
|---|---|---:|---:|---:|
| local | Fixed words, 4 KB | -16.25% | -16.52% | -19.19% |
| local | Random 1–15, 40 KB | +3.56% | +4.15% | -1.50% |
| local | Prose, 40 KB | +0.21% | +1.30% | -5.86% |
| local | French, 40 KB | -5.09% | -6.21% | -11.34% |
| local | Russian, 900 KB | -0.83% | -1.03% | -0.95% |
| front | Fixed words, 4 KB | -15.75% | -17.94% | -18.13% |
| front | Random 1–15, 40 KB | +2.26% | +2.62% | -3.09% |
| front | Prose, 40 KB | -0.32% | -1.21% | -6.99% |
| front | French, 40 KB | -4.18% | -6.21% | -11.50% |
| front | Russian, 900 KB | -0.33% | -1.74% | -1.57% |

Random/prose gains survive both cohorts. The French estimate changes with
cohort and placement mode. Fixed-word front-pad minima are essentially
unchanged despite a small paired improvement; this is not a claim that every
summary statistic improves. The Russian short-run benefit is retained. The
smaller original/legacy random gap in this four-way cohort cannot erase the
larger two-way regression established by VIBE-199.

## Iteration-count experiment

Before changing the runtime, `iterations.py` used the preserved VIBE-199
baseline and candidate at local/front placements 1, 16 and 32. It varied
only the random/prose iteration counts (1,000, 2,000 and 8,000), keeping the
two-way cohort fixed. Four rounds randomized count/placement jobs and binary
order. Each placement first received a full 8,000-call warmup group, and
every measured process/case had the same 32-call warmup. All signatures
matched, with no load rejection.

The random-run paired changes at 1,000 / 2,000 / 8,000 were
+5.72% / +4.17% / +2.17% locally and +3.75% / +5.88% / +2.96% with front pads.
Controls were +2.95% / -1.61% / -4.39% locally and -0.85% / +0.77% / +0.31%
with front pads. Prose likewise showed no consistent increase with count.
This does not reproduce a monotonic count effect. Control variation prevents
crediting the smaller differences to iteration count, and this experiment
does not establish predictor training. All warmups, orders and samples are
retained; `summarize.py` also understands this experiment's raw format.

## Full-suite and cross-target timing

Seven measured rounds follow one warmup in each target run. All fifteen
signatures match; no groups were rejected. This table shows paired
`signed / head` changes, with same-binary controls in parentheses. Native
rows are single-layout guards, distinct from the placement sweeps.

| Input | Native SIMD | Native portable | WASI |
|---|---:|---:|---:|
| Words 4 KB | -1.38% (-0.76%) | +2.18% (-0.05%) | +0.25% (-0.41%) |
| Random 40 KB | -5.15% (+0.23%) | +2.64% (-0.15%) | +0.34% (-1.54%) |
| Prose 40 KB | -4.03% (-0.95%) | -0.35% (-0.90%) | +1.12% (+0.32%) |
| French 40 KB | -10.90% (+0.34%) | +0.30% (-0.51%) | +1.90% (+1.27%) |
| French 900 KB | -3.28% (+0.07%) | -0.77% (+0.62%) | -0.03% (-1.74%) |
| Russian 900 KB | +2.13% (+0.82%) | +0.26% (+1.31%) | -0.52% (-0.30%) |
| Random 900 KB | -4.75% (+0.34%) | -0.21% (+0.19%) | -1.19% (-0.42%) |
| Escaped prose 500 KB | +4.14% (-1.35%) | -0.31% (-1.26%) | +0.97% (+1.28%) |
| HTML prose 500 KB | +4.98% (+0.21%) | -0.06% (+0.81%) | +0.16% (+0.53%) |
| Parse ASCII 64 KB | -10.66% (+0.27%) | -0.72% (+0.24%) | +1.38% (-0.47%) |
| Stringify ASCII 64 KB | -8.53% (-1.46%) | -0.35% (+0.10%) | +0.45% (-0.01%) |
| Escaped 4 KB | +1.05% (+0.59%) | -0.20% (+0.06%) | -0.67% (-0.75%) |
| Parse French 900 KB | -1.88% (+1.25%) | +0.42% (-0.15%) | -0.76% (-0.49%) |
| Parse Russian 900 KB | -3.40% (+1.05%) | -1.85% (-0.13%) | +0.25% (-0.11%) |
| Parse random 900 KB | -1.31% (+0.74%) | +1.59% (+0.20%) | -0.62% (+0.11%) |

The portable random row varies by +2.64% despite identical instruction bytes.
WASI random/prose/French changes are +0.34% / +1.12% / +1.90% despite
identical non-custom sections. These timings are reported, not attributed
to the ARM64 source edit. The single-layout escape-heavy native regressions
receive a separate placement check below.

Relative to the legacy scanner, the existing portable tradeoff and WASI
gains remain. Times below are fastest µs/call; percentages are median paired
`signed / legacy` changes across all seven measured rounds.

| Input | Portable legacy → signed µs (paired change) | WASI legacy → signed µs (paired change) |
|---|---:|---:|
| Words 4 KB | 4.26 → 5.36 (+27.99%) | 5.81 → 4.86 (-15.91%) |
| Random 40 KB | 32.83 → 44.20 (+37.52%) | 48.25 → 32.85 (-37.20%) |
| Prose 40 KB | 30.01 → 35.08 (+16.11%) | 39.16 → 25.15 (-37.56%) |
| French 40 KB | 32.35 → 35.82 (+4.58%) | 41.96 → 27.08 (-35.63%) |
| Russian 900 KB | 1394.99 → 1254.43 (-10.09%) | 1600.52 → 1423.06 (-10.04%) |

## Escape-heavy placement sensitivity

The single-layout guard exposed a 4–5% slowdown on the two 500 KB escape-heavy
inputs. A separate two-way sweep therefore repeats all 64 placements, with
one full warmup and two measured rounds, at the original fixture counts.
No samples were rejected; peak sampled load was 3.75. All output/accounting
signatures match.

| Sweep | Input | Head µs (range) | Signed µs (range) | Paired change | Control |
|---|---|---:|---:|---:|---:|
| local | Escaped prose 500 KB | 693.413 (678.932–720.501) | 718.565 (706.649–758.349) | +3.50% | +0.20% |
| local | HTML prose 500 KB | 711.578 (688.686–799.156) | 723.649 (708.722–755.277) | -2.30% | +0.23% |
| local | Escaped 4 KB | 4.223 (4.166–4.331) | 4.231 (4.177–4.324) | -0.01% | +0.01% |
| front | Escaped prose 500 KB | 725.363 (687.765–768.542) | 694.354 (660.590–738.668) | -3.90% | -0.05% |
| front | HTML prose 500 KB | 733.882 (694.262–776.698) | 710.980 (664.431–785.676) | -1.01% | +0.46% |
| front | Escaped 4 KB | 4.232 (4.160–4.319) | 4.236 (4.176–4.340) | -0.21% | +0.01% |

The local escaped-prose result is +3.50% and the front result is −3.90%,
with controls close to zero. Local HTML minima worsen despite improved paired
ratios. Both summaries and both modes are retained. A fresh phase check below
examines why the local escape sweep does not vary the relevant hot layout.

## Fresh four-byte phase confirmation

Source and fixture inspection establishes that all three escape fixtures
are ASCII-only. `write_string` handles their ordinary runs through `prefix`
and continues before `write_text` (`src/json/writer.rs:599–606`). Therefore,
moving only `write_text` does **not** vary their hot-code layout: the local
escape rows repeat one relevant layout, rather than testing 32 of them.

After inspecting the coarse sweep, Flynn suggested a 128-byte pattern. That
was a post-hoc observation, so this fresh run uses seed 207 and whole-code
front pads of 4, 8, …, 128 bytes. It covers every phase in one complete cycle,
with one full warmup and three measured rounds per phase. The original
16-byte front sweep covers four such cycles. No phase is selected or omitted.

The finer sweep reproduces the broad split: head is faster in the first
half of the cycle and signed is faster in the second. Full-cycle arithmetic
mean of matched signed/head ratios is **−2.68%**, with **+0.10%** for the
same-binary control. Mean call times are 687.52 → 667.98 µs. These are
different summaries and are not substituted for the earlier sweep medians.
All 96 measured groups pass output/accounting checks; none were rejected.

Every phase is reported below. Times and paired ratios are arithmetic means
of its three measured rounds; the 128-byte pad returns to phase zero.

| Pad bytes | Head µs | Signed µs | Mean paired change | Mean control |
|---:|---:|---:|---:|---:|
| 4 | 658.16 | 699.56 | +6.29% | +0.43% |
| 8 | 673.58 | 700.27 | +3.97% | +0.26% |
| 12 | 666.57 | 673.77 | +1.08% | -1.01% |
| 16 | 666.24 | 684.82 | +2.80% | +0.91% |
| 20 | 660.86 | 680.70 | +3.02% | +0.07% |
| 24 | 673.82 | 685.33 | +1.71% | -0.01% |
| 28 | 674.90 | 695.31 | +3.03% | -0.17% |
| 32 | 681.30 | 682.87 | +0.23% | -0.84% |
| 36 | 665.72 | 681.80 | +2.42% | -0.45% |
| 40 | 672.26 | 664.63 | -1.13% | -0.02% |
| 44 | 656.95 | 662.70 | +0.88% | +0.80% |
| 48 | 663.44 | 675.89 | +1.88% | +0.41% |
| 52 | 660.93 | 665.96 | +0.76% | +0.42% |
| 56 | 667.02 | 684.98 | +2.69% | +0.14% |
| 60 | 657.86 | 678.23 | +3.09% | +0.49% |
| 64 | 712.16 | 658.72 | -7.50% | +2.08% |
| 68 | 702.79 | 645.28 | -8.19% | +0.66% |
| 72 | 716.92 | 652.37 | -9.00% | +1.05% |
| 76 | 708.47 | 649.69 | -8.29% | +1.02% |
| 80 | 727.75 | 655.43 | -9.93% | -0.83% |
| 84 | 698.50 | 646.35 | -7.45% | -0.81% |
| 88 | 716.69 | 655.00 | -8.58% | -0.88% |
| 92 | 700.23 | 669.11 | -4.44% | -0.24% |
| 96 | 707.13 | 649.32 | -8.15% | +0.48% |
| 100 | 694.66 | 651.34 | -6.23% | +0.36% |
| 104 | 704.62 | 658.70 | -6.52% | +0.01% |
| 108 | 709.78 | 648.71 | -8.60% | -1.36% |
| 112 | 708.55 | 656.77 | -7.29% | +1.36% |
| 116 | 701.58 | 656.71 | -6.38% | +1.03% |
| 120 | 711.58 | 651.09 | -8.47% | -0.07% |
| 124 | 698.41 | 653.08 | -6.48% | -0.27% |
| 128 | 681.30 | 700.80 | +2.89% | -1.65% |

The as-built +4.14% and fixed-hot-layout +3.50% observations remain real
measurements, but they do not establish a generally slower escape algorithm.
The fresh cycle mean is also not a promise about every linked executable.
The native mixed-text gains hold in both placement modes and both cohorts.
This investigation does not identify the responsible hot function, branch,
cache event or stall. HTML prose has substantial within-binary variation
(including 15.95% at one measured front placement), so its conflicting
minima/paired estimates are retained without a causal interpretation.

Further hot-function/layout isolation is tracked separately as VIBE-201.

## Correctness and cross-target checks

`guards.json.gz` holds the exact original twelve full-suite payloads, with
the longer focused counts restored, plus French/Russian/random 900 KB parser
cases constructed from those same payloads. The parser cases exercise the
shared classifier's other callers. `validate.py` compares complete output
strings and captured stdout/stderr across all nine builds, and compares
steps and peak/retained tracked bytes within each target. All fifteen cases
match. Pointer-width-dependent accounting is not compared across targets.

The portable native instruction section is byte-identical before/after
(SHA-256 `c7dd9007dcbae3464be3733ba2da82c9b6528f59e14dfd2b5351cb1365579c56`).
All non-custom WASM sections are byte-identical
(`055eaf1898a28a5abdaafdbc82c22393f20bc01d614a94328ee56b33916d6ccd`).
Debug metadata accounts for the differing whole-file hashes. x86 SIMD code
is unchanged and already uses the signed comparison; it was not timed on
this ARM64 machine.

The full workspace gate-profile suites pass with SIMD/all features (2,562
tests) and without default features (2,520 tests). The existing classifier
tests compare every byte at every lane to scalar classification, and the
prefix tests also cover unaligned slices. String-span, invalid-UTF-8 and
JSON quota-boundary tests pass.
Formatting and library Clippy with all features pass. No load width, unsafe
boundary, UTF-8 rule or accounting operation changes.

## Reproduction

Use the capture/build/relink instructions in [README.md](README.md), with
the sources above, giving each build its own target directory. Name the
native build directories `current` and `signed`, and the placement manifests
`placements/head`, `placements/signed`, `placements/base` (legacy) and
`placements/current` (original). Keep each native executable at
`BUILD/current/compare`, `BUILD/signed/compare`; copy the legacy executable
to `BUILD/base`. The preserved originals are independently identified by
their hashes in the evidence; rebuilding does not recreate their full link.

For each of current/signed/legacy, build portable with
`CARGO_TARGET_DIR=BUILD/LABEL-portable cargo build --locked --release
--example compare --no-default-features`. Build WASI into
`BUILD/LABEL-wasi` with the matching 1.98.1 compiler and target libraries:

```sh
CARGO_TARGET_DIR="$BUILD/$LABEL-wasi" \
CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true \
cargo build --locked --release --manifest-path "$TREE/tests/platforms/wasi/Cargo.toml" \
  --target wasm32-wasip1 --bin vibescript-wasi-compare
python3 "$TOOLS/prepare200.py" "$BUILD" --wasmtime "$WASMTIME" --cache-config "$CACHE_CONFIG"
python3 "$TOOLS/validate.py" "$BUILD/guards.json" "$BUILD/validation-groups.json" "$BUILD/validation.json"
python3 "$TOOLS/iterations.py" "$TOOLS/cases.json" "$BUILD/iterations.json" \
  "base=$BUILD/placements/base/variants.json" "current=$BUILD/placements/current/variants.json"
sh "$TOOLS/run200.sh" "$BUILD"
python3 "$TOOLS/summarize.py" "$BUILD/two-way.json"
python3 "$TOOLS/summarize.py" "$BUILD/four-way.json"
python3 "$TOOLS/summarize.py" "$BUILD/iterations.json"
python3 "$TOOLS/phase200.py" "$BUILD" "$HEAD_LINK" "$SIGNED_LINK"
```

`prepare200.py` expands the exact compressed guard fixtures and writes the
target command arrays; `measure.py` passes those arrays directly without a
shell. `run200.sh` runs the native guards, two-way sweep, four-way sweep,
portable guards, WASI guards and escape-heavy placement check sequentially.
Run it only after all builds,
tests and relinks finish. Each cross-target run has seven measured rounds
after a warmup. The native guard cohort is two-way; portable and WASI add
the legacy build to measure preservation of the earlier target-specific
tradeoff, and their estimates are kept separate from the placement cohorts.

`phase200.py` recreates the fresh four-byte front sweep. `HEAD_LINK` and
`SIGNED_LINK` are the captured linker JSON files for those native builds.
It uses only the original escaped-prose fixture and asserts that it is ASCII.

`code_hash.py BINARY...` reproduces the instruction/non-custom-section hashes.
The native preliminary probe used fixed words and random 900 KB at local and
front placements 1, 8, 16 and 32, five measured rounds after one warmup.
The full raw probe is retained even though it was only an admission gate.

The [raw evidence archive](optimization-evidence.json.gz) includes every
accepted/rejected and warmup sample, order, load, signature, placement,
binary hash, both writer disassemblies, code-identity audit and test summary.
The exact full-suite payloads are in [guards.json.gz](guards.json.gz); the
focused payloads remain in [cases.json](cases.json). Archive a completed run:

```sh
python3 "$TOOLS/archive200.py" "$BUILD" "$TOOLS/optimization-evidence.json.gz"
```

The archiver rejects incomplete/duplicate placement groups or accepted
samples that violate the load cutoff. This archive includes the original pilot,
handoff, environment, test summary and disassembly. Fresh reproductions may
omit that session-specific context; absent optional metadata is explicitly
listed instead of copied from this run. To summarize an archived experiment,
extract its `two-way`, `four-way`, `iterations`, `escape-sweep`, or `phase4` JSON member
and pass it to `summarize.py`. The retained build directory for this run is
`/Volumes/AI/Build/tron-vibe-200`; binaries are not committed to the repository.
