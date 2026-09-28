# Bootstrap service footprint

No library optimization survives the requested regression limit. The branch
retains the fresh-process service benchmark, native RSS probes, compiler-identity
checks and allocation/retention guards. Production sources and build settings at
`84c8c82c` are identical to upstream `3161370a`. The preserved baseline `7ab17d93`
uses those production sources and the same measurement harness.

The final integration rebase includes upstream `a23d4ece` (VM round 3).
Production sources, Cargo settings and the core comparison harness still match
that upstream exactly. The tables below retain their original measured revisions;
they do not claim to measure the later VM changes. Post-rebase validation is
recorded in `post-vm3/validation.json` under the results directory below:
formatting, both Clippy modes, 2,007 workspace tests, golden validation, portable/SIMD
counter equality and WASI all pass. Full logs remain in the external cache under
`post-vm3/`, and the final gate records its exact tested revision under `final/`.

## Rejected changes and regression fixes

Compiled-buffer compaction was removed: its 11.53% script-heap saving cost 1.26%
first-compile latency on arm64 and 1.19% on x86, exceeding the allowed 1% tradeoff.
The lazy module-cache experiment saved only eight bytes and was also removed.

Borrowing signature-parser tokens removed 3,740 allocations and improved startup,
but the rebased arm64 comparison retained a 3.3–3.6% concatenation RSS regression.
Storing byte offsets instead of duplicate token coordinates removed another
64 KiB of temporary storage. On x86 that candidate reduced first-compile peak
heap by 13.41% and latency by 9.78%, but escaped JSON stringification remained
8.4–9.0% slower in the supplementary alternating test. Both parser changes were
therefore removed. Their measurements are preserved as rejected candidates.

Earlier x86 comparisons also mixed Arch and rustup Rust distributions. Matching
version and LLVM numbers did not mean matching standard-library builds. The
benchmark now checks full compiler identity, flags and preserved binary hashes;
a metadata bug that overwrote compiler flags with example arguments was fixed.
Current x86 pairs use Arch Rust and arm64 pairs use Homebrew Rust, both 1.98.1.
The earlier instruction-layout and allocator diagnostics are evidence of
sensitivity, not proof of one particular cache effect. The final remedy is to
remove the changes that fail the measured acceptance limits.

The service registers four typed host functions and four capabilities, compiles
100 distinct site programs (159,089 source bytes), and drops each of 1,000 call
results. Each host ran 64 alternating fresh-process pairs, with separate release
binaries for timing/RSS and allocation accounting. Core runtime comparisons use
eight alternating rounds and supplementary repetitions of outliers and original
regression witnesses. Shannon is pinned to performance core 2. Optimization level,
LTO and panic behavior are unchanged.

Allocation counts and requested bytes are cumulative. Live/peak heap count Rust
allocator requests, excluding allocator overhead, native allocations and stacks.
Phase timings cover work since the preceding checkpoint. Timings use the median
of complete two-round means, balancing AB/BA process order within each sample.
All per-process timings remain recorded. Byte-identical arm64 executable text
exposed two cold-start modes (first-in-pair about 2.77 µs, second about 3.50 µs);
a pooled constructor median produced a false 10.29% gap. Balanced blocks reduce
that to 1.01%. The same aggregation is applied to every service phase and host.
Core timing aggregation is unchanged. These fresh measurements
supersede the earlier pre-VM and rejected parser results.

## darwin

| Phase | RSS bytes, before → after | Live heap bytes | Peak heap bytes | Allocations | Requested bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| process start | 1,622,016 → 1,622,016 | 612 → 612 | 612 → 612 | 3 → 3 | 612 → 612 |
| engine new | 1,687,552 → 1,687,552 | 892 → 892 | 892 → 892 | 7 → 7 | 892 → 892 |
| registered | 2,588,672 → 2,588,672 | 16,296 → 16,296 | 16,392 → 16,392 | 209 → 209 | 23,584 → 23,584 |
| compiled 1 | 5,849,088 → 5,849,088 | 508,851 → 508,851 | 1,105,810 → 1,105,810 | 13,607 → 13,607 | 2,398,600 → 2,398,600 |
| compiled 10 | 6,946,816 → 6,963,200 | 692,638 → 692,638 | 1,105,810 → 1,105,810 | 35,106 → 35,106 | 6,778,543 → 6,778,543 |
| compiled 100 | 9,797,632 → 9,781,248 | 2,490,708 → 2,490,708 | 2,806,816 → 2,806,816 | 257,150 → 257,150 | 51,486,743 → 51,486,743 |
| calls 1000 | 11,042,816 → 11,026,432 | 2,493,583 → 2,493,583 | 2,806,816 → 2,806,816 | 1,476,119 → 1,476,119 | 147,155,850 → 147,155,850 |
| scripts dropped | 11,042,816 → 11,026,432 | 495,627 → 495,627 | 2,806,816 → 2,806,816 | 1,476,119 → 1,476,119 | 147,155,850 → 147,155,850 |

