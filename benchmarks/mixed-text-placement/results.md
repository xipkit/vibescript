# VIBE-199: separating shortcut cost from placement

This is an investigation of the VIBE-180 scanner candidate, not a runtime
patch. The native SIMD regression survives controlled code placements at the
original confirmation counts. The shortcut and mask-dependent scan have
input-dependent costs; moving code is not a general remedy. Removing the
short-run shortcut trades a random-run gain for substantial prose and Unicode
regressions. The shared scanner should not be reverted or its short-run path
removed on this evidence.

## Controlled comparisons

The baseline is `79a9cee36fc5613c9e6673c0cfc9964bdf83189c`; the corrected
candidate is `d9e3b9e7d79a561df374c3ecac0f3f14919a822a`. Only `scan.rs` and
two ASCII-scanner callers differ between them. The JSON writer source is
identical. Both were rebuilt on OTIS (Apple M4), Rust 1.98.1 / LLVM 22.1.8,
with the normal release settings and native SIMD enabled.

The original binary pair is retained separately from the rebuilds. Rebuilding
moves `write_text`, `write_string`, `rune_width`, and the relevant prefix-loop
helper by 60 bytes relative to the preserved executables. The original and
rebuilt `write_text` instruction sequences match after relocations; its static
instruction count and register allocation are unchanged within each source
version. Other functions and constant addresses also move. This is why the
rebuilds alone cannot confirm or dismiss the recorded regression.

Four source variants separate the effects:

- `base`: the original boolean block test followed by scalar rescan.
- `base-short`: the same source with only the candidate's one/two-byte shortcut.
- `no-short`: the candidate with only that shortcut removed.
- `candidate`: the unmodified candidate, mask plus shortcut.

Each variant has 32 placements moving only `write_text` in four-byte steps,
with every other symbol fixed, and 32 placements moving all text symbols in
sixteen-byte steps. All 256 binaries pass the symbol-address audits. The local
sweep places the writer at `0x1000008c4` through `0x100000940`; front padding
covers 512 bytes while preserving function alignment. These are sampled
address ranges, not a proof of immunity to every possible link layout.

The first four-variant sweep has six measured randomized rounds after one
complete warmup round, using one quarter of the confirmation iterations.
It is followed by a three-round baseline/candidate sweep and a two-round
four-variant sweep at the full confirmation counts, on the already-warmed
binaries.
The first round exposed a first-invocation artifact in newly linked binaries:
the first small fixed-word case was often much slower than the second
invocation of the exact same executable. That round is retained in the raw
data and excluded uniformly from all timing summaries. Subsequent control
measurements are reported alongside the results. No timing run overlaps the
local compilation jobs.

## Preserved-binary confirmation

Fifteen measured rounds after a warmup, using the original longer iteration
counts. The recorded regression still reproduces, with a smaller median gap
than the earlier run. Times are fastest µs/call; both ratio conventions are
shown rather than treating one sample as the result.

| Input | Baseline → candidate, µs | Fastest / paired change | Paired control |
|---|---:|---:|---:|
| 8-byte words, 4 KB | 4.02 → 3.34 | -16.9% / -14.3% | -0.3% |
| Random 1–15, 40 KB | 28.55 → 31.21 | +9.3% / +7.0% | -0.4% |
| Prose runs, 40 KB | 24.33 → 25.28 | +3.9% / +4.0% | -0.3% |
| French, 40 KB | 26.47 → 25.34 | -4.2% / -3.7% | -1.2% |

## Placement distributions

Each time is the median of the 32 per-placement minima, with the complete
range in parentheses (µs/call). Paired change is the median across all 96
matched measured groups in that sweep. Minima and paired ratios answer
different questions and are not substituted for one another.

| Sweep | Input | Baseline µs (range) | Candidate µs (range) | Paired change | Control |
|---|---|---:|---:|---:|---:|
| local | 8-byte words, 4 KB | 3.96 (3.90–4.05) | 3.34 (3.23–3.53) | -14.90% | +0.12% |
| local | Random 1–15, 40 KB | 28.85 (27.70–29.93) | 30.76 (29.94–32.43) | +8.33% | +0.05% |
| local | Prose runs, 40 KB | 25.00 (24.12–25.72) | 25.83 (24.72–27.84) | +4.43% | +0.12% |
| local | French, 40 KB | 27.06 (25.84–28.23) | 26.39 (24.85–28.72) | -0.45% | -0.56% |
| front | 8-byte words, 4 KB | 3.97 (3.90–4.10) | 3.35 (3.11–3.49) | -14.45% | -0.16% |
| front | Random 1–15, 40 KB | 28.95 (28.05–30.82) | 30.89 (29.92–32.31) | +7.96% | -0.09% |
| front | Prose runs, 40 KB | 25.12 (24.22–25.99) | 26.05 (24.64–27.73) | +3.68% | -0.20% |
| front | French, 40 KB | 27.17 (26.19–28.19) | 26.28 (24.88–29.35) | -2.23% | -0.35% |

