# Short-span batching reduces the measured lone-escape band

The physical OTIS comparison supports advancing runtime candidate
[`a7daf27b`](https://github.com/xipkit/vibescript/commit/a7daf27b8386d2565fce32252a3390d811e64a11)
against baseline
[`7ab5ea12`](https://github.com/xipkit/vibescript/commit/7ab5ea12a4b87a438789a2a347b4a0d399a05f4d)
to review for VIBE-187. Across both 32-placement writer sweeps, lone-escape
median minima fall about 37% in SIMD and 50% in portable builds. The writer's
normalized placement band falls from 4.87–5.15% to 1.51–1.96% in SIMD and
8.56–8.65% to 1.58–1.63% in portable builds. Every head placement minimum is
below every baseline placement minimum in both independent axes and seeds.

ASCII-line median minima change by -0.009 to +0.011 µs, and Unicode by
-0.015 to +0.007 µs. The helper sweep does not reproduce the large portable
helper band of [rejected b9380440](b9380440.md). All untrimmed phase means and
raw rounds remain available, including a large head-helper outlier described
below. This is evidence about the specified isolated-placement experiment;
it does not establish a hardware predictor mechanism or arbitrary joint
writer/helper layout robustness. No crossed experiment was run.

## The intervention and spill audit

Repeated `ab` + LF previously alternated between the ordinary-span scanner
and in-place escape loop, carrying the output length and deferred charge
through memory on each turn. The source intervention batches those complete
short spans and escapes in the existing bounded helper. Its cursors and
step count stay local until the block is appended. It reuses the outer
scanner's computed span length for entry and probes/copies at most eight
ordinary ASCII bytes per span inside the helper. Long spans and Unicode
retain their scanner, whole-span append charges and growth boundaries.
This identifies the source-level loop interaction being changed; it does
not identify which hardware predictor event caused the earlier spread.

A native sample of the exact SIMD head confirms that the lone-escape payload
executes the expanded helper, including the short-span path. Sampling ended
before timing, and its binary checksum was verified after the full run.
Both baseline dependencies (#1401 and #1404) are in the measured baseline.
The release profile and Rust 1.98.1 compiler are unchanged.

In the physical ARM64 dumps, both modes' helper frames remain 0xd0 bytes,
matching baseline. The escape and short-copy loop bodies have no loop-state
stack reload or shared-prefix call. The 14 stack references in each helper
are saves/restores, block initialization and the reserve slow path. The
portable mask reloads introduced by b9380440 are absent. Prefix and Unicode
helpers retain only their entry/exit stack references. `write_text` has the
same frame and stack instructions as baseline in each mode.

Outer writer frames remain 0x120 in SIMD and 0x160 in portable. Existing
pending charges and the saved settle cursor remain on the stack outside the
buffered run. The new helper return adds one pending load/store and one
restore of that existing cursor; it introduces no new spill slot. Two added
stores construct a cold bounds-error result. There are still cursor restores
on paths into `write_text`; this is not a claim that the outer writer is
spill-free. The measured Unicode instruction medians remain within 0.08%
of baseline across the two axes.

## Protocol and complete timing summaries

Dumont collected the data for VIBE-205 on physical Apple M4, macOS 27.0.1,
Rust 1.98.1 / LLVM 22.1.8. [His collection record is preserved separately](https://github.com/xipkit/vibescript/blob/eb6206094238d360175aa09f4be523ae958ecd4e/benchmarks/vibe-205-otis-a7daf27b.md).
Each SIMD/portable × base/head build has two independent sweeps: move only
`write_string` (`string`), then only `Output::put_run` (`run`), four bytes
at a time through 128 bytes. Every other original symbol stays fixed within
a sweep. Comparisons match actual selected-function addresses modulo 128;
the manifests record both hot-function addresses. The fixed partner address
can differ between base and head. There are 256 relinked executables.

All executables pass six full-output/accounting cases. Timing fixtures are
59 ASCII bytes + LF repeated 69 times; `é界🙂` repeated 456 times; and
`ab` + LF repeated 1,366 times, as `JSON.stringify({id: 7, payload: ...})`.
Every sweep runs a full warmup and 15 measured rounds with 6,000 calls per
payload, randomized base/head/control order and a repeated baseline binary
in each group. All eight sweeps complete: 4,096 accepted groups including
256 warmup groups, zero rejected groups, peak accepted load 3.355 under the
predeclared cutoff of 6. Builds, profiling and instruction capture finish
before timing starts.

Times below are microseconds per call: median of the 32 placement minima,
with their complete range in parentheses. Each minimum uses 15 measured
rounds. Paired percentages are **medians** of the 480 matched per-group
ratios minus one. The paired control medians range from -0.28% to +0.23%;
the untrimmed control means, stored separately in phase JSON, range from
-0.182% to +0.486%. No sample or outlier was removed.

| Mode | Moved function | Seed | Payload | Base median (range) | Head median (range) | Paired head | Paired control |
|---|---|---:|---|---:|---:|---:|---:|
| SIMD | string | 187 | lines | 2.096 (2.067–2.135) | 2.095 (2.076–2.148) | -0.11% | +0.12% |
| SIMD | string | 187 | unicode | 4.746 (4.703–4.790) | 4.741 (4.704–4.780) | -0.02% | +0.23% |
| SIMD | string | 187 | lone_escapes | 8.170 (7.997–8.395) | 5.154 (5.108–5.209) | -37.85% | +0.02% |
| SIMD | string | 188 | lines | 2.105 (2.075–2.135) | 2.096 (2.065–2.139) | -0.31% | -0.07% |
| SIMD | string | 188 | unicode | 4.762 (4.702–4.798) | 4.748 (4.702–4.781) | -0.24% | +0.09% |
| SIMD | string | 188 | lone_escapes | 8.186 (7.984–8.405) | 5.157 (5.130–5.208) | -37.99% | +0.16% |
| SIMD | run | 187 | lines | 2.096 (2.065–2.136) | 2.104 (2.066–2.115) | +0.33% | +0.15% |
| SIMD | run | 187 | unicode | 4.746 (4.712–4.805) | 4.752 (4.681–4.801) | -0.10% | +0.04% |
| SIMD | run | 187 | lone_escapes | 8.170 (8.093–8.269) | 5.152 (5.087–5.191) | -38.16% | +0.01% |
| SIMD | run | 188 | lines | 2.100 (2.083–2.140) | 2.107 (2.072–2.125) | +0.33% | +0.15% |
| SIMD | run | 188 | unicode | 4.763 (4.725–4.819) | 4.766 (4.704–4.803) | +0.02% | +0.18% |
| SIMD | run | 188 | lone_escapes | 8.186 (8.053–8.358) | 5.162 (5.104–5.190) | -38.22% | -0.06% |
| portable | string | 187 | lines | 2.702 (2.680–2.725) | 2.713 (2.674–2.741) | +0.35% | +0.01% |
| portable | string | 187 | unicode | 4.749 (4.686–4.799) | 4.748 (4.710–4.785) | +0.28% | -0.11% |
| portable | string | 187 | lone_escapes | 10.324 (9.935–10.819) | 5.177 (5.144–5.226) | -51.40% | +0.19% |
| portable | string | 188 | lines | 2.711 (2.684–2.742) | 2.714 (2.688–2.749) | +0.28% | +0.02% |
| portable | string | 188 | unicode | 4.760 (4.694–4.819) | 4.767 (4.728–4.812) | +0.20% | +0.09% |
| portable | string | 188 | lone_escapes | 10.357 (9.901–10.797) | 5.182 (5.134–5.218) | -51.43% | -0.28% |
| portable | run | 187 | lines | 2.712 (2.690–2.735) | 2.706 (2.679–2.738) | -0.28% | -0.08% |
| portable | run | 187 | unicode | 4.763 (4.718–4.804) | 4.754 (4.678–4.794) | -0.18% | -0.09% |
| portable | run | 187 | lone_escapes | 10.107 (9.967–10.242) | 5.178 (5.130–5.219) | -49.92% | -0.00% |
| portable | run | 188 | lines | 2.716 (2.692–2.746) | 2.715 (2.685–2.752) | -0.05% | +0.18% |
| portable | run | 188 | unicode | 4.778 (4.716–4.833) | 4.763 (4.681–4.800) | -0.37% | +0.01% |
| portable | run | 188 | lone_escapes | 10.079 (9.914–10.275) | 5.184 (5.148–5.215) | -49.93% | +0.09% |

## Lone-escape bands and untrimmed means

The band is `(maximum - minimum) / median` across the 32 placement minima.
Mean ranges use each placement's arithmetic mean over all 15 measured
rounds. These are descriptive observations, with every outlier retained;
minima and means answer different questions.

| Mode | Moved function | Seed | Base band | Head band | Base phase-mean range, µs | Head phase-mean range, µs | Control phase-mean range, µs |
|---|---|---:|---:|---:|---:|---:|---:|
| SIMD | string | 187 | 4.87% | 1.96% | 8.227–8.759 | 5.168–5.252 | 8.247–9.195 |
| SIMD | string | 188 | 5.15% | 1.51% | 8.226–8.678 | 5.171–5.265 | 8.202–8.773 |
| SIMD | run | 187 | 2.16% | 2.02% | 8.321–8.760 | 5.151–5.291 | 8.381–8.675 |
| SIMD | run | 188 | 3.72% | 1.68% | 8.341–8.613 | 5.157–5.594 | 8.351–8.599 |
| portable | string | 187 | 8.56% | 1.58% | 10.357–11.376 | 5.199–5.307 | 10.344–11.435 |
| portable | string | 188 | 8.65% | 1.63% | 10.337–11.297 | 5.203–5.351 | 10.279–11.445 |
| portable | run | 187 | 2.72% | 1.72% | 10.287–10.781 | 5.203–5.291 | 10.345–10.987 |
| portable | run | 188 | 3.58% | 1.29% | 10.362–10.888 | 5.212–5.294 | 10.403–10.923 |

SIMD helper seed 188 has a retained head outlier at phase `local/09`, round
11: 10.678 µs, versus base 8.305 µs and control 8.541 µs in that group, at
accepted load 3.118. That phase's untrimmed head mean is 5.594 µs. Seed 187's
largest head phase mean is 5.291 µs at `local/18`. The head helper's range
of untrimmed phase means is wider than baseline in seed 188; it is not a
flat curve under every statistic. The narrow minima bands and speedup
repeat across both seeds. This outlier alone does not establish a repeatable
helper-phase effect; its cause remains unassigned. A broader joint-layout
claim would need a predeclared crossed experiment.

## All retired-instruction captures

Each entry is the descriptive median of 96 differential estimates per side:
32 placements × three repeats, with N/3N at 3,000/9,000 calls. All process
totals, including outliers, are retained. Instruction medians are rounded to
one instruction per call and do not describe phase-specific effects.

| Mode | Moved function | Payload | Base | Head | Change |
|---|---|---|---:|---:|---:|
| SIMD | string | lines | 50,095 | 50,178 | +0.17% |
| SIMD | string | unicode | 112,310 | 112,296 | -0.01% |
| SIMD | string | lone_escapes | 254,102 | 135,305 | -46.75% |
| SIMD | run | lines | 50,008 | 50,243 | +0.47% |
| SIMD | run | unicode | 112,248 | 112,275 | +0.02% |
| SIMD | run | lone_escapes | 254,055 | 135,340 | -46.73% |
| portable | string | lines | 65,385 | 65,572 | +0.29% |
| portable | string | unicode | 112,285 | 112,334 | +0.04% |
| portable | string | lone_escapes | 282,599 | 136,335 | -51.76% |
| portable | run | lines | 65,447 | 65,476 | +0.04% |
| portable | run | unicode | 112,321 | 112,233 | -0.08% |
| portable | run | lone_escapes | 282,641 | 136,272 | -51.79% |

## Correctness and preserved evidence

The complete local Rust 1.98.1 gate passes: formatting, Clippy and Adamantium
in both feature modes (zero findings, no new exemptions), debug/all-feature
workspace tests, optimized SIMD/portable workspace tests, documentation tests
and every golden corpus. Forty-four public-API release fixtures match full
output/accounting in both modes, including short spans of lengths 1–9 and
2/3/8-escape bursts followed by up to 8 KiB of ordinary text. The unit
oracle checks complete-span boundaries, Unicode and invalid UTF-8 tails,
step quotas, output limits and tracked memory.

Fresh golden observations match the pinned baseline for all 130,697 stable
successful step/peak/retained-memory records and all 103,201 error-phase
records. Stored-ledger drift predates this change; the ledger is unchanged.
Hosted ARM64 correctness validation also passes all 256 isolated placements
and six cases per executable. It supplies no physical-host timing evidence.

[The committed raw evidence](../results/json-lone-escapes/otis-a7daf27b.tgz)
contains all original JSON files verbatim, including raw rounds, warmups,
phase curves, instruction process totals, fixtures, validation, placement
and symbol manifests, environment, native sample and checksum, and provided
function assembly extracts. Four `write_text` extracts are derived from the
original full dumps. All 84 archived files were checked against their source;
the full-binary dumps and executables are omitted. SHA-256:
`2f565404af0413142f59eb1d10256338199aec6c4ad57350bda5dc68d78dd948`.
The committed shared summarizers reproduce the phase summaries to floating
point rounding tolerance (1e-12) across Python versions.

Dumont's full archive, with four complete disassemblies and logs, remains at
`/home/mgomes/Fleet/Build/dumont-vibe-187/otis-a7daf27b-evidence.tar.gz`
on Shannon and
`/Volumes/AI/Build/dumont-vibe-187/share/otis-a7daf27b-evidence.tar.gz`
on OTIS. SHA-256:
`a5f4826d07ea20f8563315164e7e4c37eb70406e18b9c6a067c0e0593090c066`.
Build artifacts and the 256 binaries remain at
`/Volumes/AI/Build/dumont-vibe-187/measurement-a7daf27b/`.
The rejected b9380440 and interrupted earlier runs remain separate evidence.
