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

## History

Until September 2026 these benchmarks also built Go Vibescript v0.70.0, then the compatibility reference, with and without Go's SIMD experiment. Three reports compared the builds on an Apple M4 while the Rust implementation still covered a subset of the language: the initial comparison, a first optimization pass (copy-on-write arrays, shared host strings, faster Unicode and JSON scans), and indexed hashes with the website corpus. The Rust core ran the integer loop about nine times faster than Go and parsed a 2,048-key JSON object in 0.34 ms against Go's 0.50 ms, while counting Unicode characters stayed about 1.5 times slower. The reports, their raw results and the Go harness remain in the repository history (`git log -- benchmarks/results`).

The [JSON SIMD report](json-simd.md) records the initial results. Its [follow-up](json-simd-followup.md) covers regression fixes, complete paired arm64/x86_64 measurements, accounting changes and CPU profiles.
