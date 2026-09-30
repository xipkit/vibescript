# Method name suffix measurements

`?` and `!` are reserved for method names, calls and symbols. Adjacent `!=`
always compares; invalid bindings receive V0003 and a machine-applicable fix.
Hash and keyword labels retain their string keys. Optional type and block
markers retain their meaning. See [ADR-008](../docs/adr/008-canonical-surface-for-ai-authors.md#addendum-method-name-suffixes-2026-09-28).

## Method

- Baseline: `1f4968875fe17e3fd8f3260951d62d8ed8820248`.
- Measured code: `e06808245e24e8058b4d6d06e2356c0db3acb3d4`. Later changes contain only documentation and test cases.
- Eight rounds of `python3 scripts/compare.py --rounds 8 --suite parse --out <dir>`
  on each revision, then eight rotating paired rounds using the preserved baseline
  binaries and `compare.measure` with a 200 ms target. Both revisions pass their
  own golden observations; all paired parser digests agree. The same executable
  entry path is used for every variant. Allocation instrumentation is separate.
- RSS is the median of eight rotating fresh-process measurements per variant and case.
- arm64: `darwin`, Apple M4, macOS 26.6.2, Rust 1.98.1.
- x86_64: `shannon`, Intel Core Ultra 9 285H, Linux, Rust 1.98.1. Paired timing
  and RSS pin core 2 and disable ASLR. The shared gate lock reserves both hosts;
  process snapshots show no competing gate, compiler or checker benchmark.
- Cases cover large real programs, wide declarations, two 65 KiB enum names,
  an escaped string, bounded recovery, and 1,000 predicate/bang method definitions
  with calls and labels. Parsing/tooling has no invocation accounting: steps,
  tracked peak bytes and tracked retained bytes are `0 -> 0` for every case.

## Results

Times are medians in microseconds. Pairs are baseline → candidate; a single
allocation or byte count is unchanged in both builds. RSS includes code pages.

Every timing stays within the 3% limit: the maximum increase is 2.59% on
arm64 and 2.46% on x86_64. Allocations and tracked counters are unchanged.
Two arm64 portable RSS outliers are retained below and tested with a code
placement control; all other repeated RSS comparisons stay within 3%.

### arm64

| Case | SIMD µs | Change | Portable µs | Change | Allocations | Requested bytes | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| massive | 538.71 → 540.08 | +0.25% | 536.78 → 542.33 | +1.03% | 3,872 | 2,326,986 | 4.641 → 4.734 | 4.586 → 4.719 |
| bitwise_operations | 321.69 → 323.02 | +0.41% | 321.10 → 325.32 | +1.32% | 2,535 | 1,097,539 | 5.266 → 5.320 | 5.133 → 5.297 |
| wide_call | 23604.37 → 23556.13 | -0.20% | 23567.58 → 23452.81 | -0.49% | 200,106 | 79,781,772 | 437.055 → 437.328 | 437.133 → 437.141 |
| wide_shape | 3110.04 → 3127.84 | +0.57% | 3110.33 → 3132.49 | +0.71% | 28,241 | 14,170,081 | 9.836 → 10.016 | 9.797 → 9.984 |
| large_enum | 604.21 → 619.71 | +2.57% | 604.54 → 620.18 | +2.59% | 75 | 349,848 | 3.812 → 3.922 | 3.727 → 3.883 |
| large_literal | 241.85 → 241.54 | -0.13% | 241.84 → 247.94 | +2.53% | 157 | 124,585 | 4.922 → 4.945 | 4.828 → 4.906 |
| pathological | 5426.48 → 5468.78 | +0.78% | 5444.39 → 5424.93 | -0.36% | 777 | 19,168,951 | 11.008 → 11.078 | 10.953 → 11.062 |
| method_names | 3285.21 → 3329.50 | +1.35% | 3281.91 → 3339.02 | +1.74% | 25,570 | 12,748,240 | 9.992 → 10.062 | 9.844 → 10.094 |

### x86_64

| Case | SIMD µs | Change | Portable µs | Change | Allocations | Requested bytes | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| massive | 614.77 → 615.67 | +0.15% | 614.26 → 620.10 | +0.95% | 3,872 | 2,326,986 | 13.465 → 13.465 | 13.465 → 13.465 |
| bitwise_operations | 377.27 → 386.57 | +2.46% | 376.72 → 385.02 | +2.20% | 2,535 | 1,097,539 | 13.465 → 13.465 | 13.465 → 13.465 |
| wide_call | 25406.51 → 25413.53 | +0.03% | 25394.83 → 25493.00 | +0.39% | 200,106 | 79,781,772 | 43.016 → 43.000 | 43.211 → 43.008 |
| wide_shape | 3475.37 → 3465.99 | -0.27% | 3441.53 → 3467.49 | +0.75% | 28,241 | 14,170,081 | 13.492 → 13.492 | 13.555 → 13.492 |
| large_enum | 767.58 → 775.05 | +0.97% | 774.70 → 775.03 | +0.04% | 75 | 349,848 | 13.465 → 13.465 | 13.465 → 13.465 |
| large_literal | 203.80 → 203.79 | -0.01% | 204.19 → 205.89 | +0.83% | 157 | 124,585 | 13.465 → 13.465 | 13.465 → 13.465 |
| pathological | 5226.61 → 5090.59 | -2.60% | 5184.30 → 5155.49 | -0.56% | 777 | 19,168,943 | 14.254 → 14.090 | 14.152 → 14.090 |
| method_names | 3930.39 → 3941.10 | +0.27% | 3930.56 → 3928.88 | -0.04% | 25,570 | 12,748,240 | 13.465 → 13.465 | 13.465 → 13.465 |

### RSS placement control

The arm64 portable eight-sample RSS medians rise 3.20% for bitwise operations
and 4.19% for the large enum. A separate baseline build changes only the
private lexer helper name from `escape` to `decode_escape` and its two call
sites. Its body is unchanged, and neither measured input contains a backslash,
so neither executes that helper. This is a no-op code placement control,
not a suffix-rule implementation.

Eight rotating rounds compare all three binaries through the same entry path.
Every output digest agrees. The control itself moves RSS by 3.05% and 2.30%,
while its timing changes only 0.38% and 0.04%. The candidate remains less than
1.1% above the control's RSS. These outliers meet the specified code-placement
exception; no allocator growth or attributable regression above 3% is accepted.

| Case | Original baseline RSS MiB | Renamed-helper baseline RSS MiB | Candidate RSS MiB | Candidate vs control |
| --- | ---: | ---: | ---: | ---: |
| bitwise_operations | 5.125 | 5.281 | 5.320 | +0.74% |
| large_enum | 3.734 | 3.820 | 3.859 | +1.02% |

## Implementation and accounting

The initial per-character lookahead and Unicode suffix searches made arm64
large-enum parsing 35.5% slower (603 → 817 µs). Checking the suffix boundary
once per token, validating variables at their parse sites, and using one
`memchr2` search remove that repeated work. `memchr` 2.8.3 was already locked
as a transitive dependency; it is now a direct dependency. Binding names are
checked once. No custom SIMD code or allocation was added.

The [golden README](../tests/golden/README.md#method-name-suffixes-2026-09-28)
records all intentional observations: 140 successful sources and six rejection
sources are renamed without changing their results; 15 forbidden-name tests
become rejections; 88 adjacent comparisons now succeed and 37 reach type errors.
Only the affected rejection and parse observations are re-recorded.

Five intentional source-rewrite counter reductions are recorded: steps
42 → 35, 57 → 52 and 57 → 50 in the three shape fixtures, and peak bytes
5,203 → 5,200 and 5,688 → 5,686 in the two variable-name fixtures. Their other
counters do not change. The paired audit of 127,420 successful cases on the
rewritten sources finds no counter differences. Existing path/quota drift
also occurs in the baseline; no increase is accepted.

## Verification

- Formatting, both Clippy configurations and all 2,102 workspace tests pass.
- All eight golden corpora pass, including CLI and LSP observations.
- Portable and SIMD validation passes 107,682 shared cases with identical counters.
- WASI tests and filesystem witnesses pass.
- The 37,227-case parse sweep has 44 expected first-error changes: 43 V0003
  diagnostics and one mutation that now parses `!=`. No other first error,
  acceptance, panic, hang, duplicate diagnostic or invalid span changes. Its
  unfiltered exit is 1 because expected changes are reported as differences.
- The final distributed gate log is retained as `gate-all.log` below.

## Evidence

Raw artifacts are outside Git under `.cache/name-suffix/` on the external volume.

- `remote/{darwin,shannon}/search-paired/`: timing, allocations, digests, inputs and immutable binary hashes.
- `remote/{darwin,shannon}/search-rss/`: all repeated RSS samples.
- `remote/darwin/layout-control/` and `layout-control.patch`: the no-op
  baseline control, its binary hashes, all timing/RSS samples and output digests.
- Earlier `paired`, `optimized-paired` and `boundary-paired` runs retain the
  rejected implementations and measurements; they are not acceptance results.
- `counter-rewrite-audit.json`, `counter-search-audit.json`,
  `golden-boundary-audit.json`, and `sweep-search/summary.json`: accounting
  and intentional observation audits.
- `verification-search.json`, `*-search.log` and `gate-all.log`: local and
  distributed verification. Preserved baseline binaries remain in each remote
  clone under `.cache/name-suffix/before/`; shipped bundles remain in the local cache.
