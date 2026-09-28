# Exact memory accounting with fewer x86 atomic updates

On x86, ordinary tracked allocations now require three atomic read-modify-write operations instead of approximately five. The executing call owns cumulative reservation and peak updates; releases remain atomic, and charges retain their existing `Arc` ownership. Steps, quotas, tracked peaks, retained bytes, charge sizes and public charging APIs are unchanged. ARM keeps its original accounting implementation.

## Measurements

Delivery is rebased onto `32ae7e77`, which adds the VM agent's regex-literal and array-loop optimizations. The accounting source is byte-for-byte identical to the measured version (`src/budget.rs` blob `5ea50a829b8416a2784900cc90dc5e2fbb9472fa`), and upstream's golden updates are retained. The timings below isolate this change at the original baseline; integration correctness is checked again after rebasing.

Eight paired rounds compare baseline `a4cc97a1` with runtime commit `aef741c8` (measured as `2d930cc6`, with the identical Git tree `527a12a748a183ecdbb08990976f1ee2b226c85e`). The benchmark-only site filter was added separately. Four timing binaries rotate before/after and portable/SIMD; separate instrumented binaries count allocations. Core and JSON target 150 ms per case/build; site uses the 75 ms default. RSS is each fresh process's high-water memory, including initialization.

- `darwin`: Apple M4, macOS 26.6.2, Rust 1.98.1.
- `shannon`: Intel Core Ultra 9 285H, Linux 7.1.6, Rust 1.98.1, pinned to performance core 2 for timing and profiling.
- Hosts were checked for other gates and reserved with gate markers. A monitor aborts measurement if another gate starts. Builds used offline Cargo and four jobs.

There are 152 core, 38 JSON and 392 runnable site cases, each measured in both builds: 1,164 comparisons per architecture. All 2,328 comparisons have identical steps, tracked peaks, retained bytes and allocation counts. X86 allocates exactly eight additional untracked bytes per call for its ledger; ARM allocated bytes are identical. No RSS comparison exceeds +3%.

| Architecture | Core time geomean | JSON time geomean | Site time geomean | Time results above +3% |
| --- | ---: | ---: | ---: | --- |
| M4 | -0.08% | -0.18% | +0.18% | Eight original outliers; one remains in the focused control below |
| x86 | -3.61% | -4.81% | -5.42% | None |

Representative SIMD metered calls follow. Entries are before → after; a single allocation or byte count means both revisions are identical. Complete results for every case/build, including steps, allocation volume and RSS, are in each host's `all-comparisons.csv`.

### darwin

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| numeric_loop | 36.966 → 36.973 | +0.02% | 9 | 1,512 | 1,464 | 0 | 5.42 → 5.45 |
| loop_branches | 75.458 → 76.191 | +0.97% | 9 | 1,512 | 1,464 | 0 | 5.47 → 5.45 |
| array_map_select | 168.987 → 168.731 | -0.15% | 32 | 68,072 | 46,856 | 0 | 6.02 |
| record_build | 326.062 → 325.454 | -0.19% | 2,030 | 244,520 | 228,284 | 0 | 6.25 → 6.23 |
| glue_orders | 194.275 → 193.750 | -0.27% | 1,751 | 141,364 | 151,416 | 4,192 | 6.59 → 6.64 |
| json_transform | 32.301 → 32.281 | -0.06% | 367 | 28,671 | 30,835 | 128 | 6.23 → 6.16 |
| json_object_512 | 70.399 → 69.512 | -1.26% | 1,573 | 104,168 | 87,558 | 76,984 | 6.34 → 6.33 |
| json_object_2048 | 283.587 → 285.129 | +0.54% | 6,187 | 408,440 | 342,534 | 307,384 | 7.06 → 7.19 |

### shannon

