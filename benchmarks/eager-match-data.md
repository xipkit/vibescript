# Cheap eager match data

VIBE-128 follows Mauricio's decision to optimize eager construction for
[#1338](https://github.com/xipkit/vibescript/issues/1338). Match data remains
the same protected seven-field hash. `Value::as_hash`, Offset identity and
character offsets, detached partial strings, rendering and host imports keep
their existing behavior.

Seven fixed keys and empty captures, named captures and string windows now
share immutable process constants. These contain no invocation data and need
no per-match storage charge. Importing them into another call follows the
ordinary destination-budget rules. Dynamic fields, nonempty windows, arrays,
offsets and hash storage remain metered. Captures are built directly instead
of being copied from a temporary whole-match vector. Offsets stay eager and
retain their existing representation; they do not acquire a retained subject
or move rune-scanning work to accessor calls.

## Measurement

Baseline: `4cb18cb4ab042a0968eed3cc5303b7da9cdbc1b8`. Apple M4, arm64,
macOS 27.0.1, Rust 1.98.1 (`48a229cea`), release profile with thin LTO,
one codegen unit and overflow checks. SIMD and portable builds each use
separate binaries for timing and allocator instrumentation.

The exact `regex_log_lines` fixture comes from `scripts/fixtures.py`: 128 lines,
102 matches and 26 invalid lines. Controls use the same input for unread,
numbered and repeated capture reads, offsets, whole-match storage, groupless
matches, named captures and `match?`. A separate 1,000,001-byte subject reads
only its small capture. Each runs with normal and unlimited quotas. All four
binaries validate every case against an independent expected result before
measurement. The original fixture's source and expected result are unchanged.

Allocation samples contain 100 calls. Timing uses eight paired rounds,
alternating baseline/candidate order and targeting 200 ms per sample,
with the existing harness's 32-call warmup. Initial compilation,
constant initialization and result serialization are outside the timed calls.
The harness keeps the last result alive through the next call in both builds.
This is a shared development machine; ranges and unchanged controls help show
noise. These are local ARM measurements, not x86 performance claims.

| Workload (SIMD, metered) | Allocations before → after | Allocated bytes before → after | Median µs before → after |
| --- | ---: | ---: | ---: |
| regex_log_lines | 5,558 → 3,722 | 474,974 → 384,704 | 594.92 → 544.57 |
| unread | 5,532 → 3,696 | 472,270 → 382,000 | 593.11 → 541.13 |
| index0 | 5,532 → 3,696 | 472,270 → 382,000 | 604.38 → 551.01 |
| index1 | 5,532 → 3,696 | 472,270 → 382,000 | 605.98 → 558.59 |
| repeat_index | 6,144 → 4,308 | 517,966 → 427,696 | 704.60 → 653.69 |
| offsets | 5,762 → 3,926 | 492,110 → 401,840 | 681.10 → 624.61 |
| whole_store | 5,539 → 3,703 | 476,678 → 386,408 | 633.18 → 572.38 |
| unread_groupless | 4,690 → 2,752 | 380,670 → 284,688 | 525.42 → 465.11 |
| predicate | 1,936 → 1,936 | 216,768 → 216,768 | 409.88 → 404.96 |
| named | 6,246 → 4,512 | 521,638 → 443,608 | 659.48 → 605.41 |
| long_subject | 54 → 38 | 1,004,343 → 1,003,634 | 83.60 → 82.44 |

The original loop uses **33.0% fewer allocations**, **19.0% fewer allocated
bytes**, and is **8.5% faster** in the metered SIMD trials. Whole-match storage
improves 9.6%. The metered portable loop improves from 647.62 to 593.90 µs
(8.3%), with its unchanged predicate control at 445.65 → 446.89 µs.

Raw [SIMD](results/eager-match-data/simd.json) and
[portable](results/eager-match-data/portable.json) samples include every round,
min/max/median, source and fixture hashes, binary hashes and validation results.

The first portable unlimited predicate trial improved 9% despite using an
unchanged path. Those trials overlapped portable test compilation, so their
timing gains should not all be attributed to this patch. After compilation,
an [eight-round follow-up](results/eager-match-data/portable-controls.json)
targeting 400 ms samples measured the unlimited log loop at 595.53 → 550.08 µs
(7.6% faster) and the predicate control at 400.60 → 397.10 µs (0.9% faster).
All original samples are retained. Allocation counts agree in both runs.

The long-subject case still constructs nonempty pre/post windows and eager
offset arrays. This change reduces fixed construction costs; it does not
implement whole-match laziness or change search scratch allocation.

## Accounting audit

A paired same-checkout sweep compares 197,937 engine cases. All observations
agree except four existing clock-dependent replay results; no quota outcome
changes. Only 749 cases change counters: three conformance, 661 language,
two compatibility and 83 replay cases. Steps and peak bytes fall in all 749;
retained bytes also fall in 29. The
[per-case audit](results/eager-match-data/counters.jsonl) records before/after
steps, peak and retained bytes. Every unselected counter and every golden
observation is preserved. Selected cases were recorded twice and pass strict
counter/quota verification.

For the exact log loop, steps fall from 41,165 to 40,349, tracked peak from
79,212 to 77,967 bytes, and retained storage stays at 55,768 bytes. Fewer
copies and key constructions account for the removed work; this introduces
no deferred allocation or later failure boundary.

## Reproduction

Build `examples/compare.rs` at the baseline and candidate revisions in separate
targets. Use `cargo build --locked --release --example compare` for SIMD timing,
add `--features allocation-stats` for its allocator binary, and add
`--no-default-features` to each for portable builds. Save each binary before
the next build replaces it. Keep targets and caches outside the source tree.

Run each pair from the candidate checkout:

```sh
python3 scripts/bench_match_data.py \
  --baseline /path/to/baseline-timing \
  --baseline-alloc /path/to/baseline-alloc \
  --candidate /path/to/candidate-timing \
  --candidate-alloc /path/to/candidate-alloc \
  --minimum-ms 200 \
  --out /path/to/new-results-directory
```

The script generates exact fixtures, independently checks outputs, records
binary/source/fixture hashes, counters, allocator samples and every timing
round. `--case NAME` selects controls for follow-up measurements.
