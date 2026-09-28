# Language decisions: validation and performance

The four ADR-008 addenda reject negative integer powers, give floats a NaN-first
total order, diagnose adjacent expressions, and keep required-file function
calls in their lexical scope. Tests cover checker/runtime agreement, both power
assignment forms, stable NaN/signed-zero ordering, parser gaps and parenless
calls, and module calls with conflicting caller bindings.

## Measurement

The task specifically requested the core suite on `darwin`, rather than a new
two-architecture benchmark campaign. These are Apple M4 arm64 measurements on
macOS 26.6.2 with Rust 1.98.1. x86_64 correctness is covered by the required
`shannon` gate; no x86_64 timing result is claimed.

The baseline runtime is `feeef64c`, built at fixture-only commit `42ba69ef` with
the final independently checked power/sort fixtures. The candidate is
`542853f3`, rebased onto checker integration `f1c777b8`. Each passed its full
validation before measurement. Offline release builds used four build jobs.
The shared gate lock reserved the host and no remote gate was running.

Separate eight-round `scripts/compare.py` runs were followed by eight paired
rounds rotating the preserved baseline/candidate portable and SIMD binaries
through the harness's fixed execution path, targeting 75 ms per case. Paired
measurements check every core result against the validated expectations;
intentional non-core language disagreements are not used as a timing oracle.
Allocation instrumentation runs separately; RSS includes process startup.

The following paired times are microseconds per metered call; RSS is MiB.
Single allocation/memory values are unchanged before and after.

| Case | Build | Time, before → after | Change | Allocations | Allocated B | Peak tracked B | Retained B | RSS, before → after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| integer_power | portable | 4.588 → 4.604 | +0.35% | 14 | 3224 | 2664 | 0 | 5.859 → 5.859 |
| integer_power | SIMD | 4.597 → 4.570 | -0.59% | 14 | 3224 | 2664 | 0 | 5.859 → 5.844 |
| big_integer_power | portable | 3.852 → 3.910 | +1.51% | 75 | 4966 | 3022 | 822 | 6.156 → 6.094 |
| big_integer_power | SIMD | 3.832 → 3.889 | +1.49% | 75 | 4966 | 3022 | 822 | 6.141 → 6.125 |
| float_power | portable | 4.436 → 4.485 | +1.08% | 18 | 4168 | 3368 | 608 | 5.906 → 5.922 |
| float_power | SIMD | 4.422 → 4.406 | -0.36% | 18 | 4168 | 3368 | 608 | 5.859 → 5.906 |
| float_sort | portable | 81.056 → 75.911 | -6.35% | 18 | 12352 | 11696 | 4192 | 5.844 → 5.797 |
| float_sort | SIMD | 81.039 → 75.270 | -7.12% | 18 | 12352 | 11696 | 4192 | 5.828 → 5.781 |

Unlimited powers range from -0.57% to +0.69%; unlimited sorting improves
6.39% portable and 6.59% SIMD. Steps remain 793, 330, 625 and 13313 respectively,
in both modes. Across all 332 paired core comparisons, allocations, allocated
bytes, steps and tracked peak/retained bytes are unchanged. Every paired RSS
increase is below 3%.

## Regression audit

The separate runs found portable `hash_lookup/unlimited` +3.27% and SIMD
`loop_branches/metered` +5.06%. Pairing reduces the former to +2.81%; the latter
remains +5.32% (74.237 → 78.189 µs). Two initial portable RSS outliers,
`upstream_fibonacci/unlimited` +3.06% and `upstream_countdown/unlimited` +3.27%,
also clear in the paired run.

The branch loop's integer arithmetic, loads/stores and conditional jumps run
in `vm::simple::run`. Its source and budget source are unchanged from the
baseline. Disassembly of the exact timed SIMD binaries finds all 3,962
instructions identical after resolving call/branch relocations and ARM
page-relative addresses; the function moved from `0x100303fc0` to
`0x10030e194`. Fifty-one other selected helpers, including accounting, also
have identical instruction sequences. General `Run::advance` changes only
for the required-file resolution fix, which this loop does not exercise.
This supplies the unchanged-code evidence required to treat the branch-loop
timing as a nonblocking placement effect. The raw +5.32% remains disclosed;
it is not included in the claim that the requested power/sort cases meet 3%.

## Golden observations and verification

The [golden README](../tests/golden/README.md#language-decisions-2026-09-27)
lists every re-recorded file and corpus repair. Upstream checker observations
and counters were retained before selectively recording these decisions.
The only changed existing counter record is the rewritten NaN showcase:
378 → 377 steps, 3883 → 3879 peak bytes, 1329 → 1325 retained bytes. Eight new
core modes add counters; no other existing counter record changes.

Local formatting, both all-target Clippy configurations with warnings denied,
workspace tests, all eight golden corpora, portable/SIMD validation and WASI
pass. Portable and SIMD counters agree on all 107,595 validation cases.
The final distributed gate is `gate-all.sh mgomes/language-2`, run from the
main repository with its shared lock after measurement. Its exact head and
results are archived separately so that recording the outcome does not move
the tested commit.

All raw evidence is outside Git under
`.cache/language-2/`: `darwin/before-measure/`,
`darwin/after/`, `darwin/paired/`, `darwin/assembly/`, `compare-assembly.py`,
`golden-audit.json`, `final-*.log`, `validate.log`, `wasi.log`, `gate-head.txt`
and `gate-all.log`. Assembly evidence includes binary copies, symbol/import
tables, the comparison and source hashes. No raw benchmark output is committed.

## Language-1 integration

Rebased onto `187e0455`, preserving the money/formatting documentation, required
match captures, V0201 hints and typed JSON failures alongside these four
decisions. Conflicting golden files started from upstream and only the same
language-2 cases were re-recorded, preserving upstream counter reductions.
The timing tables above describe the earlier measured revisions; they are not
new measurements of this integration. Fresh verification and the exact-head
distributed gate are archived under `rebase-language-1/` in the same cache,
including `verification.json`, `golden-audit.json`, `gate-head.txt` and
`gate-all.log`.
