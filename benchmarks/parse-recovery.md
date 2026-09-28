# Parser recovery measurements

Recovery reports independent syntax errors, preserving the original first error.
It is bounded by 100 diagnostics, syntax depth, and source-proportional work.
Successful parsing keeps the fail-fast parser specialization. Failed host parses
reuse untouched canonical tokens; lexical failures and rewritten tokens re-lex.

**Performance acceptance remains incomplete.** The final x86 measurements below
are contended by another agent's 200-million-case checker sweep. The valid arm64
run meets the 3% clean-input timing limit in both builds. Producing 100 diagnostics
also adds allocations and increases malformed-input arm64 RSS by 3.07–3.60%; this
is disclosed below, not treated as an approved exception to the requested limit.

## Method

- Baseline: `58c1c207d3fa0dcdfde6e60f4e1b939679fa5b1a`, which adds only the benchmark
  harness to `f515bd90`.
- Measured candidate: `502dd41a5cf1b98afd4fb5811e03070094191ffb`, whose runtime code
  is identical to delivery commit `03c7ef10` (only the commit message changed).
  It includes the integration rebase onto `de1b6c9e`. That integration does not
  change `src/syntax*` or `src/tooling.rs`.
- Eight rotating paired rounds of `python3 scripts/compare.py --rounds 8
  --suite parse --baseline <preserved binaries> --out <directory>`. Timing and
  allocation instrumentation use separate offline release builds. RSS uses
  fresh processes, followed by eight rotating repetitions per variant and case.
- arm64: `darwin`, Apple M4, macOS 26.6.2, Rust 1.98.1. No gate or other benchmark
  ran during measurement; the shared gate lock reserved the hosts.
- x86_64: `shannon`, Intel Core Ultra 9 285H, Linux, Rust 1.98.1. The final paired
  run pins core 2 and disables ASLR. A separate checker driver ignores the gate
  lock and starts successive batches with 12 workers; pinning does not make the
  host quiet. The directory named `after-quiet` is therefore **not a quiet run**.
- Clean cases measure lexing, parsing and tooling token extraction, without type
  checking. The malformed case invokes host checking but fails in syntax before
  the type checker can run. Host compilation has no invocation accounting:
  reported steps and tracked peak/retained bytes are all zero. Requested bytes
  and allocation counts are actual allocator measurements; RSS includes the
  process and its code pages.

| Case | Source bytes | Selection |
| --- | ---: | --- |
| massive | 9,231 | Largest upstream program |
| bitwise_operations | 5,042 | Largest site program |
| wide_call | 120,105 | Large replay call, `01ca33781c4690e5` |
| wide_shape | 79,723 | Large replay shape, `d93a23c7b52df4df` |
| large_enum | 131,138 | Large replay enum, `e018901b17434038` |
| large_literal | 98,428 | Largest language fixture |
| pathological | 100,000 | 10,000 independent malformed assignments |

## Before and after

Times are medians in microseconds. Each paired cell is baseline → candidate;
a single value is unchanged. Allocation counts and requested bytes agree between
portable and SIMD builds. Tracked peak/retained bytes are `0 / 0` for every row,
before and after. RSS is the median of eight fresh-process samples for SIMD.


### arm64, quiet

| Case | SIMD µs | Change | Portable µs | Change | Allocations | Requested bytes | RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| massive | 537.87 → 540.22 | +0.44% | 539.98 → 536.52 | -0.64% | 3,872 | 2,333,010 → 2,326,986 | 4.695 → 4.672 |
| bitwise_operations | 322.51 → 323.29 | +0.24% | 324.32 → 321.89 | -0.75% | 2,535 | 1,098,595 → 1,097,539 | 5.195 → 5.234 |
| wide_call | 23,944.64 → 23,340.52 | -2.52% | 24,060.70 → 23,423.95 | -2.65% | 200,106 | 79,781,756 → 79,781,772 | 437.195 → 437.266 |
| wide_shape | 3,184.07 → 3,195.76 | +0.37% | 3,211.57 → 3,177.46 | -1.06% | 28,241 | 14,170,177 → 14,170,081 | 9.906 → 9.898 |
| large_enum | 618.48 → 603.52 | -2.42% | 618.76 → 603.90 | -2.40% | 75 | 349,872 → 349,848 | 3.828 → 3.812 |
| large_literal | 242.03 → 236.46 | -2.30% | 248.06 → 236.05 | -4.84% | 157 | 124,657 → 124,585 | 4.852 → 4.883 |
| pathological | 5,450.04 → 5,528.44 | +1.44% | 5,468.49 → 5,571.03 | +1.88% | 55 → 777 | 18,880,778 → 19,168,951 | 10.688 → 11.016 |

### x86_64, contended; informational only