| Timed work | Before | After | Change |
| --- | ---: | ---: | ---: |
| engine new | 3.115 µs | 3.146 µs | +1.01% |
| compiled 1 | 831.812 µs | 833.969 µs | +0.26% |
| compiled 10 | 2,166.229 µs | 2,199.208 µs | +1.52% |
| compiled 100 | 20,142.552 µs | 20,111.979 µs | -0.15% |
| calls 1000 | 85,719.292 µs | 85,645.625 µs | -0.09% |

Stripped `vibes`: 6,265,808 → 6,265,808 bytes.
Incremental retained heap per script: 19,978.6 → 19,978.6 bytes.
Sandbox counters are identical: 9,549,190 steps, 92,912 bytes maximum call peak, and 2,392,760 bytes summed across returned values (not simultaneous retained memory).

### Resident mapping composition

Eight alternating mapping pairs, separate from timing runs; medians in bytes.

| Resident component | After 100 compilations, before → after | After 1,000 calls, before → after |
| --- | ---: | ---: |
| Process RSS | 9,871,360 → 9,797,632 | 11,108,352 → 11,018,240 |
| Executable text and read-only data | 2,998,272 → 2,998,272 | 3,768,320 → 3,768,320 |
| Other executable segments | 147,456 → 147,456 | 147,456 → 147,456 |
| Allocator resident pages | 5,693,440 → 5,619,712 | 6,160,384 → 6,070,272 |
| Native allocated bytes, inside allocator pages | 2,805,584 → 2,817,360 | 2,941,584 → 2,940,512 |
| Allocator free-space/overhead estimate | 2,888,256 → 2,802,352 | 3,220,176 → 3,128,936 |
| Thread stacks | 49,152 → 49,152 | 49,152 → 49,152 |
| Other charged resident pages | 983,040 → 983,040 | 983,040 → 983,040 |
| RSS reclaimed by diagnostic pressure relief | 0 → 0 | 0 → 0 |
| Pressure-relief time, ns | 542 → 584 | 562 → 626 |

The core run measures 122 cases per flavor and validates 107,466 cases per flavor. Counter differences: 0; allocation regressions: 0. Initial timing/RSS outliers: 0/2. Supplementary timing/RSS regressions: 0/0. Raw outliers remain recorded, not discarded.

## shannon

| Phase | RSS bytes, before → after | Live heap bytes | Peak heap bytes | Allocations | Requested bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| process start | 3,182,592 → 3,188,736 | 548 → 548 | 548 → 548 | 2 → 2 | 548 → 548 |
| engine new | 3,465,216 → 3,465,216 | 820 → 820 | 820 → 820 | 6 → 6 | 820 → 820 |
| registered | 5,312,512 → 5,351,424 | 16,224 → 16,224 | 16,312 → 16,312 | 208 → 208 | 23,480 → 23,480 |
| compiled 1 | 8,071,168 → 8,060,928 | 508,827 → 508,827 | 1,105,738 → 1,105,738 | 13,606 → 13,606 | 2,399,144 → 2,399,144 |
| compiled 10 | 8,466,432 → 8,464,384 | 692,686 → 692,686 | 1,105,738 → 1,105,738 | 35,105 → 35,105 | 6,784,383 → 6,784,383 |
| compiled 100 | 10,964,992 → 10,948,608 | 2,491,612 → 2,491,612 | 2,807,800 → 2,807,800 | 257,149 → 257,149 | 51,546,655 → 51,546,655 |
| calls 1000 | 11,880,448 → 11,868,160 | 2,494,487 → 2,494,487 | 2,807,800 → 2,807,800 | 1,475,118 → 1,475,118 | 147,143,762 → 147,143,762 |
| scripts dropped | 11,880,448 → 11,868,160 | 495,595 → 495,595 | 2,807,800 → 2,807,800 | 1,475,118 → 1,475,118 | 147,143,762 → 147,143,762 |

| Timed work | Before | After | Change |
| --- | ---: | ---: | ---: |
| engine new | 11.092 µs | 10.659 µs | -3.91% |
| compiled 1 | 1,014.848 µs | 1,020.832 µs | +0.59% |
| compiled 10 | 2,793.582 µs | 2,796.482 µs | +0.10% |
| compiled 100 | 27,454.098 µs | 27,431.782 µs | -0.08% |
| calls 1000 | 118,555.727 µs | 118,706.538 µs | +0.13% |