This confirmation uses the original longer iteration counts, three measured
rounds at each of 64 placements, and binaries already warmed by the earlier
sweep. The earlier four-variant sweep used one quarter as many iterations
and reported only about 3% for random runs and under 1% for prose. Those
smaller estimates cannot be used to dismiss the original pair. The longer
four-way comparison also yields a smaller gap, so iteration count alone
does not explain the difference. Keep the cohorts separate; do not combine
their percentages into a causal accounting of the original regression.
No favorable single placement is credited as a fix.

### Shortcut ablation

Two further rounds at all 64 placements use the original longer counts for
all four variants. Median paired change when adding the identical shortcut:

| Input | Boolean/scalar, local / front | Mask, local / front |
|---|---:|---:|
| 8-byte words, 4 KB | +4.73% / +5.18% | +11.45% / +10.47% |
| Random 1–15, 40 KB | +5.22% / +5.92% | +6.04% / +6.49% |
| Prose runs, 40 KB | +3.17% / +1.79% | -17.70% / -17.61% |
| French, 40 KB | +0.71% / +0.83% | -21.20% / -22.66% |

The shortcut has a measurable cost on fixed words and random runs and a
large benefit on prose and French. Removing it is not a broadly safe
optimization. These are net compiled effects, including the extra branches
and the change in register allocation and relative branch layout.

### Retired instructions

Median differential process counts, instructions per call rounded to the
nearest thousand. Counts use N and 3N calls, three alternating repetitions.

| Input | Base | Base + shortcut | Mask only | Candidate |
|---|---:|---:|---:|---:|
| 8-byte words, 4 KB | 118k | 121k | 72k | 76k |
| Random 1–15, 40 KB | 879k | 891k | 435k | 468k |
| Prose runs, 40 KB | 719k | 721k | 419k | 445k |
| French, 40 KB | 755k | 743k | 458k | 480k |

On prose, mask-only retires about 42% fewer instructions than the baseline
yet takes about 22% longer. The shortcut increases its instruction count
while making it 17–18% faster. Together with the dependent mask-to-pointer
chain in the disassembly, this supports latency on short runs as a cost;
instruction volume alone does not explain it. This is an inference, not a
direct stall-counter measurement. Counts at six selected local/front
placements per variant differ by at most 0.23% on random/prose inputs,
consistent with the same work being retired at different addresses.

## Portable, WASI, and short-run guards

Nine measured rounds after a warmup, on unmodified baseline/candidate
rebuilds, outside the placement sweep. Times are fastest µs/call; parentheses
show median paired change and the same-binary control. These checks do not
claim a portable placement sweep or a new portable optimization.

| Input | Native portable: before → after (change; control) | Wasmtime release: before → after (change; control) |
|---|---:|---:|
| 8-byte words, 4 KB | 4.50 → 5.45 (+25.9%; +0.4%) | 5.96 → 4.97 (-16.6%; -0.4%) |
| Random 1–15, 40 KB | 34.22 → 47.95 (+39.2%; -0.5%) | 49.72 → 33.83 (-36.3%; -1.1%) |
| Prose runs, 40 KB | 31.04 → 37.84 (+17.5%; -2.9%) | 40.93 → 26.09 (-37.1%; -0.9%) |
| French, 40 KB | 34.26 → 38.87 (+13.0%; -0.9%) | 43.45 → 27.99 (-37.1%; -1.8%) |
| Russian, 900 KB | 1481.65 → 1348.89 (-7.9%; +0.3%) | 1660.79 → 1497.10 (-10.0%; -0.0%) |

The native SIMD Russian guard is -1.2% for the candidate, but
+32.7% with the shortcut removed (control +0.3%).
WASI retains its substantial gains. Native portable retains the previously
documented slowdown on the mixed Latin inputs; this investigation does not
claim to fix it.

Output bytes match across all eight full-suite variants (four native SIMD,
two portable, two WASI), for all twelve original fixtures. Steps, peak tracked
bytes and retained tracked bytes match baseline within each configuration
and on every timed invocation. Accounting across native and WASI is compared
within its platform, rather than conflating pointer-width-dependent sizes.

## Assembly and interpretation

