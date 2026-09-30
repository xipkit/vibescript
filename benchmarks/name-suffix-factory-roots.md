# Suffixed callable factory roots

Review comment 4129884905 identified declaration-time rejection of factories
that return global callable descriptors under names such as `ready?`.
Factory declarations and preludes now share one builder: a terminal `?` or
`!` uses the method-spelling validator and an unsigned function declaration.
Binding still checks the actual result, rejecting data under a suffixed root.
Internal and repeated suffixes remain invalid; unsuffixed factory declarations
keep their existing `any` behavior. Factory callbacks never run during declaration.

Tests cover both suffixes, bare and parenthesized calls, data rejection at bind,
invalid spellings, and declared/granted prelude consistency. The public factory
API documentation, language guide, and ADR implementation notes describe this rule.

## Verification and counters

Formatting, both Clippy configurations, all 2,118 workspace tests, all eight
golden corpora, portable/SIMD validation, and WASI pass. The 37,227-case parse
sweep has no first-error changes. A paired audit of 197,906 runtime cases finds
zero counter or stable observation changes (excluding the same four existing
time/UUID-dependent cases as the golden checker). No Counter log entry is needed.

## Paired measurements

Before: `72536c1a`, using preserved `6190e184` binaries with identical `src/`.
After: `f7241028`, before this report and the documentation updates.
Eight rotating paired rounds use `scripts/compare.py --rounds 8 --target-ms 250`
for the eight capability workloads in both accounting modes and eight parser
cases. Hosts were quiet and reserved with the shared gate lock: `darwin`
(Apple M4, arm64 macOS) and `shannon` (Intel Core Ultra 9 285H, x86_64 Linux).
Linux runs pin core 2 and disable ASLR. Allocations use instrumented binaries;
RSS uses fresh processes. All variants pass fixture validation.

### arm64

Maximum timing increase: 1.21%; maximum RSS increase: 3.04%.
Allocations, requested bytes, steps, peak and retained tracked bytes are unchanged.

Arrows mean before → after. Runtime rows show metered mode; raw data also
includes unlimited mode. Unchanged accounting values apply to both variants.

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| glue_orders_cap/metered | 237.91 → 236.78 | 239.53 → 239.84 | 2,055 / 168,822 | 31,279 | 156,242 / 4,192 | 6.922 → 6.891 | 7.031 → 7.000 |
| json_transform_cap/metered | 35.35 → 35.28 | 35.83 → 35.93 | 479 / 36,161 | 6,264 | 35,661 / 128 | 6.484 → 6.406 | 6.484 → 6.484 |
| json_object_512_cap/metered | 75.92 → 75.79 | 77.18 → 76.60 | 1,687 / 111,690 | 10,614 | 92,384 / 76,984 | 6.672 → 6.516 | 6.688 → 6.672 |
| hash_lookup_cap/metered | 100.33 → 100.47 | 99.75 → 100.12 | 193 / 16,442 | 24,680 | 16,002 / 0 | 6.234 → 6.156 | 6.203 → 6.203 |
| member_calls_cap/metered | 57.00 → 57.19 | 57.08 → 57.08 | 385 / 28,722 | 9,245 | 37,676 / 0 | 6.391 → 6.297 | 6.359 → 6.344 |
| record_fields_cap/metered | 122.56 → 121.65 | 122.15 → 122.22 | 904 / 91,578 | 22,049 | 98,806 / 0 | 6.734 → 6.656 | 6.688 → 6.625 |
| record_build_cap/metered | 328.02 → 326.85 | 328.37 → 325.80 | 2,142 / 252,010 | 59,183 | 233,110 / 0 | 6.531 → 6.578 | 6.547 → 6.594 |
| records_retained_cap/metered | 117.79 → 116.57 | 116.67 → 117.25 | 1,162 / 132,890 | 20,133 | 122,182 / 115,188 | 6.625 → 6.547 | 6.688 → 6.703 |
| parse/massive | 540.34 → 536.59 | 539.29 → 538.97 | 3,872 / 2,326,986 | 0 | 0 / 0 | 4.609 → 4.594 | 4.688 → 4.703 |
| parse/bitwise_operations | 323.27 → 323.26 | 322.87 → 325.09 | 2,535 / 1,097,539 | 0 | 0 / 0 | 5.156 → 5.312 | 5.141 → 5.297 |
| parse/wide_call | 23662.20 → 23673.49 | 23733.95 → 23706.40 | 200,106 / 79,781,772 | 0 | 0 / 0 | 437.125 → 437.172 | 437.375 → 437.344 |
| parse/wide_shape | 3132.94 → 3110.25 | 3117.46 → 3132.09 | 28,241 / 14,170,081 | 0 | 0 / 0 | 9.812 → 9.812 | 9.969 → 9.922 |
| parse/large_enum | 619.43 → 604.87 | 619.58 → 604.67 | 75 / 349,848 | 0 | 0 / 0 | 3.797 → 3.797 | 3.859 → 3.859 |
| parse/large_literal | 236.00 → 236.17 | 243.74 → 241.66 | 157 / 124,585 | 0 | 0 / 0 | 4.906 → 4.906 | 4.859 → 4.984 |
| parse/pathological | 5454.87 → 5381.43 | 5430.81 → 5420.96 | 777 / 19,168,951 | 0 | 0 / 0 | 11.094 → 11.109 | 11.078 → 11.062 |
| parse/method_names | 3337.35 → 3327.22 | 3318.11 → 3351.82 | 25,570 / 12,748,240 | 0 | 0 / 0 | 9.859 → 9.875 | 10.047 → 10.047 |

