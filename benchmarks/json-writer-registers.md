# JSON writer register pressure

VIBE-184, compared with `202cb8a8` on OTIS (Apple M4), Rust 1.98.1,
release profile with thin LTO and overflow checks. These are local diagnostic
measurements, not release benchmarks.

The non-ASCII span scan, append and rune fallback now share a separate,
non-inlined frame. The outer writer carries its remaining input slice instead
of a separate index. Checkpoints use the equivalent descending remaining-byte
threshold; step charges, output limits and chunk boundaries are preserved.

## Instructions across placements

Each of four binaries was relinked at 32 placements, four bytes apart over
128 bytes, using VIBE-182's padding object and linker order file. `nm` verified
that every other symbol stayed at exactly the same address within each sweep.

Instructions per call are the difference between `/usr/bin/time -l` totals at
3,000 and 9,000 calls, divided by 6,000, repeated twice per placement. The table
uses the median across 32 placements. Process setup and scheduling introduce
some noise; small sub-percent differences should not be read as exact wins.
All cases use the script-facing API with unlimited accounting.

| Build | Payload | Before | After | Change |
|---|---|---:|---:|---:|
| simd | `lines_4k` | 62,619 | 62,186 | -0.7% |
| simd | `unicode_4k` | 119,872 | 111,344 | -7.1% |
| simd | `lone_escapes_4k` | 307,009 | 306,981 | -0.0% |
| portable | `lines_4k` | 72,232 | 71,679 | -0.8% |
| portable | `unicode_4k` | 110,106 | 112,777 | +2.4% |
| portable | `lone_escapes_4k` | 365,987 | 354,958 | -3.0% |

The ASCII-line fixture has 59 ordinary bytes and LF, repeated 69 times.
The Unicode fixture repeats `é界🙂` 456 times. Lone escapes repeat `ab\n`
1,366 times. Both ASCII patterns now have permanent suite fixtures.

## Assembly audit

| SIMD path | Before | After |
|---|---|---|
| `write_string` | 1,144 instructions; 368-byte frame | 811 instructions; 288-byte frame |
| Ordinary scalar scan | 13 instructions per ordinary byte | 13, with both masks outside the byte loop |
| Non-ASCII span loop | One stack reload plus mask reconstruction per rune | Neither in the helper's loop |
| Escape run | 15 instructions per escape, cursors in registers | Same hot loop; nonempty input removes an entry check |

The called `rune_width` function still saves/restores its frame; the stack
comparison above concerns the span loop itself. This fresh baseline has one
span-loop reload, rather than the six reported by the earlier VIBE-182 build.
Portable already calls `prefix` out of line and has no per-rune span-loop
spill; the helper adds about two percent instructions on its Unicode case.

Extracting only the helper without changing the outer cursor was rejected:
LLVM rebuilt both ASCII masks inside the scalar loop, taking it from 13 to 17
instructions per byte and adding about four percent on ASCII lines. Passing
a mutable index to the helper had the same problem. Both rejected SIMD shapes
were also checked across 32 placements.

## Timing across placements

After the local gates finished, the same 128 binaries ran in randomized order
for 15 rounds at 6,000 calls per case. Each entry is the range of placement
minima in microseconds, with their median in parentheses. Host load rose from
3.9 to 12.4 during the run. Earlier measurements during compilation were too
noisy to interpret and are excluded.

| Build | Payload | Before | After |
|---|---|---:|---:|
| simd | `lines_4k` | 2.329–2.486 (2.407) | 2.291–2.565 (2.381) |
| simd | `unicode_4k` | 5.428–5.503 (5.465) | 5.410–5.491 (5.447) |
| simd | `lone_escapes_4k` | 9.198–9.610 (9.261) | 8.894–9.826 (9.304) |
| portable | `lines_4k` | 2.584–2.688 (2.638) | 2.572–2.713 (2.623) |
| portable | `unicode_4k` | 5.399–5.486 (5.438) | 5.399–5.495 (5.442) |
| portable | `lone_escapes_4k` | 10.978–11.293 (11.152) | 10.809–11.398 (11.111) |

Unicode timing is essentially unchanged despite the SIMD instruction reduction.
Lone escapes still depend on placement: their SIMD spread widens from 4.5% to
10.5%, while the median moves only +0.5% and the worst placement +2.2%. This is
not a fix for that sensitivity. Its remaining work is tracked separately in
VIBE-187; it must be remeasured after VIBE-180's scanner rewrite lands.

## Limits and tradeoffs

