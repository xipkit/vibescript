# Benchmarks

`scripts/compare.py` builds the Rust implementation twice, with its portable scanners and with explicit SIMD, checks both builds against the golden corpora and the fixture expectations, and then times the shared workloads in `scripts/fixtures.py`:

```sh
python3 scripts/compare.py --validate-only           # validation only
python3 scripts/compare.py --rounds 8                # validation and timing
python3 scripts/compare.py --suite site --rounds 8   # every site program
python3 scripts/compare.py --baseline DIR --rounds 12
```

Every build is a release build with thin LTO and one codegen unit. Each workload runs with accounting enabled (`/metered`) and disabled (`/unlimited`). Pilot runs choose one iteration count per case, shared by every build, so that the slowest build takes about `--target-ms`. The rounds rotate the build order so that each build takes every position equally often; the round count must therefore be a multiple of the number of builds. Compilation and output encoding are outside the timer; argument import and execution are inside, and the final timed output is checked against the validated one. Separate binaries built with the `allocation-stats` feature count allocation volume per call, and `/usr/bin/time -l` records each build's peak resident set size.

`--baseline DIR` adds the `rust-portable` and `rust-simd` timing and allocation binaries of an earlier revision, preserved in `DIR` with a `revision` file holding its full commit hash, for before-and-after comparisons. A run writes its environment, raw samples, allocations and `summary.json` to `benchmarks/results/<timestamp>`, or to `--out`.

## History

Until September 2026 these benchmarks also built Go Vibescript v0.70.0, then the compatibility reference, with and without Go's SIMD experiment. Three reports compared the builds on an Apple M4 while the Rust implementation still covered a subset of the language: the initial comparison, a first optimization pass (copy-on-write arrays, shared host strings, faster Unicode and JSON scans), and indexed hashes with the website corpus. The Rust core ran the integer loop about nine times faster than Go and parsed a 2,048-key JSON object in 0.34 ms against Go's 0.50 ms, while counting Unicode characters stayed about 1.5 times slower. The reports, their raw results and the Go harness remain in the repository history (`git log -- benchmarks/results`).
