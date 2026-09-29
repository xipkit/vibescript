# Object data keys and callable-name validation

Review comment 4129511218 identified a key conversion before the callable
check in object import. Import now checks for a host method or exported
function before requiring its key to spell a method. Ordinary data retains
its original keys.

The audit covered capability templates and factories, root globals, host
publication, registered functions, module exports, and recursive imports.
Other method-name validator callers already check callability or receive
only callable declarations. Root data bindings still use the separate
binding-name rule. No additional production changes were needed.

Two regression tests cover nested `Value::object` values with an integer
key, a punctuation-bearing string key, and a non-UTF-8 byte key through host
returns, an exported module function, globals, capability templates and
factories, and publication/receiver snapshots. They failed before the fix.

The literal `object[1] = 2` example is already rejected on master: typed hash
writes report V0101, and runtime indexing also rejects integer keys. The
unit fixture therefore constructs the equivalent `(1, 2)` object entry
before testing the public import and host round-trip paths. Existing hash
indexing rules are unchanged; the evidence is in
`numeric-index-existing-rejection.txt`.

## Paired measurements

Before: `ebbce4f2` (preserved binaries from `a9baf423f`, with identical `src/`).
After: `2820b77c`, the fix before this report was added. Eight rotating paired
rounds of `scripts/compare.py --rounds 8 --target-ms 250 --baseline <before>`
use its core cases with `capability_probe`, covering eight workloads in both
metered and unlimited modes. Both builds pass fixture/golden validation.
Hosts were quiet and reserved through the shared gate lock: `darwin` (Apple
M4, arm64 macOS) and `shannon` (Intel Core Ultra 9 285H, x86_64 Linux). Linux
runs pin core 2 and disable ASLR. Allocation instrumentation is separate
from timing; RSS samples use fresh processes.

Across both accounting modes and build variants, allocation counts,
requested bytes, steps, tracked peak and retained bytes are unchanged.
The maximum timing increases are 0.63% on arm64 and 0.40% on x86_64. All
RSS increases stay below 3%; no outlier exclusion or repeat is needed.

Tables show the metered cases. Arrows mean before → after; accounting
values without arrows are unchanged in both portable and SIMD builds.

### arm64

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| glue_orders_cap | 237.88 → 237.99 | 240.01 → 240.87 | 2,055 / 168,822 | 31,272 | 156,242 / 4,192 | 6.844 → 6.984 | 6.922 → 6.984 |
| json_transform_cap | 35.88 → 35.29 | 35.72 → 35.79 | 479 / 36,161 | 6,257 | 35,661 / 128 | 6.422 → 6.484 | 6.500 → 6.547 |
| json_object_512_cap | 76.04 → 76.01 | 76.09 → 76.26 | 1,687 / 111,690 | 10,607 | 92,384 / 76,984 | 6.688 → 6.703 | 6.609 → 6.703 |
| hash_lookup_cap | 99.25 → 99.47 | 100.04 → 99.71 | 193 / 16,442 | 24,673 | 16,002 / 0 | 6.172 → 6.188 | 6.172 → 6.219 |
| member_calls_cap | 57.05 → 57.13 | 57.38 → 57.07 | 385 / 28,722 | 9,238 | 37,676 / 0 | 6.281 → 6.328 | 6.297 → 6.344 |
| record_fields_cap | 122.06 → 122.41 | 121.95 → 122.17 | 904 / 91,578 | 22,042 | 98,806 / 0 | 6.562 → 6.641 | 6.672 → 6.703 |
| record_build_cap | 326.61 → 325.87 | 325.29 → 326.17 | 2,142 / 252,010 | 59,176 | 233,110 / 0 | 6.594 → 6.562 | 6.578 → 6.609 |
| records_retained_cap | 117.35 → 117.30 | 117.51 → 117.15 | 1,162 / 132,890 | 20,126 | 122,182 / 115,188 | 6.625 → 6.688 | 6.656 → 6.734 |

### x86_64

| Case | SIMD µs | Portable µs | Allocations / requested B | Steps | Peak / retained B | SIMD RSS MiB | Portable RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| glue_orders_cap | 311.89 → 310.95 | 316.33 → 316.45 | 2,054 / 168,766 | 31,272 | 156,234 / 4,192 | 13.473 → 13.473 | 13.473 → 13.473 |
| json_transform_cap | 48.89 → 48.52 | 49.78 → 49.92 | 478 / 36,105 | 6,257 | 35,653 / 128 | 13.473 → 13.473 | 13.473 → 13.473 |
| json_object_512_cap | 97.78 → 97.58 | 97.73 → 97.59 | 1,686 / 111,634 | 10,607 | 92,376 / 76,984 | 13.473 → 13.473 | 13.473 → 13.473 |
| hash_lookup_cap | 148.89 → 149.00 | 148.72 → 149.15 | 192 / 16,386 | 24,673 | 15,994 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| member_calls_cap | 90.55 → 90.54 | 91.00 → 91.14 | 384 / 28,666 | 9,238 | 37,668 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| record_fields_cap | 191.84 → 191.30 | 190.93 → 191.02 | 903 / 91,522 | 22,042 | 98,798 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| record_build_cap | 491.51 → 491.55 | 493.25 → 493.12 | 2,141 / 251,954 | 59,176 | 233,102 / 0 | 13.473 → 13.473 | 13.473 → 13.473 |
| records_retained_cap | 172.80 → 172.28 | 172.97 → 172.83 | 1,161 / 132,834 | 20,126 | 122,174 / 115,188 | 13.473 → 13.473 | 13.473 → 13.473 |

## Verification

- Formatting, Clippy with all features and with no default features, and all
  2,112 workspace tests pass.
- All eight golden corpora pass. A paired audit of 197,906 runtime cases
  finds no stable observation or counter differences; it excludes the same
  four existing time/UUID-dependent cases as the golden checker.
- All 37,227 parse-sweep cases preserve their first diagnostic/acceptance,
  with no crashes, hangs, duplicate diagnostics or invalid spans.
- Portable/SIMD validation passes 107,682 shared cases with identical
  tracked counters.
- WASI tests, CLI and filesystem witnesses pass.
- No new Counter log entry or golden recording is needed.
- The final distributed gate is recorded separately in `gate-all.log`.

Raw logs, immutable baseline harness, benchmark binaries/round data, and
validation outputs stay under
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/name-suffix/object-keys/`.
Benchmark raw outputs are copied to its `darwin/measure/` and
`shannon/measure/` directories; remote originals are under the corresponding
clone's `.cache/name-suffix/object-keys/`. Only this summary is committed.
