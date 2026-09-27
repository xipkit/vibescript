# ASCII substring search

The `mgomes/text-index` change accelerates long ASCII `index` and `rindex`
searches with an out-of-line mismatch scanner: SSE2 on x86_64, NEON on
aarch64, and eight-byte SWAR in portable and WASI builds. The KMP table,
scratch capacity, byte-work checkpoints, and rune fallback are retained.
Existing functions in the shared `src/scan.rs` are unchanged.

This resumes the [profiled text investigation](../text-regex/README.md) under
the coordinator's revised regression rule. A result beyond 3% blocks when
changed code or shared hot code can cause it. An unrelated workload with
unchanged generated instructions is assessed with a control and recorded,
including differences caused by placement or measurement noise.

The comparison baseline is `7d309479453319dbf8cd6806e174435c5bcfdc1a`.
The measured candidate is `6f1149aae` (the performance commit was later
amended only to record measurements). Its runtime tree is
`7c2957326ffe72aee4354b6a0471c59bd0bb371b`, unchanged by the summary commits. Before delivery the branch was rebased
onto `4b8c9aef`, which adds footprint tooling and authoring diagnostics,
fixtures, and documentation. The text, scan, regex, and VM source and the
benchmark inputs are unchanged by these upstream additions. The timings
describe the measured revisions above; final verification covers the
rebased delivery head.
Measurements use `scripts/compare.py --rounds 8 --target-ms 150`, both
`--suite text` and `--suite core`, paired baseline/candidate portable and
SIMD builds, and quiet host reservations. Raw artifacts, binaries,
disassembly, and controls live under
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/mgomes/text-regex/index/`
locally, with the same branch cache hierarchy on each host. Only summary
Markdown is committed alongside the code.
All 752 comparisons have identical allocation counts/bytes and tracked
steps/peak/retained bytes. There are no RSS increases beyond 3%.
There are 752 comparisons: 66 text and 122 core cases, each in two builds
on two hosts. Linux runs on CPU 2 with per-process ASLR disabled. Each host
builds both revisions at the same checkout path and measures through a
fixed executable entry path.

Seventeen initial timing comparisons exceed +3%. Six clear in the controls;
eleven persist, from +3.65% to +12.59%. They are nonblocking under the new
rule because the workloads do not reach the changed code and the shared
hot instructions are unchanged apart from relocations. They remain in the
report and raw matrix; persistent differences are not assumed to be noise.
All substring-search and fallback controls stay within the regression limit.

## Vinci arm64 results

Eight-round medians, metered calls. Allocation and tracked-memory figures
are identical before/after; RSS is the per-case process peak.

| Case / build | Time, µs before → after | Allocations / bytes | Tracked peak / retained bytes | RSS, KiB before → after |
| --- | ---: | ---: | ---: | ---: |
| index / simd | 19.580 → 0.977 | 15 / 2020 | 10265 / 0 | 6080 → 6016 |
| index / portable | 19.740 → 1.469 | 15 / 2020 | 10265 / 0 | 6032 → 6000 |
| rindex / simd | 38.403 → 1.272 | 15 / 2020 | 18462 / 0 | 6080 → 6048 |
| rindex / portable | 39.112 → 2.207 | 15 / 2020 | 18462 / 0 | 6112 → 6080 |
| index_short_hit / simd | 0.699 → 0.694 | 15 / 1972 | 2022 / 0 | 6032 → 6000 |
| index_short_hit / portable | 0.694 → 0.703 | 15 / 1972 | 2022 / 0 | 6016 → 6032 |
| index_short_miss / simd | 0.737 → 0.740 | 15 / 2044 | 2094 / 0 | 5968 → 5952 |
| index_short_miss / portable | 0.728 → 0.742 | 15 / 2044 | 2094 / 0 | 5968 → 5984 |
| index_unicode / simd | 36.909 → 30.721 | 15 / 2020 | 20497 / 0 | 6096 → 6032 |
| index_unicode / portable | 47.376 → 40.900 | 15 / 2020 | 20497 / 0 | 6096 → 6080 |
| index_overlap / simd | 19.803 → 14.209 | 15 / 2032 | 10265 / 0 | 6032 → 6016 |
| index_overlap / portable | 20.103 → 14.303 | 15 / 2032 | 10265 / 0 | 6016 → 6000 |

## Shannon x86_64 results

Eight-round medians, metered calls. Allocation and tracked-memory figures
are identical before/after.

| Case / build | Time, µs before → after | Allocations / bytes | Tracked peak / retained bytes | RSS, KiB before → after |
| --- | ---: | ---: | ---: | ---: |
| index / simd | 15.765 → 1.223 | 15 / 2020 | 10265 / 0 | 13792 → 13792 |
| index / portable | 15.939 → 1.564 | 15 / 2020 | 10265 / 0 | 13792 → 13792 |
| rindex / simd | 30.501 → 1.454 | 15 / 2020 | 18462 / 0 | 13792 → 13792 |
| rindex / portable | 30.834 → 2.153 | 15 / 2020 | 18462 / 0 | 13792 → 13792 |
| index_short_hit / simd | 0.985 → 0.977 | 15 / 1972 | 2022 / 0 | 13792 → 13792 |
| index_short_hit / portable | 0.970 → 0.973 | 15 / 1972 | 2022 / 0 | 13792 → 13792 |
| index_short_miss / simd | 1.010 → 0.998 | 15 / 2044 | 2094 / 0 | 13792 → 13792 |
| index_short_miss / portable | 0.988 → 0.995 | 15 / 2044 | 2094 / 0 | 13792 → 13792 |
| index_unicode / simd | 26.918 → 26.761 | 15 / 2020 | 20497 / 0 | 13792 → 13792 |
| index_unicode / portable | 38.434 → 38.281 | 15 / 2020 | 20497 / 0 | 13792 → 13792 |
| index_overlap / simd | 22.233 → 20.082 | 15 / 2032 | 10265 / 0 | 13792 → 13792 |
| index_overlap / portable | 21.539 → 20.138 | 15 / 2032 | 10265 / 0 | 13792 → 13792 |

## ARM controls and attribution

Nine initial timing comparisons exceeded +3%. Each was repeated in 24
interleaved rounds with baseline, an identical-baseline control, and the
candidate. No process interference was detected. Allocation counts/bytes
and every accounting metric are identical throughout. No RSS comparison
exceeds +3%.

| Case | Build | Initial time change | Repeated change | Baseline self change | Attributable? |
| --- | --- | ---: | ---: | ---: | --- |
| text/regex_unanchored_miss/metered | simd | +10.40% | -0.91% | -0.31% | No; unchanged executed code |
| text/regex_unanchored_miss/unlimited | portable | +3.19% | -0.02% | -0.01% | No; unchanged executed code |
| text/regex_unanchored_miss/unlimited | simd | +19.76% | -1.31% | +2.60% | No; unchanged executed code |
| text/regex_literal_miss/metered | portable | +11.38% | -15.10% | -18.26% | No; unchanged executed code |
| numeric_loop/metered | portable | +3.56% | +2.97% | -0.14% | No; unchanged executed code |
| numeric_loop/unlimited | portable | +3.73% | +3.66% | -0.01% | No; unchanged executed code |
| function_calls/metered | portable | +3.58% | +2.60% | -0.56% | No; unchanged executed code |
| loop_float/metered | portable | +3.57% | +3.65% | -0.04% | No; unchanged executed code |
| loop_float/unlimited | portable | +4.02% | +4.08% | +0.01% | No; unchanged executed code |

The three persistent differences are `numeric_loop/unlimited`,
`loop_float/metered`, and `loop_float/unlimited` in the portable build.
They are recorded as nonblocking under the revised rule, not dismissed as
random noise. No core fixture calls `index` or `rindex`. The compiler and
the VM, operators, allocation paths, records, and dispatch source are
unchanged. Disassembly of the VM loop (23,112 instructions), binary
operators (1,679), member dispatch (1,143), and string dispatch (705) is
identical after normalizing address relocations in both ARM builds.
The regex literal scanner and search new/find/add functions also have
identical instructions. Thus neither the regex nor numeric outliers run
the changed search code or acquire different shared hot instructions.
The baseline-self control exposes the regex literal scan's large timing
variation even with identical binary bytes. Raw disassembly and the
relocation checks are preserved with the controls.

## x86 controls and attribution

Eight initial timing comparisons exceeded +3%; none is a substring-search
case. Each was repeated in 24 interleaved rounds with an identical-baseline
control. No process interference was detected.

| Case | Build | Initial time change | Repeated change | Baseline self change | Attributable? |
| --- | --- | ---: | ---: | ---: | --- |
| text/regex_unanchored_miss/metered | simd | +11.47% | +12.59% | +0.12% | No; unchanged executed code |
| text/regex_unanchored_miss/unlimited | simd | +9.68% | +10.91% | +0.23% | No; unchanged executed code |
| text/regex_literal_miss/metered | portable | +5.25% | +4.81% | -0.27% | No; unchanged executed code |
| text/regex_literal_miss/metered | simd | +6.26% | +10.90% | -0.58% | No; unchanged executed code |
| text/regex_literal_miss/unlimited | portable | +3.65% | +7.92% | -1.79% | No; unchanged executed code |
| text/regex_literal_miss/unlimited | simd | +7.08% | +6.34% | -1.68% | No; unchanged executed code |
| json_parse_escaped_4k/metered | portable | +4.39% | +4.34% | -0.23% | No; unchanged executed code |
| json_parse_escaped_4k/unlimited | portable | +3.72% | +4.16% | -0.25% | No; unchanged executed code |

The regex literal scanner (390 instructions), search construction/find/add
(635/1,342/553), VM loop (21,632), binary operators (1,729), and member
dispatch (1,127) have identical instructions after address-relocation and
LLVM symbol-suffix normalization in both x86 builds. The portable JSON
string readers (1,267/1,627), value parser (3,489), and existing byte/rune
scanners are also identical. These workloads do not call the changed
substring search; no shared hot instructions, allocations, or value layouts
changed. Persistent differences are recorded as nonblocking under the
revised rule.

## Verification

Formatting, both workspace/all-target Clippy configurations,
workspace tests, goldens, portable/SIMD validation, and `scripts/check-wasi`
pass. Commands and exit codes are retained in `local-verification.json`
under the artifact root. The delivery-head three-host gate result and logs
are retained in `gate-status.json` and `gates/`.

## Accounting

A paired audit of 197,856 engine cases found zero changes in steps, peak
tracked bytes, or retained tracked bytes. Four replay observations contain
clocks or UUIDs; every deterministic observation is unchanged. No Counter
log entry or golden counter update is needed.

## Regex literal caching investigation

Regex literals are not compiled-regex constants today. The bytecode
compiler stores pattern bytes and emits `Op::Regex(index, flags)`
(`src/bytecode.rs`). Every execution of that instruction calls
`Regex::compile` (`src/vm.rs`), including repeated executions in a block
and successive calls of one compiled script. A string pattern passed to
`match?`, `Regex.match`, or the regex operations also compiles at runtime;
an existing regex value reuses its compiled code.

A cache owned by the compiled script is feasible. `Regex` already owns
immutable instructions, character-class parts, and capture-name metadata
through `Arc<Code>`, and `Regex::import` shares that code across execution
budgets. Cache entries should be keyed by literal slot and flags; dynamic
string patterns need a separate bounded policy. A successful cached value
must not retain a previous call's budget, and quota, cancellation, or
deadline failures must not become cached pattern errors. Runtime error
timing, error names,
compile size limits, and interruption/accounting behavior require explicit
VM design: a probe confirms an invalid literal in an unexecuted branch
compiles and runs successfully, while evaluating it raises the runtime
`regex literal invalid regex` error. Eager rejection would change that
behavior. Blindly importing a cached value also removes compilation
work and temporary-memory charges.

On the measured 64-bit layout, the compiled code costs
`104 + 40 * instruction_capacity + 24 * part_capacity + 16 * name_capacity`
bytes, including its `Arc` header, before the pattern and any cache table.
A complete `Regex` adds a 144-byte allocation and its source/flag-expanded
pattern ownership. An external probe returning a single literal measured
these complete retained tracked sizes; these are sizing examples, not an
implemented cache's incremental script footprint:

| Pattern | Retained bytes |
| --- | ---: |
| `request-id` | 962 |
| `\AID-[0-9]{8}\z` | 1,279 |
| Email validation pattern from the text suite | 1,426 |
| Named user, ID, and status captures from the text suite | 1,952 |
| `(a?){1000}` | 160,562 |

Capacity, rather than instruction count alone, matters. At the existing
100,000-instruction limit, the instruction vector alone can retain about
3.81 MiB per literal, plus class/name tables. Sharing existing pattern
constants can reduce the complete-value figures above. Search scratch and
the current per-search literal-prefix/KMP metadata would still be created
by `Search::new`; caching compiled code alone does not eliminate them.

No regex cache is implemented here. This is a handoff to `mgomes/vm-round-4`.
