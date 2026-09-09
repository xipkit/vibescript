# Final performance fix integration validation

Worktree: `/tmp/vibescript-perf-integration`
Branch: `mgomes/perf-integration-check`
Final aggregate: `6d3a2f7de0392349e84661bf6cd0475626d30908`
Source tree: `c31b2be0911dc4a19b69bbdb5c7f487b52abb160`

The aggregate includes origin/master `bec63760` (seven delivered performance fixes and unrelated PR1262), plus final scan `b5c70248`. Declaration layout `ab4a6011`, checker `900a1a3b`, and epoch `349d7067` are included.

All requested checks passed:

- Full `go test ./... -count=1` on aggregate `d4f33cd6`.
- Full runtime suite with `VIBES_ESTIMATOR_VERIFY=1 VIBES_ENV_RECYCLE_VERIFY=1` on `d4f33cd6`; runtime 55.984s.
- Expanded focused race suite count3 on final `6d3a2f7de0392349e84661bf6cd0475626d30908` covering memo mutation, journal, overlapping calls, lazy declarations, snapshots and scan; runtime 17.555s, value 1.598s.
- `go vet ./...` on final tree.
- Focused scan suite on final tree; runtime 0.781s.
- `golangci-lint run --timeout=10m`: 0 issues.
- Formatting and `git diff --check`: clean.
- Unchanged `./scripts/bench_smoke_check.sh` on final tree: passed every original threshold. `BenchmarkCallShortScript` measured 998.6 ns/op, 3472 B/op and 8 allocs/op against limits of 5000 ns, 3500 B and 12 allocs. This gate ran after other CPU-heavy jobs finished.

The final tree differs from `d4f33cd6` only by removing the ineffectual `loc = nil` assignment in scan; the full suites were already running when that lint cleanup arrived. Final scan/race/vet/lint/smoke checks cover the exact resulting tree.

The public-API 64KiB capture admission probe preserves all eight baseline-admitted live, erased and mixed-capture cases, and admits three additional larger cases. `aggregate-parity-comparison.json` records the compared sets; no cases were dropped.

Integration review:

- Upstream PR1262 merged without conflicts and passed focused checker tests count3. Independent review found no cache-lifetime, type-mutability, depth-validity, scalar alias, or accessor-cache interaction issue.
- Declaration/epoch conflict in `Env.getSkipping` was resolved using helper delegation with versioned lazy materialization.
- Rebased declaration ancestry attempted to restore `cloneFunctionsForCall`; the aggregate preserved the checker removal. Subsequent origin/master merge was source-identical.
- Independent cross-feature review found no issues with lazy declaration roots, weak caches, epoch invalidation, or frame cleanup.

`aggregate-checks.json` records exact commands, exit codes, and tested revisions. No external writes or pushes were performed. The worktree is clean.