| Build | `write_text` instructions | Stack bytes |
|---|---:|---:|
| base | 386 | 208 |
| base-short | 424 | 208 |
| no-short | 348 | 192 |
| candidate | 402 | 192 |

The original block test ends in `umaxv; fmov; cbz`, then rescans bytes with a
scalar loop. The mask path ends in `shrn; fmov; cbz; rbit; clz; lsr`. Its count
feeds the slice/pointer advance and hence the next rune load. The short-run
shortcut tests the next one or two bytes before that dependency; it adds
branches and classification work when the run continues.

One/two-byte runs are 0% of the fixed-word payload, 13.9% of random 1–15 runs,
19.0% of prose runs, 28.5% of French runs, and 100% of the Russian diagnostic's
ASCII runs. The shortcut's value is therefore strongly input dependent.
The ablations measure its net compiled effect, including register allocation
and branch layout. They do not isolate a single instruction's latency or
measure branch misses, and no predictor-index explanation is claimed.

## Execution-context and profiling controls

The four-way full-count study reads +3.18% / +2.50% for random runs
(local / front), versus +8.33% / +7.96% in the two-way study. The shortcut
comparisons are valid within their cohort; they cannot be added to or
subtracted from the two-way result to explain every percentage point.

A follow-up alternates two-way (A) and four-way (B) groups in `ABBAABBA`
order at the same six preselected placements (1, 16, 32 in each sweep),
with identical full-count inputs. Each phase covers all six placements.

| Phase | Cohort | Random paired change | Prose paired change |
|---|---|---:|---:|
| 01 | A | +6.76% | +1.85% |
| 02 | B | +5.61% | +0.26% |
| 03 | B | +7.00% | +2.10% |
| 04 | A | +8.72% | +1.61% |
| 05 | A | +7.35% | +2.76% |
| 06 | B | +4.79% | -0.07% |
| 07 | B | +2.07% | +3.14% |
| 08 | A | +5.79% | +2.88% |

Across those phases, random-run change is +6.93% for A and +5.61% for B;
the corresponding baseline controls are +0.94% and +0.09%. Prose is +2.03%
and +1.41%, with controls -0.39% and -0.41%. This does not consistently
reproduce the earlier five-point cohort gap or establish interleaving as its
sole cause. It confirms that a single low figure should not be promoted as
the algorithm cost. Host load, execution history, and scheduling were not
individually controlled; every accepted group stayed below the load cutoff.
The executables retain PIE and default scheduling; only their static linker
addresses were deliberately controlled.

A separate 60-byte front pad shifts all 5,643 baseline / 5,644 candidate
text symbols by exactly 60 bytes, matching the preserved hot-function
addresses. On random runs, the rebuilt pair reads +7.38% and the padded
pair +7.45% (nine rounds, full counts). That one shift does not remove the
regression. Other addresses and constants still differ from the preserved
binaries; matching a few hot addresses does not reconstruct the entire link.

Two 15-second Instruments CPU Counters recordings were attempted. Each
produced only one process/core interval near startup, rather than usable
hot-loop coverage, before the time limit stopped the target. They are
excluded from the diagnosis. No measured branch-miss count, stall-cycle
attribution, or specific predictor mechanism is claimed. The mask latency
interpretation rests on the assembly dependency and the instruction/time
contrast, with the limitations above.

## Decision

This establishes a real native SIMD tradeoff, not a link-only accident:
shortcut dispatch costs time on random runs, while bypassing the dependent
mask count is valuable on very short runs. The exact stall/branch-miss split
is not established, and context-sensitive timing prevents attributing every
point of the original regression to one instruction group.

No runtime change is justified by deleting the shortcut or selecting a lucky
link placement. A separate optimization should retain the short-run behavior,
reduce the long-run dispatch/dependency cost, and pass the full-count
placement tests, the Russian guard, and native portable/WASI comparisons.
That implementation work is separate from this investigation.

## Validation and artifacts

The candidate's nine shared scanner tests plus three JSON-scanner tests pass
with both native feature configurations. The ablation tool round-trips both
source versions byte-for-byte. Python syntax checks pass; the relinker and
measurement tools are exercised by the complete experiment.

The reproducible tools, exact four focused fixtures, and compressed raw
[evidence bundle](evidence.json.gz) are in this directory;
see [the reproduction instructions](README.md). Large binaries, captured link
objects, full twelve-case input, test logs and complete disassembly remain at
`/Volumes/AI/Build/tron-vibe-199/investigation`. The original evidence is kept
in its parent directory. None of the diagnostic source edits are included in
the runtime or this PR.