| Case | Time, µs | Change | Allocations | Allocated bytes | Tracked peak | Retained | RSS, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| numeric_loop | 72.237 → 71.778 | -0.64% | 9 | 1,512 → 1,520 | 1,464 | 0 | 13.44 → 13.48 |
| loop_branches | 135.448 → 135.208 | -0.18% | 9 | 1,512 → 1,520 | 1,464 | 0 | 13.47 → 13.47 |
| array_map_select | 332.231 → 332.265 | +0.01% | 32 | 68,072 → 68,080 | 46,856 | 0 | 13.48 → 13.47 |
| record_build | 627.305 → 607.685 | -3.13% | 2,030 | 244,520 → 244,528 | 228,284 | 0 | 13.48 |
| glue_orders | 319.982 → 312.794 | -2.25% | 1,751 | 141,364 → 141,372 | 151,416 | 4,192 | 13.36 → 13.48 |
| json_transform | 55.540 → 53.089 | -4.41% | 367 | 28,671 → 28,679 | 30,835 | 128 | 13.45 → 13.47 |
| json_object_512 | 122.529 → 113.261 | -7.56% | 1,573 | 104,168 → 104,176 | 87,558 | 76,984 | 13.45 → 13.48 |
| json_object_2048 | 490.662 → 454.934 | -7.28% | 6,187 | 408,440 → 408,448 | 342,534 | 307,384 | 13.48 → 13.37 |

## ARM controls and rejected experiment

An initial version used cumulative reservations on both architectures. Its M4 core mean was flat (-0.12%), but 2,048-field SIMD JSON objects regressed 3.30% metered / 3.56% unlimited and an unlimited branch loop regressed 4.52%. That ARM implementation was rejected. Its results and profiles remain under `after-*`; acceptance uses `final-*`.

The final ARM implementation retains the original ledger size and `ldadd` / `ldumax` operations. Eight original timing results exceeded +3%. A focused run used twelve balanced rounds of before/final/control binaries, both builds, a 500 ms target and the harness's 100,000-call cap. The control is the baseline plus one uncalled, retained 120-byte assembly symbol; it changes executable layout without changing runtime logic. Every pilot's result and counters match the original baseline.

| M4 case | Build | Original time, µs | Original change | Focused change | No-op control change |
| --- | --- | ---: | ---: | ---: | ---: |
| loop_branches/unlimited | simd | 68.623 → 72.394 | +5.50% | +3.13% | +2.41% |
| strip_65536/unlimited | portable | 2.019 → 2.087 | +3.40% | -0.81% | +0.54% |
| long_multiplication/metered | portable | 4.067 → 4.206 | +3.41% | -0.14% | -0.64% |
| bubble_sort/metered | portable | 15.546 → 16.068 | +3.36% | +0.17% | +0.21% |
| long_multiplication/unlimited | portable | 4.058 → 4.191 | +3.28% | +0.39% | +0.05% |
| square_free_integers/unlimited | portable | 13.158 → 13.579 | +3.20% | +0.34% | +0.35% |
| timestamps/metered | portable | 3.520 → 3.628 | +3.07% | -0.66% | -0.34% |
| binary_digits/unlimited | simd | 8.790 → 9.058 | +3.04% | +1.56% | +2.47% |

Seven outliers disappear in the focused run. `loop_branches/unlimited` remains +3.13% in SIMD, while the no-op control alone moves it +2.41%. This is the permitted code-placement exception: the VM's 3,922 instructions, `charge_slowly`'s 131 instructions and `checkpoint_slowly`'s 102 instructions match after address relocation; the 40-instruction reservation function is byte-for-byte identical at a new address. The loop source is unchanged. The disassembly, relocation comparison and no-op source/binaries are retained, rather than inferring immunity from the flat suite mean.

The first site pilot also exposed a pre-existing harness issue: it attempted to execute `jobs_and_events`, which is recorded as a V0201 static rejection and fails in both revisions. `site_benchmark_cases` now excludes the five static-rejection examples from runtime timing. They remain covered by golden validation. The failed pilot and baseline reproduction are preserved.

## Profiles and implementation choice

Samply captures cover array map/select, glue orders, record building, numeric loops, JSON transforms and 512-field JSON objects. Mac uses native recording with presymbolication. Linux uses user-only `perf record -e cycles:u -F 1000 --call-graph dwarf,16384`, followed by Samply import and symbolication, as in round 3. Linux analysis selects the core event markers rather than the inactive Atom stream.

The old x86 reservation sequence contains `lock xadd`, a `lock cmpxchg` peak loop and an `Arc` increment. Drops add the byte subtraction and `Arc` decrement. The final reservation removes the first two locked updates. Most reservations inline into byte construction, hash construction and buffer growth; limiting attribution to a standalone `reserve` symbol misses those costs.

The following are identified leaf-sample shares, combining source locations and recognizable counter/Arc instruction sequences, rather than elapsed-time speedups or all allocator work. Mac inline DWARF coverage varies between builds; its lower identified share after the change does not imply an ARM accounting optimization. `work_bytes` remains a leading M4 accounting hotspot (8.16% → 7.31% of the JSON-object samples). Before/final captures contain 5,040–7,469 samples on M4 and 9,488–12,881 on x86.

