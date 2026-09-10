# String scans and executable placement

Issue [#1309](https://github.com/xipkit/vibescript/issues/1309) asked why
unchanged string scans moved between timing clusters on x86. Controlled
function placement reproduces this effect in ordinary embedding executables.
Removing unnecessary scans also makes default `rindex` substantially faster,
without adding a decoder, cache, allocation, or accounting exception.

The production comparison is `25b77777f8b802adebee760c106868c92758ae92` to
`cc45416913f6ccb870f817ea9f6390d90fbfecd0`. All comparisons use matching
fixtures and build settings within each toolchain and experiment.

## What causes the timing changes

An initial [AMD EPYC 7763 experiment](https://github.com/xipkit/vibescript/actions/runs/34514459001)
built ordinary public-API embedding programs with Go 1.27.1, using the normal
linker layout and 16 randomized layouts. The generated rune-count instructions
were unchanged, but the direct Unicode counter varied from roughly 2.9 to
5.1 microseconds. Public Unicode length commonly clustered around 0.81–0.89 ms
or 0.98–1.03 ms, correlated with the length closure's address modulo 64;
the raw samples also contain larger outliers.
[Diagnostic source](https://github.com/xipkit/vibescript/tree/71ae12eab6e7ac86d05c35f3ae05574b976d87e0/scripts)
and the run artifacts preserve the layouts, instructions, and measurements.

Correlation alone does not isolate a cause. A second
[experiment on Intel Xeon 6973P-C](https://github.com/xipkit/vibescript/actions/runs/34515806414)
moved one chosen function inside a fixed 64-byte reservation. A diagnostic
linker overlay compensated after that function and asserted that **every other
text symbol kept its original address**. This excludes unrelated hot functions
moving as the explanation for those comparisons. Relocations and PC metadata
still change as required; this is not a claim that every byte in the executable
is identical.

The experiment covered the length closure, `runtime.decoderune`, and
`unicode/utf8.ValidString`, offsets 0/16/32/48, and default, SIMD, and
AVX2-disabled builds. All 216 sample invocations completed. The following
0-to-32 comparisons preserve Go's ordinary 32-byte function alignment:

| Function moved, SIMD build | Unicode length | Unicode index | Unicode rindex |
|---|---:|---:|---:|
| Length closure, +32 bytes | 584.8 → 541.0 µs, −7.50% | 1.009 → 1.012 ms, no significant change | 1.775 → 1.779 ms, no significant change |
| UTF-8 validation, +32 bytes | 557.0 → 564.1 µs, no significant change | 1.017 → 1.160 ms, +14.03% | 1.764 → 1.885 ms, +6.86% |

Each cell uses six repetitions of one fixed executable per placement.
[Length](length-shift-benchstat.txt) and
[validation](validation-shift-benchstat.txt) tables include allocations and
statistical results; their four input files are retained here. Moving validation
affects searches that call it, while moving the length closure affects length.
The preferred placement is not universal across machines and build variants.

This establishes hot-function placement as a concrete cause of scan timing
changes, independently of the earlier JSON and clone changes. The VM did not
expose usable hardware PMU counters, even through privileged `perf`, so it does
**not** identify a particular instruction-cache, alignment, or branch-predictor
stall. The
[diagnostic linker source](https://github.com/xipkit/vibescript/blob/57b57a279056ff0cd534a646bf8c936124857aa8/scripts/rune_alignment.py)
stays outside the production branch. No production padding or alignment hint
is used to fit a benchmark.

## Structural improvement

Default reverse search previously counted all receiver runes just to obtain
an upper bound. The valid-Unicode path then counted the full receiver and
needle, found the end boundary, searched backwards, and counted the result's
prefix. The byte length already bounds the rune length. With an unbounded
offset and validated UTF-8, `strings.LastIndex` can search directly, followed
by just the prefix count needed for the public rune offset.

Explicit bounded and negative offsets retain their existing search behavior.
The member builtin also avoids computing a default that an explicit offset
would immediately replace. Malformed UTF-8 still takes the original
scratch-accounted canonicalization path. Empty needles, missing matches,
oversized offsets, and the receiver-bounded needle rejection retain their
existing semantics.

Custom width counters and validate-then-count prototypes were also evaluated.
Some improved mixed text or CJK but regressed long ASCII prefixes by roughly
30%; other word-assisted variants regressed mixed Latin text or malformed
suffixes. Those input-dependent tradeoffs do not justify a second decoder.
The production change instead removes redundant work using existing primitives.

## ARM measurements and remaining variation

Apple M4, darwin/arm64, `GOMAXPROCS=1`; Go 1.26.3 uses `nosimd`, Go 1.27.1
uses `simd`. Ten alternating repetitions of prebuilt package-test binaries,
100 ms per case, cover 60 complete public-call cases, with no concurrent local
builds or benchmarks. These existing controls each perform 200 searches:

| Workload | Go 1.26 before → after | Go 1.27 SIMD before → after |
|---|---:|---:|
| ASCII rindex | 226.3 → 166.5 µs, −26.40% | 171.9 → 141.3 µs, −17.79% |
| Unicode rindex | 1198.2 → 640.1 µs, −46.58% | 1130.8 → 643.2 µs, −43.12% |

Allocation-count medians are unchanged in all 60 cases. This is a CPU saving,
not a retained-memory reduction. The complete
[Go 1.26](go126-benchstat.txt) and [Go 1.27](go127-benchstat.txt) tables preserve
adverse controls too: unchanged Unicode length increases 30.34% and 23.84%,
respectively; Go 1.26 negative Unicode rindex increases 11.33%, and Go 1.27
Unicode slice increases 3.65%. There are additional small malformed-input
and mixed-prefix control increases. The ARM `stringRuneLen` instruction
sequences are identical in the retained base/head disassemblies, while the
function address changes. One package-test binary is insufficient evidence
for a general cost claim in either direction.

The ordinary embedding driver therefore measured 13 layouts on the M4 with
Go 1.26.3: seed 0 uses the regular linker, and seeds 1–12 independently vary
function placement. There are 26 binaries and 78 validated invocations, each
with 28 public-call cases. The table uses **one median per layout**, then the
median of the 12 paired percentage changes; repeated samples within one
layout are not counted as independent layouts. Seed 0 is kept separate.

| Ordinary executable workload | Median paired change, seeds 1–12 | Range across those layouts |
|---|---:|---:|
| ASCII length | +0.11% | −0.74% to +0.86% |
| Unicode length | −0.28% | −2.80% to +6.50% |
| Unicode index | −0.01% | −0.42% to +17.46% |
| ASCII rindex | −27.12% | −27.85% to −25.92% |
| Unicode rindex | −45.91% | −47.17% to −35.16% |
| Unicode slice | +0.05% | −0.73% to +25.79% |
| Explicit negative Unicode rindex | +1.02% | −2.52% to +9.46% |

The ordinary seed-0 Unicode length result is 485.4 → 478.9 µs and rindex is
1206.0 → 643.9 µs. The large package-binary length shift does not generalize to
these ordinary executables; the reverse-search improvement survives every
layout. Individual controls still vary substantially, including the large
index/slice outliers above. No samples or unfavorable layouts were discarded.
All 28 allocation-count medians match in every layout.
[All cases and ranges](arm-layouts/summary.json), raw samples, environment,
source fingerprints, and binary hashes are in [arm-layouts](arm-layouts).

The [native Go 1.27 comparison](https://github.com/xipkit/vibescript/actions/runs/34519310933)
uses the same ordinary embedding driver and production commits on ARM64 and
x86-64, including matched default/SIMD builds and x86 AVX2-disabled runs.
The PR's native benchmark review records those results and any remaining
variations separately from these local measurements.

## Behavior and accounting verification

An independent rune-slice oracle covers arbitrary text/needle/offset triples,
including malformed UTF-8 and maximum integer offsets. Tests exercise both
direct public calls and member dispatch. Thirty seconds of fuzzing and both
full suites pass with all three verification modes enabled:
`VIBES_ESTIMATOR_VERIFY`, `VIBES_ENV_RECYCLE_VERIFY`, and
`VIBES_BUILTIN_CONTRACT_VERIFY`. Both toolchains pass vet; lint, the benchmark
smoke gates, and the SIMD-profile tests also pass.

The temporary [accounting probe](accounting-probe.go.txt) compares 72 workloads
per toolchain. All **144 base/head snapshots are byte-identical**, including
results, minimum step and memory quotas, probes on either side of those
boundaries, and cancellation errors/poll counts. The four accounting outputs
are retained here. The same probe was present in both package-test binaries;
it was removed afterward and is not compiled into production or ordinary
embedding executables.

## Reproduce ordinary executable comparisons

Use isolated base/head checkouts and the head's reviewed fixtures:

```sh
python3 -B scripts/bench_string_layout.py \
  --base /tmp/vibescript-before --head /tmp/vibescript-after \
  --output /tmp/vibescript-layout-results \
  --toolchain go1.27.1 --experiments nosimd,simd \
  --seeds 0,1,2,3,4,5,6,7,8,9,10,11,12 --count 3 --benchtime 100ms
```

The output directory must be new. The driver builds external embedding
modules using public APIs, excludes package tests, alternates base/head order,
pins a Linux CPU, validates all 28 cases in every invocation, verifies that
AVX2 is disabled for fallback samples, and rejects production-source changes
during measurement. It retains fixture/source/binary hashes and raw samples.
The linker's `-randlayout` is a diagnostic flag; use the pinned toolchain and
keep ordinary seed-0 results distinct from randomized-layout summaries.

The [package measurement manifest](manifest.json) identifies the production
commits, fixture/probe hashes, and all 40 sample invocations. Existing short
and mixed-prefix controls are included there; the ordinary driver intentionally
uses the eight existing length/search/slice controls and 20 offset cases.
