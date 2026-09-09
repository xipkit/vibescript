# Final performance fix integration validation

Worktree: `/tmp/vibescript-perf-integration`
Branch: `mgomes/perf-integration-check`
Final aggregate: `e8452179fb1005a7bdb5721b4a07eb1ae6bc802c`
Verified source tree: `b0fa83617c44258fbfea192bddc873306998dff4`

The aggregate includes current origin/master `18095060` (seven delivered performance fixes and unrelated PRs1262/1265), plus final scan `b5c70248`. No files from either unrelated PR were modified.

All final checks passed on this exact aggregate:

- `go test ./... -count=1`; runtime 54.766s.
- Full runtime suite with `VIBES_ESTIMATOR_VERIFY=1 VIBES_ENV_RECYCLE_VERIFY=1`; runtime 49.255s.
- Focused checker tests count3 covering type-arm summaries, container classification, scalar locals/unions, declaration roots/context keys, and independent declaration scaling.
- The same focused checker tests under the race detector; runtime 1.806s.
- `go vet ./...`.
- `golangci-lint run --timeout=10m`: 0 issues.
- Formatting and `git diff --check`: clean.
- Unchanged `./scripts/bench_smoke_check.sh`: every original threshold passed. `BenchmarkCallShortScript` measured 1004 ns/op, 3472 B/op and 8 allocs/op against limits of 5000 ns, 3500 B and 12 allocs. This gate ran with no other local CPU-heavy jobs.

`master1265-checks.json` records the exact commands, exit codes, head and timings.

Earlier expanded race coverage for memo mutation, wrapper journals, overlapping calls, lazy declarations, snapshots and scan passed on `6d3a2f7`; the only subsequent production changes were PR1265's checker cache, covered by the final checker race suite above. Scan fuzzing was not repeated.

The public-API 64 KiB capture admission probe preserved all eight baseline-admitted live, erased and mixed-capture cases and admitted three additional larger cases. `aggregate-parity-comparison.json` records those sets. Runtime scan code has not changed since that validation except removal of an ineffectual assignment, which received focused scan/race coverage.

Independent reviews found no cross-feature issues. PR1265's checker-local type summary cleanly replaces PR1262's container cache, preserves nullable/invalid/depth-limited union rules, and keeps named resolution tied to the current root. Scalar alias filtering and accessor caching retain their existing semantics.

Earlier integration conflicts were limited to preserving versioned lazy materialization through Env.getSkipping and retaining removal of the unused cloneFunctionsForCall helper across rebased ancestry. Subsequent master merges introduced no conflicts.



Master `d61e439ff89792a0210d0a56cd8af6668b337b92` was fetched after PR1260 merged. Its source tree is exactly `b0fa83617c44258fbfea192bddc873306998dff4`, matching the tested aggregate; `git diff --exit-code e8452179 origin/master` passed.