| Case | SIMD µs | Change | Portable µs | Change | Allocations | Requested bytes | RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| massive | 791.24 → 790.86 | -0.05% | 789.39 → 795.03 | +0.71% | 3,872 | 2,333,010 → 2,326,986 | 13.344 |
| bitwise_operations | 489.82 → 483.05 | -1.38% | 490.07 → 485.50 | -0.93% | 2,535 | 1,098,595 → 1,097,539 | 13.344 |
| wide_call | 33,559.07 → 33,447.55 | -0.33% | 34,038.22 → 33,501.18 | -1.58% | 200,106 | 79,781,756 → 79,781,772 | 45.781 → 42.113 |
| wide_shape | 4,680.86 → 4,578.92 | -2.18% | 4,626.20 → 4,558.68 | -1.46% | 28,241 | 14,170,177 → 14,170,081 | 14.145 → 13.492 |
| large_enum | 1,023.64 → 1,011.97 | -1.14% | 1,029.49 → 1,030.61 | +0.11% | 75 | 349,872 → 349,848 | 13.344 |
| large_literal | 254.32 → 252.85 | -0.58% | 254.10 → 254.52 | +0.17% | 157 | 124,657 → 124,585 | 13.344 |
| pathological | 6,732.15 → 6,681.23 | -0.76% | 6,613.97 → 6,870.25 | +3.87% | 55 → 777 | 18,880,770 → 19,168,943 | 14.250 → 14.254 |

## Limits and follow-up

The first x86 run, before the checker sweep started, measured clean cases within
−1.68% to +1.28% across both builds. Its candidate was `d7398f09`, before token
reuse; it is useful evidence but does not certify final-head x86 performance.
The unpinned final run showed portable wide-shape +4.12% and pathological +5.55%
while the other process used roughly nine cores. The pinned repeat cleared the
clean timing outliers, but pathological portable remained +3.87%. These runs
remain contended and are not accepted as regression controls. A quiet x86 rerun
is still required, including wide-call RSS (portable +3.84% in the contended
confirmation, SIMD −8.01%). No other agent's process was stopped or changed.

The arm64 clean timing maximum is +0.44% SIMD; all portable clean cases improve.
Its initial portable large-literal RSS outlier, +4.49%, clears to +1.31% across
eight fresh-process repetitions. Every clean case in that repeated RSS check
stays within 3% in both builds.

Malformed output changes from one diagnostic to 100. Token reuse reduces the
initial recovery implementation from 8.340 ms to 5.528 ms on arm64 SIMD, and
requested bytes from 28,605,559 to 19,168,951. Against the original fail-fast
baseline, final timing rises 1.44% SIMD and 1.88% portable; requested bytes rise
1.53%. Allocation count rises from 55 to 777 to construct the additional errors.
Repeated RSS rises 10.688 → 11.016 MiB SIMD (+3.07%) and 10.625 → 11.008 MiB
portable (+3.60%). These costs accompany the expanded diagnostic output; the
requested performance limit has not been fully met or waived.

## Correctness and accounting

- The mutation sweep passes 37,227 cases, including combined edits, deep nesting,
  long expressions and 10,000-error inputs. It finds 2,940 multi-error cases;
  maximum output is 100. No panic, timeout, acceptance change, duplicate
  diagnostic or first-error change occurs.
- A paired audit of 233,865 engine/parser cases preserves the first outcome and
  every step, tracked peak and retained-byte count. Existing baseline quota
  drift is preserved. No counter golden is re-recorded.
- Intentional additions: 1,774 parse mutations, 33 rejections, two replay compiles,
  and appended syntax diagnostics in the broken-document LSP session. The first
  diagnostic stays identical. Reasons and the Counter log addendum are in
  [the golden README](../tests/golden/README.md#parser-recovery-2026-09-28).
- Formatting, both native Clippy configurations, 2,096 workspace tests, golden
  validation, portable/SIMD validation and WASI all pass. WASI's standalone
  lockfile required only its existing package version to move from 0.1.0 to
  0.80.0 after integration; dependencies are unchanged. The final distributed
  gate log is retained with the artifacts below.

## Evidence

All raw artifacts live under [`.cache/parse-recovery/`](../.cache/parse-recovery/)
on the external volume. Nothing beyond this summary is committed.

- `remote/darwin/after-reuse/`: accepted timing, allocator and environment data;
  `remote/darwin/rss-confirm/`: repeated RSS samples.
- `remote/shannon/after-initial/`: initial quiet run before token reuse;
  `remote/shannon/after-reuse/` and `remote/shannon/after-quiet/`: contended final
  runs; `remote/shannon/rss-confirm/`: contended pinned RSS repetition.
- `remote/{darwin,shannon}/before/`: initial baseline runs. The preserved baseline
  binaries and immutable revision file remain on each host under the same task
  cache, with the candidate bundle in the local cache.
- `sweep-reuse/summary.json`, `paired-audit.json`, and
  `golden-additions-audit.json`: mutation, observation and first-error evidence.
- `verification-final.json`, `*-reuse.log`, `wasi-final.log`, and `gate-all.log`:
  verification records. The older `verification.json` retains the initial
  stale-lockfile failure rather than erasing it.
