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