### x86_64

Maximum timing increase: 0.97%; maximum RSS increase: 0.00%.
Allocations, requested bytes, steps, peak and retained tracked bytes are unchanged.

Arrows mean before → after. Runtime rows show metered mode; raw data also
includes unlimited mode. Unchanged accounting values apply to both variants.

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| glue_orders_cap/metered | 312.46 → 311.66 | 316.40 → 316.70 | 2,054 / 168,766 | 31,279 | 156,234 / 4,192 | 13.469 → 13.469 | 13.469 → 13.469 |
| json_transform_cap/metered | 48.54 → 48.78 | 49.96 → 49.89 | 478 / 36,105 | 6,264 | 35,653 / 128 | 13.469 → 13.469 | 13.469 → 13.469 |
| json_object_512_cap/metered | 97.79 → 97.68 | 97.89 → 97.67 | 1,686 / 111,634 | 10,614 | 92,376 / 76,984 | 13.469 → 13.469 | 13.469 → 13.469 |
| hash_lookup_cap/metered | 148.69 → 148.37 | 148.67 → 148.20 | 192 / 16,386 | 24,680 | 15,994 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| member_calls_cap/metered | 90.49 → 90.40 | 90.58 → 90.39 | 384 / 28,666 | 9,245 | 37,668 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| record_fields_cap/metered | 190.38 → 190.18 | 190.40 → 189.95 | 903 / 91,522 | 22,049 | 98,798 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| record_build_cap/metered | 489.45 → 490.49 | 490.59 → 489.59 | 2,141 / 251,954 | 59,183 | 233,102 / 0 | 13.469 → 13.469 | 13.469 → 13.469 |
| records_retained_cap/metered | 172.28 → 172.79 | 172.34 → 172.53 | 1,161 / 132,834 | 20,133 | 122,174 / 115,188 | 13.469 → 13.469 | 13.469 → 13.469 |
| parse/massive | 619.23 → 618.33 | 618.55 → 612.97 | 3,872 / 2,326,986 | 0 | 0 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| parse/bitwise_operations | 387.95 → 385.18 | 382.60 → 381.18 | 2,535 / 1,097,539 | 0 | 0 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| parse/wide_call | 25630.42 → 25548.18 | 25700.63 → 25472.15 | 200,106 / 79,781,772 | 0 | 0 / 0 | 43.109 → 43.109 | 43.117 → 43.117 |
| parse/wide_shape | 3446.02 → 3440.76 | 3423.69 → 3421.03 | 28,241 / 14,170,081 | 0 | 0 / 0 | 13.492 → 13.492 | 13.473 → 13.473 |
| parse/large_enum | 765.15 → 772.00 | 771.97 → 764.50 | 75 / 349,848 | 0 | 0 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| parse/large_literal | 204.08 → 204.26 | 204.37 → 206.02 | 157 / 124,585 | 0 | 0 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| parse/pathological | 5152.12 → 5138.03 | 5054.57 → 5051.42 | 777 / 19,168,943 | 0 | 0 / 0 | 14.133 → 14.133 | 14.133 → 14.133 |
| parse/method_names | 3946.19 → 3948.57 | 3956.73 → 3889.26 | 25,570 / 12,748,240 | 0 | 0 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |

## Evidence

The original arm64 `parse/bitwise_operations` RSS samples increased by 3.04%
portable and 3.03% SIMD. A control reran sixteen rotating samples each of
before, after, and a control using the exact same before binary at the same
runner path. Portable median RSS was 5.296875 → 5.296875 MiB (0.00%); SIMD
was 5.140625 → 5.148438 MiB (+0.15%). The identical-binary control medians
differed from before by -0.15% and +0.30%, respectively. The portable before
binary alone ranged from 5.125 to 5.343750 MiB, a 4.27% spread. Parser work
does not call the changed factory-declaration builder, and allocation counts
and requested bytes are identical. The isolated original RSS increases are
process sampling variation, not a change-induced regression. The original
samples remain in the tables and raw output; no timing measurements were
replaced. Control samples and procedure are in `darwin/rss-control/` and
`rss-control.py` under the artifact directory below.

Raw artifacts remain under
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/name-suffix/factory-roots/`:
`verification.json`, individual verification logs, `counter-audit.json`,
`sweep/summary.json`, and each host's `measure/` and `parse/` directories.
`bench.sh` and `compare-capabilities.py` record the exact measurement procedure.
The distributed gate runs against the final commit; its evidence is
`gate-all.log` and `gate-summary.json` in the same directory.