| Workload | ARM identified accounting | x86 identified accounting | x86 reservations |
| --- | ---: | ---: | ---: |
| array_map_select | 4.94% → 4.62% | 5.52% → 5.49% | 0.15% → 0.06% |
| glue_orders | 7.34% → 7.79% | 13.63% → 11.38% | 5.34% → 2.18% |
| record_build | 8.01% → 7.58% | 13.97% → 10.71% | 5.33% → 2.42% |
| numeric_loop | 3.64% → 3.79% | 18.15% → 18.51% | 0.19% → 0.16% |
| json_transform | 7.83% → 6.73% | 19.26% → 14.26% | 9.45% → 4.22% |
| json_object_512 | 18.30% → 12.59% | 29.17% → 23.95% | 14.35% → 6.50% |

Keeping `Arc` preserves cross-thread lifetime management, weak identity references and the existing charge representation. A custom ledger would require more lifetime machinery and could change tracked value headers. Builder batching would move quota/peak observation points and cross other agents' ownership boundaries. The measured x86 reservation cost supports this smaller change. Step charging is unchanged.

## Ordering and exactness

X86 stores cumulative reserved and released totals. Only the executing call reserves; any thread may release. Reservation uses relaxed atomic loads/stores, and releases retain `fetch_add`. The released-total load is the reservation's accounting point: an earlier release reduces live usage, while a later release overlaps that allocation. Safe ownership and existing synchronization serialize context handoffs between Tokio workers. The shared ledger retains its thread-safe representation without introducing unsafe code.

Lifetime totals wrap modulo the machine word; their difference remains exact because admission checks prevent live usage from overflowing. Every original quota check remains before its reservation. No reservation is combined, delayed or skipped. `reserve_available` still excludes unused credit from the peak and releases it at the same points. Failure cleanup and JSON's exclusive peak publisher use the same ledger. All peak publishers belong to the executing call, so x86 uses a conditional load/store; remote drops never publish peaks.

A charge still releases its bytes before dropping its `Arc`, including after the call ends or on another thread. Cancellation, latched exhaustion and import identity checks are unchanged. Seven focused tests cover partial remote releases and restored headroom; last-clone release after the call; reservation peaks and failure cleanup; cumulative wrap and live overflow; concurrent allocation/drop; host-ignored quota errors inside script rescue; and Tokio task handoffs followed by retained-result drops.

## Counters, verification and artifacts

No golden file or Counter log entry changed. Paired sweeps covered 233,851 engine/parser cases from the same checkout path: all 233,847 stable observations and counters are identical. Four pre-existing clock-varying cases retain their recorded varying outcomes; fractional timestamp formatting can change their unrecorded string-byte totals. Nothing was re-recorded.

The measured revision passed local checks: formatting, Clippy with all features and without defaults, 2,045 default workspace test executions, all goldens, portable/SIMD validation of 107,562 shared successful cases plus golden corpora, and 1,891 WASI tests plus CLI/filesystem witnesses. Thirteen focused budget tests also passed with all features, including Tokio. One local validation process received SIGTERM; the retry using the same final binaries passed. Both the interruption and successful retry are preserved.

After rebasing, the complete local verification sequence and a fresh paired golden audit are retained in `work/rebased/`; portable/SIMD validation is in `rebased-validate/`. The final distributed gate runs against the integrated branch including this report. Its log and exit status are retained in `work/gate-all.log` and `work/gate-all.exit`; the completion message records its result and exact branch head.

All paths below are relative to `.cache/accounting/`; host originals remain in each clone's `.cache/accounting/`.

- `{darwin,shannon}/final-{core,json,site}/`: environments, hashes, per-round timing, allocation and RSS data; `all-comparisons.csv` summarizes every pair.
- `{darwin,shannon}/{before,final}-profiles/` and `*-bins/`: Samply captures, symbols, derived sample counts and preserved executables. `after-*` retains the rejected first experiment.
- `darwin/final-focus/`, `darwin/control-bins/`, `work/budget-control.rs`, `work/arm-relocations.json` and `work/darwin-*-code.json`: focused controls and machine-code evidence.
- `work/{source_samples,mac_accounting,profile_stats,symbolicate,focus,control,tables}.py`: extraction and reproduction helpers. `work/*-observations.jsonl`, `counter-audit.json`, `verification.json` and individual logs retain local verification evidence.
