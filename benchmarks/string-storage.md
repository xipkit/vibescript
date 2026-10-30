# Runtime string storage

Runtime strings keep their `Vec<u8>` in their owning Arc and create a separate
shared vector owner only when imported into another budget. Host values and
compiler strings use the shared representation from creation. Imports, compiled
constants, and bytecode literal deduplication still share the original buffer;
an imported value does not keep its source call's budget alive.

The one-word string handle preserves the size of `Value` and VM frames. Its
tag selects the owned or shared Arc layout. Both layouts combine their storage
and header charges, which always belong to the same budget. Sharing uses a
`OnceLock` to transfer vector ownership once, including concurrent imports.

## Measurements

Baseline: `690d834b64203f2667fa46fa178519689fd5464f`. After: the source patch in
the evidence archive, SHA-256
`91b9139100acd4668bb93a0bdaefb1b35c90413065893c942e0906945ae11083`.
Measurements use an Apple M1 Max, arm64 macOS 27.0.1, Rust 1.98.1. The normal
`scripts/compare.py --suite json --rounds 8 --baseline <preserved binaries>`
rotates all four before/after portable/SIMD variants. Allocation instrumentation
is separate from timing; RSS is sampled in fresh processes. No rounds are
excluded. These are local arm64 measurements, not x86_64 performance claims.

The 1 MB metered workloads below show before → after. Requested bytes are
decimal MB; timings are medians in milliseconds.

| Workload | Allocations | Allocated MB | SIMD ms | Portable ms |
| --- | ---: | ---: | ---: | ---: |
| Parse | 89,841 → 77,003 | 8.250 → 7.942 | 8.696 → 8.730 | 8.817 → 8.680 |
| Typed shape | 89,842 → 77,004 | 6.608 → 6.300 | 10.998 → 11.324 | 11.167 → 11.083 |
| Typed array | 89,831 → 76,996 | 6.608 → 6.299 | 11.011 → 11.414 | 11.147 → 11.085 |
| Projection | 98,430 → 85,594 | 8.581 → 8.273 | 15.292 → 15.338 | 15.303 → 14.974 |
| Stringify | 83,407 → 83,406 | 8.770 → 8.052 | 11.422 → 11.370 | 11.453 → 11.316 |

Parsing removes approximately 14% of allocations and 4–5% of allocated bytes;
projection removes 13% of allocations. Stringify's allocation count is almost
unchanged, while its smaller imported headers reduce allocated bytes by 8.2%.
The timing tradeoff remains visible: SIMD typed-array parsing is 3.66% slower,
and typed-shape parsing is 2.96% slower. Across all 38 cases, SIMD median changes
range from −2.32% to +3.66%; portable changes range from −3.81% to −0.56%.

[All measurements](results/string-storage/summary.json) include both accounting
modes, every input size, allocation counts, requested bytes, tracked memory,
RSS, and all timing samples. The [evidence archive](results/string-storage/arm64-evidence.tgz)
contains raw measurements, inputs, build metadata, and the source patch.

## Accounting

On 64-bit targets, a new runtime string allocates a 72-byte header instead of
the previous two headers totaling 96 bytes. It reserves 112 bytes: the 72-byte
owner plus a possible future 40-byte shared vector owner. That promotion must
remain possible after the creating call has ended. Imported and compiler
strings reserve 80 bytes instead of 96 bytes. Buffer-capacity charging is
unchanged.

Consequently, lower actual allocations do not imply lower tracked memory for
every operation. The 1 MB parse case's tracked peak rises from 9,013,059 to
9,218,403 bytes; stringify's falls from 10,520,461 to 9,802,125 bytes. This
conservative accounting includes promotion headroom before the allocation
happens and keeps each call's ownership independent.

The paired audit covers all 233,882 engine/parser cases. It finds no stable
non-quota observation changes and no step-count changes among cases that
succeed in both builds. Memory counters change in 92,956 cases. Among tightly
limited replay cases, 3,203 now succeed, one newly exceeds its memory limit,
and 1,654 still exceed a memory limit at a different point. No compile outcome
changes. The same four clock/UUID-dependent replay cases retain their existing
variable observations.

Affected counters are recorded twice through the golden runner. Observations,
unaffected counters, and pre-existing counter offsets are preserved. The
[complete audit](results/string-storage/counter-audit.json.gz) retains every
counter delta and quota transition.

## Verification

Both scanner variants pass all 107,693 shared fixture expectations with equal
accounting. All six engine/parser golden corpora pass. Five strict-provenance
Miri tests cover both handle layouts, concurrent sharing/imports, independent
budget lifetimes, rejected imports, empty buffers, and spare capacity. Existing
compiler-constant and cross-call lifetime regressions also pass under Miri.
