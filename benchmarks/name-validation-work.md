# Callable-name validation work

Review comment 4129669562 identified unmetered scans of callable object keys.
Name checks now require compilation work, with an execution meter supplied
by runtime imports, capability binding/publication, and global validation.
Key length is charged before UTF-8 and spelling checks, and cancellation is
checked before scanning. Parser method and alias checks charge their compiler
work too. Exported functions use that parser validation and the same runtime
key check when imported. Existing global-name and registered-function charges
are reused. Standalone host declaration/prelude APIs retain their unmetered
setup behavior; ordinary object data fields skip method-name validation.

A 1 MiB valid callable key previously imported within a 1,024-step quota and
now fails with `ErrorKind::Steps`. Regression tests cover both host methods
and exported functions through globals, factories, and host returns;
publication fails before allocating the oversized key. Separate tests check
cancellation before invalid UTF-8 can be examined and metered compilation of
registered names. Previous tests for unconstrained ordinary data keys pass.

## Counter audit

`work_bytes` charges one step per started 64 bytes, including short names.
Paired runs of 197,906 runtime cases find no stable observation differences.
The existing four time/UUID-dependent cases are excluded as in the golden
checker. Exactly 540 conformance and 40 compatibility cases gain 1–17 steps;
all other stable counters, including peak and retained bytes, are unchanged.
The existing counter records receive only those positive step deltas, leaving
unrelated path/quota drift intact. The change is recorded in the
[Counter log](../tests/golden/README.md#counter-log).

For example, `glue_orders_cap/metered` changes from 31,272 to 31,279 steps,
`json_transform_cap/metered` from 6,257 to 6,264, and `hash_lookup_cap/metered`
from 24,673 to 24,680. This is intentional accounting for previously uncharged
validation work, not extra script execution.

## Paired measurements

Before: `166924b5` (preserved binaries from `2820b77c`, with identical `src/`).
After: `6190e184`, before this report was added. Eight rotating paired rounds
use `scripts/compare.py --rounds 8 --target-ms 250 --baseline <before>` for the
eight core workloads carrying `capability_probe` in both accounting modes,
and all eight parser cases. The wrapper selects the capability workloads;
measurement, validation, allocation counting, and RSS use the standard script.

Hosts were quiet and reserved with the shared gate lock: `darwin` (Apple M4,
arm64 macOS) and `shannon` (Intel Core Ultra 9 285H, x86_64 Linux). Linux runs
pin core 2 and disable ASLR. Allocations use separate instrumented binaries;
RSS samples use fresh processes. Both before/after variants pass fixture
validation and match observable results.

Maximum timing increases are 2.29% on arm64 and 1.95% on x86_64. All RSS
increases are below 3%, with maxima of 1.65% and 0.02%, respectively. No outlier
exclusion or repeat is needed. Allocation counts, requested bytes, tracked
peak and retained bytes are unchanged in every measured case.

Arrows mean before → after. Accounting without arrows is unchanged in both
build variants. Runtime rows show metered mode; raw data includes unlimited
mode too. Parser tracked counters are zero.

### arm64

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| parse/massive | 537.03 → 541.34 | 539.58 → 538.50 | 3,872 / 2,326,986 | 0 | 0 / 0 | 4.719 → 4.609 | 4.672 → 4.688 |
| parse/bitwise_operations | 321.66 → 323.04 | 322.63 → 323.63 | 2,535 / 1,097,539 | 0 | 0 / 0 | 5.297 → 5.094 | 5.281 → 5.266 |
| parse/wide_call | 23063.37 → 23303.25 | 23152.86 → 23209.14 | 200,106 / 79,781,772 | 0 | 0 / 0 | 437.281 → 437.094 | 437.344 → 437.078 |
| parse/wide_shape | 3118.35 → 3129.78 | 3138.43 → 3122.57 | 28,241 / 14,170,081 | 0 | 0 / 0 | 9.969 → 9.812 | 9.891 → 9.969 |
| parse/large_enum | 619.40 → 619.56 | 620.01 → 619.68 | 75 / 349,848 | 0 | 0 / 0 | 3.875 → 3.859 | 3.844 → 3.844 |
| parse/large_literal | 238.40 → 238.63 | 241.28 → 246.79 | 157 / 124,585 | 0 | 0 / 0 | 4.953 → 4.781 | 4.922 → 4.969 |
| parse/pathological | 5415.04 → 5444.53 | 5398.32 → 5411.04 | 777 / 19,168,951 | 0 | 0 / 0 | 11.031 → 11.016 | 11.062 → 11.094 |
| parse/method_names | 3309.59 → 3335.38 | 3338.82 → 3320.47 | 25,570 / 12,748,240 | 0 | 0 / 0 | 10.016 → 9.906 | 10.078 → 10.062 |
| glue_orders_cap | 239.29 → 238.02 | 241.48 → 240.73 | 2,055 / 168,822 | 31,272 → 31,279 | 156,242 / 4,192 | 7.016 → 6.922 | 7.094 → 6.984 |
| json_transform_cap | 35.50 → 35.78 | 35.96 → 36.12 | 479 / 36,161 | 6,257 → 6,264 | 35,661 / 128 | 6.484 → 6.516 | 6.531 → 6.438 |
| json_object_512_cap | 77.73 → 75.89 | 77.35 → 77.95 | 1,687 / 111,690 | 10,607 → 10,614 | 92,384 / 76,984 | 6.719 → 6.625 | 6.750 → 6.703 |
| hash_lookup_cap | 99.60 → 100.88 | 99.78 → 100.09 | 193 / 16,442 | 24,673 → 24,680 | 16,002 / 0 | 6.203 → 6.219 | 6.203 → 6.203 |
| member_calls_cap | 57.71 → 57.04 | 57.17 → 57.03 | 385 / 28,722 | 9,238 → 9,245 | 37,676 / 0 | 6.344 → 6.328 | 6.359 → 6.375 |
| record_fields_cap | 123.13 → 122.46 | 122.21 → 122.39 | 904 / 91,578 | 22,042 → 22,049 | 98,806 / 0 | 6.641 → 6.703 | 6.719 → 6.688 |
| record_build_cap | 329.51 → 330.25 | 325.41 → 329.43 | 2,142 / 252,010 | 59,176 → 59,183 | 233,110 / 0 | 6.578 → 6.625 | 6.609 → 6.641 |
| records_retained_cap | 118.86 → 118.86 | 118.29 → 117.80 | 1,162 / 132,890 | 20,126 → 20,133 | 122,182 / 115,188 | 6.672 → 6.609 | 6.703 → 6.609 |

### x86_64

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| parse/massive | 623.04 → 627.36 | 614.53 → 620.26 | 3,872 / 2,326,986 | 0 | 0 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| parse/bitwise_operations | 390.24 → 388.03 | 383.89 → 383.04 | 2,535 / 1,097,539 | 0 | 0 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| parse/wide_call | 25794.20 → 25670.47 | 25502.24 → 25710.19 | 200,106 / 79,781,772 | 0 | 0 / 0 | 43.102 → 43.109 | 43.117 → 43.117 |
| parse/wide_shape | 3471.09 → 3471.37 | 3450.01 → 3451.39 | 28,241 / 14,170,081 | 0 | 0 / 0 | 13.492 → 13.492 | 13.469 → 13.469 |
| parse/large_enum | 772.64 → 764.80 | 772.44 → 772.56 | 75 / 349,848 | 0 | 0 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| parse/large_literal | 204.12 → 203.90 | 203.77 → 203.99 | 157 / 124,585 | 0 | 0 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| parse/pathological | 5151.94 → 5168.87 | 5059.81 → 5069.56 | 777 / 19,168,943 | 0 | 0 / 0 | 14.133 → 14.133 | 14.133 → 14.133 |
| parse/method_names | 3975.43 → 3961.64 | 3946.02 → 3974.89 | 25,570 / 12,748,240 | 0 | 0 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| glue_orders_cap | 312.43 → 313.32 | 317.62 → 317.53 | 2,054 / 168,766 | 31,272 → 31,279 | 156,234 / 4,192 | 13.473 → 13.473 | 13.473 → 13.473 |
| json_transform_cap | 48.63 → 48.72 | 49.90 → 49.78 | 478 / 36,105 | 6,257 → 6,264 | 35,653 / 128 | 13.473 → 13.473 | 13.473 → 13.473 |
| json_object_512_cap | 97.37 → 97.79 | 97.22 → 97.59 | 1,686 / 111,634 | 10,607 → 10,614 | 92,376 / 76,984 | 13.473 → 13.473 | 13.473 → 13.473 |
| hash_lookup_cap | 150.34 → 149.96 | 150.07 → 150.87 | 192 / 16,386 | 24,673 → 24,680 | 15,994 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| member_calls_cap | 90.56 → 90.64 | 91.00 → 90.69 | 384 / 28,666 | 9,238 → 9,245 | 37,668 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| record_fields_cap | 190.31 → 190.32 | 190.25 → 190.08 | 903 / 91,522 | 22,042 → 22,049 | 98,798 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| record_build_cap | 493.33 → 492.69 | 494.63 → 492.75 | 2,141 / 251,954 | 59,176 → 59,183 | 233,102 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| records_retained_cap | 173.70 → 173.69 | 174.17 → 174.24 | 1,161 / 132,834 | 20,126 → 20,133 | 122,174 / 115,188 | 13.473 → 13.473 | 13.473 → 13.473 |

## Verification

- Formatting, both Clippy configurations, and all 2,116 workspace tests pass.
- All eight golden corpora pass; the paired audit has no observation changes.
- All 37,227 parse-sweep cases preserve their first diagnostic/acceptance,
  with no crashes, hangs, duplicate diagnostics or invalid spans.
- Portable/SIMD validation passes 107,682 shared cases with identical counters.
- WASI tests, CLI and filesystem witnesses pass.
- The final distributed gate is recorded separately in `gate-all.log`.

Raw evidence stays under
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/name-suffix/name-work/`: baseline
harness, before/after observations, counter audit and recording deltas,
verification logs, and per-host `measure/` and `parse/` benchmark outputs.
Remote originals are under each clone's `.cache/name-suffix/name-work/`.
No raw benchmark outputs or executables are committed.
