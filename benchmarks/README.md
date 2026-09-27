# Benchmarks

`scripts/compare.py` builds the Rust implementation twice, with its portable scanners and with explicit SIMD, checks both builds against the golden corpora and the fixture expectations, and then times the shared workloads in `scripts/fixtures.py`:

```sh
python3 scripts/compare.py --validate-only           # validation only
python3 scripts/compare.py --rounds 8                # validation and timing
python3 scripts/compare.py --suite site --rounds 8   # every site program
python3 scripts/compare.py --suite json --rounds 8   # API JSON and record transformations
python3 scripts/compare.py --baseline DIR --rounds 12
```

Every build is an offline release build with thin LTO and one codegen unit. Each workload runs with step and memory limits enabled (`/metered`) and unset (`/unlimited`); both modes still record accounting counters. Pilot runs choose one iteration count per case, shared by every build, so that the slowest build takes about `--target-ms`. The rounds rotate the build order so that each build takes every position equally often; the round count must therefore be a multiple of the number of builds. Compilation and output encoding are outside the timer; argument import and execution are inside, and the final timed output is checked against the validated one. Separate binaries built with the `allocation-stats` feature count allocation volume per call. A fresh process per case measures peak resident set size through Python's child-resource accounting on macOS and Linux. RSS includes the harness, compilation, arguments, output encoding and repeated calls; it is not the call's tracked memory counter.

The JSON suite in `scripts/json_fixtures.py` covers roughly 1 KB, 16 KB, 256 KB and 1 MB API responses, typed packets and arrays of records, filtering/projecting records into JSON requests, and medium/large nested output. Its independently computed expectations participate in validation without adding cases to the recorded golden corpora. Summaries include time, allocation count and volume, logical steps, peak and retained tracked bytes, and per-case process RSS.

`--baseline DIR` adds the `rust-portable` and `rust-simd` timing and allocation binaries of an earlier revision, preserved in `DIR` with a `revision` file holding its full commit hash, for before-and-after comparisons. A run writes its environment, raw samples, allocations and `summary.json` to `benchmarks/results/<timestamp>`, or to `--out`.

## Service footprint

`scripts/footprint.py` measures a fresh embedding process at startup, after
`Engine::new`, after four typed host functions and four capability declarations,
after compiling 1, 10 and 100 site programs, after 1,000 calls, and after dropping
the scripts:

```sh
CARGO_BUILD_JOBS=4 python3 scripts/footprint.py --rounds 8 --check --out .cache/mgomes-footprint/service
```

Pass `--baseline /path/to/previous/output` to alternate preserved baseline and
current binaries in fresh processes. The tool verifies baseline binary hashes,
the full compiler identity (including distribution) and `RUSTFLAGS`, then writes
the new baseline samples and summary under `baseline/` in the output. Paired runs
require an even round count. Their timing summary is the median of adjacent
two-round means, so both binaries run first and second within each block. This
avoids a pooled median falling between distinct cold-start modes. Raw timings
and block samples are retained. `scripts/footprint.py --summarize /path/to/output`
recomputes summaries from saved samples without building or launching binaries.

The fixture is the 100 largest self-contained site programs with a no-argument
`run`, totaling 159,089 source bytes. Sources live in read-only binary pages;
fixture loading and report serialization do not allocate inside the measured
phases. Calls rotate through all 100 scripts and drop each result. The startup
snapshot includes the Rust process runtime and measurement setup. Registration
also installs discard writers and deterministic entropy.

Separate release binaries measure timing/RSS and allocator requests. The report
includes cumulative allocation counts and bytes, currently live and peak live
requested Rust heap bytes, current process RSS, cold engine/first-compile time,
later compilation/call times, and the stripped release `vibes` binary's size.
Reallocations count as requests for their new size; live heap subtracts their old
size. Allocator bookkeeping, native allocations and thread stacks are outside
these heap counters but may contribute to RSS. RSS is sampled through Mach on
macOS and `/proc/self/statm` on Linux; unsupported platforms report null.
Raw rounds, build logs, revision/toolchain details and binary hashes accompany
the median summary. RSS after dropping scripts exposes allocator residency
separately from live script storage.

`--check` bounds engine construction at 16 allocations / 4 KiB, first compilation
at 16,000 allocations, and average incremental retained script storage at 28 KiB.
The workspace's isolated
`tests/footprint.rs` test enforces the same limits and checks that a 4 MiB call
followed by 1,000 small calls does not retain its temporary buffers. These are
generous heap guards; timing and RSS are reported without brittle CI thresholds.

`scripts/footprint_maps.py /path/to/footprint .cache/mgomes-footprint/maps` pauses the
uninstrumented example after 100 compilations and 1,000 calls, records native
allocator statistics, and captures `vmmap` on macOS or `/proc/<pid>/smaps` on
Linux. `--trim` adds snapshots after native allocator pressure relief; it is a
diagnostic option and does not affect normal service measurements. Raw mappings,
per-region data, executable hashes and category totals accompany each snapshot.
Use `--summarize .cache/mgomes-footprint/maps` to regenerate derived category totals from
saved mappings.

The [bootstrap service footprint report](footprint.md) records arm64 and x86_64
measurements, implementation tradeoffs, and the rejected trials.

## History

Until September 2026 these benchmarks also built Go Vibescript v0.70.0, then the compatibility reference, with and without Go's SIMD experiment. Three reports compared the builds on an Apple M4 while the Rust implementation still covered a subset of the language: the initial comparison, a first optimization pass (copy-on-write arrays, shared host strings, faster Unicode and JSON scans), and indexed hashes with the website corpus. The Rust core ran the integer loop about nine times faster than Go and parsed a 2,048-key JSON object in 0.34 ms against Go's 0.50 ms, while counting Unicode characters stayed about 1.5 times slower. The reports, their raw results and the Go harness remain in the repository history (`git log -- benchmarks/results`).

The [JSON SIMD report](json-simd.md) records the initial results. Its [follow-up](json-simd-followup.md) covers regression fixes, complete paired arm64/x86_64 measurements, accounting changes and CPU profiles.

The [typed-VM rebase report](json-simd-rebase.md) compares the integrated JSON implementation with upstream `6bad12f`.