Stripped `vibes`: 7,349,408 → 7,349,408 bytes.
Incremental retained heap per script: 19,988.1 → 19,988.1 bytes.
Sandbox counters are identical: 9,549,190 steps, 92,904 bytes maximum call peak, and 2,392,760 bytes summed across returned values (not simultaneous retained memory).

### Resident mapping composition

Eight alternating mapping pairs, separate from timing runs; medians in bytes.

| Resident component | After 100 compilations, before → after | After 1,000 calls, before → after |
| --- | ---: | ---: |
| Process RSS | 10,950,656 → 10,928,128 | 11,866,112 → 11,802,624 |
| Executable text and read-only data | 4,587,520 → 4,548,608 | 5,261,312 → 5,210,112 |
| Other executable segments | 8,192 → 8,192 | 8,192 → 8,192 |
| Allocator resident pages | 3,663,872 → 3,649,536 | 3,667,968 → 3,661,824 |
| Native allocated bytes, inside allocator pages | 3,195,808 → 3,197,552 | 3,260,936 → 3,262,984 |
| Allocator free-space/overhead estimate | 465,672 → 451,504 | 404,992 → 401,360 |
| Thread stacks | 49,152 → 49,152 | 49,152 → 49,152 |
| Other charged resident pages | 2,686,976 → 2,670,592 | 2,889,728 → 2,877,440 |
| RSS reclaimed by diagnostic pressure relief | 149,504 → 145,408 | 100,352 → 98,304 |
| Pressure-relief time, ns | 12,296 → 12,440 | 10,666 → 11,083 |

The core run measures 122 cases per flavor and validates 107,466 cases per flavor. Counter differences: 0; allocation regressions: 0. Initial timing/RSS outliers: 0/0. Supplementary timing/RSS regressions: 0/0. Raw outliers remain recorded, not discarded.

## RSS interpretation

The raw captures use `vmmap` on macOS and `/proc/<pid>/smaps` on Linux.
Rust-requested heap is a subset of native allocation, which is inside allocator
resident pages; these are overlapping measurements and must not be added.
Allocator slack subtracts native allocated bytes from resident allocator pages.
It includes overhead and is not a count of reclaimable pages. Component medians
need not sum exactly to the median total.

Linux malloc regions include the labelled heap and writable portions of guarded
64 MiB glibc arenas. Other anonymous mappings remain separate. On macOS, shared
cache residency includes pages not charged to this process's Mach RSS, so the
remaining charged resident bytes are reported as a residual rather than adding
the whole shared cache. The macOS executable text/read-only category is `__TEXT`;
other executable segments include `__DATA_CONST`. Raw mappings and allocator
statistics are preserved. The Linux RSS checkpoint precedes the map capture;
some captures gain one page between those reads.

Timezone data and Unicode/casing tables already reside in immutable, demand-paged
binary storage, with no startup heap copies. The timezone index and parsed
signature table are already lazy. Neighboring accesses and OS read-ahead can
still bring unused table pages into RAM. Source text and position data are needed
for diagnostics; checker/compiler scratch already dies after compilation.

No retired large checker stack appears in the snapshots. The native checker
reserves 64 MiB only while needed; deeply nested surface checking can reserve
256 MiB. A standalone pthread probe confirmed that macOS's 64 MiB Memory Tag 22
mapping predates thread creation and is not a checker stack. A persistent-checker
prototype added about 1 KiB live heap and 48 KiB resident stack with no useful RSS
saving, so it was discarded.

Diagnostic pressure relief does not reduce RSS on macOS and reduces it by only
a small fraction on Linux. It remains opt-in in the probe; automatic process-wide
trimming would add allocator work for too little benefit under the requested tradeoff.
No production change is retained. The tools measure the unchanged steady-state
script storage and make future regressions visible without accepting a throughput
tradeoff that exceeded the requested limits.

## Guards and verification

The isolated allocation test bounds Engine construction at 16 allocations / 4 KiB,
first compilation at 16,000 allocations, retained script storage at 28 KiB per
script, and storage left after a 4 MiB call plus 1,000 small calls at 4 KiB.
The first-compile guard allows the original lexer while bounding future growth.

Verification results and exact revisions are recorded beside the fresh summaries.
At the measured revision, the native suites pass 2,004 tests. Formatting, both Clippy configurations, golden
validation and WASI pass. No golden observations or sandbox counters changed
relative to the preserved upstream baseline; no Counter log entry or golden
re-recording is needed. Raw artifacts remain on the external volume at
`/Volumes/AI/Work/xipkit/vibescript.rs/.cache/mgomes-footprint/`, with the
per-round results and fresh summaries under `benchmarks/results/footprint/` there.
The exact-head three-host gate log and revision metadata are saved under
`final/gate-all.log` and `final/gate-result.json` in that directory.