Calling the helper for many tiny Unicode spans costs more. In an additional
4 KiB `é\n` workload, SIMD instructions increased from approximately 364,500 to
398,700 per call (+9.4%). A mixed `ab\né界🙂` workload increased about 1.9%.
Output and accounting remained identical. This change isolates the scanners;
it does not make every payload faster or guarantee immunity to future LLVM
allocation and code-placement changes.

## Correctness

All 30 JSON benchmark cases match the baseline's expected output, digest,
steps, peak bytes and retained bytes on both feature sets. The gate suites
passed 2,485 tests with SIMD/all features and 2,445 portable tests. After the
final loop change, the SIMD library and integration suites were rerun (754
and 1,278 tests). Formatting, Clippy and Adamantium pass on both feature sets.

A new differential test covers valid UTF-8, malformed sequences, Unicode
line/paragraph separators and escaped tails at every offset from four bytes
before through four bytes after a chunk boundary. It checks every step quota
and nearby output limits against the per-append writer and verifies cleanup.

## Refresh against master `04395536`

After merging master on November 5, the only conflict was the compressed
conformance counters. The resolution preserves all 1,115 master records and
adds this branch's four benchmark records. No scanner code changed in the
merge. The following measurements compare fresh builds of that master with
the merged branch, using the same machine, compiler, profile and differential
instruction-count method above. Every build again has 32 placements; all
unrelated symbol addresses remain fixed within each sweep.

| Build | Payload | Before | After | Change |
|---|---|---:|---:|---:|
| simd | `lines_4k` | 62,567 | 62,164 | -0.6% |
| simd | `unicode_4k` | 120,922 | 111,227 | -8.0% |
| simd | `lone_escapes_4k` | 307,107 | 307,004 | -0.0% |
| portable | `lines_4k` | 72,295 | 71,638 | -0.9% |
| portable | `unicode_4k` | 111,423 | 112,724 | +1.2% |
| portable | `lone_escapes_4k` | 366,122 | 354,952 | -3.1% |

The SIMD writer is now 1,140 → 811 instructions, still with a 368 → 288-byte
frame. The ordinary scalar loop remains 13 instructions per byte with its
masks outside the back edge. The Unicode span loop loses its one stack reload
and mask reconstruction per rune. The escape-run loop remains 15 instructions
per escape, with its cursors in registers. Portable still calls `prefix` out
of line, and neither its old nor its extracted Unicode loop spills per rune.

All 30 JSON benchmark outputs, digests and accounting match across the four
fresh binaries, as do 22 additional escape/Unicode patterns. Every placement
also has matching digests, steps, peak and retained memory. Short Unicode
spans retain their cost: `é\n` increases SIMD instructions by 9.3%, and mixed
`ab\né界🙂` by 2.0%.

Fifteen randomized timing rounds at 6,000 calls per case ran alongside the
local test gate; host load rose from 4.48 to 8.09. These remain diagnostic
timings. Entries are the range of placement minima in microseconds, with
their median in parentheses.

| Build | Payload | Before | After |
|---|---|---:|---:|
| simd | `lines_4k` | 2.362–2.493 (2.423) | 2.294–2.582 (2.424) |
| simd | `unicode_4k` | 5.504–5.599 (5.550) | 5.498–5.565 (5.530) |
| simd | `lone_escapes_4k` | 9.349–9.767 (9.408) | 9.018–9.892 (9.447) |
| portable | `lines_4k` | 2.619–2.730 (2.680) | 2.591–2.736 (2.657) |
| portable | `unicode_4k` | 5.495–5.568 (5.522) | 5.454–5.569 (5.510) |
| portable | `lone_escapes_4k` | 11.214–11.485 (11.323) | 10.945–11.566 (11.218) |

Lone-escape sensitivity remains: the SIMD placement spread is 4.5% before
and 9.7% after, with the median +0.4% and the worst placement +1.3%.
VIBE-187 still owns that follow-up; the extraction does not fix it.

Release engine golden runs pass all 233,902 cases on each feature set, and
130,697 stable successful accounting records match across them. The four
benchmark records pass strict counter checks on both. The full corpus still
reports nonblocking drift against older counter records; those records were
not changed by the conflict resolution.

The full `scripts/check` gate passes: formatting, Clippy and Adamantium on
both feature sets; 2,518 debug tests, 2,517 optimized all-feature tests,
2,477 optimized portable tests and 61 documentation tests; all engine goldens,
2,867 CLI cases and 216 LSP cases.

Raw builds, disassembly and per-placement results for this refresh are under
`/Volumes/AI/Build/tron-vibe-184/refresh/`; `harness/counts.json` contains the
128-placement instruction results and `harness/quiet-timings.json` the timing
samples (the filename is inherited from the original harness).
