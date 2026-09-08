# Performance audit, September 7, 2026

Target: upstream `6e9710c427cf434d4e178b7be7562d296026bb3c` (v0.60.0 plus current fixes).
Host: Apple M4, 16 GiB RAM, macOS arm64, Go 1.26.3. The original working branch was stale; all measurements use current upstream.

The audit covers compilation/checking, call setup, quota accounting, collections, strings/regex, JSON, capability boundaries, editor navigation and file watching. It changes no production code. Diagnostic probes are behind the `perfaudit` build tag and record baseline behavior; they are not assertions that the inefficient behavior should remain.

## Findings and priority

| Order | Finding | Measured impact |
|---|---|---|
| 1 | Popped call frames retain dead locals | Clearing inaccessible stack slots frees 64 MiB during a call configured with a 48 MiB quota |
| 2 | JSON tokens retain their full source document | 32 one-byte results retain 17.0 MB after GC; escaped-token control does not |
| 3 | `uniq` repeatedly walks the receiver under a memory quota | Scalar-key block: 28.4 / 101.3 / 435.2 ms for 200 / 400 / 800 rows; 800 rows without memory quota: 0.94 ms |
| 4 | Whole-script checking clones all declarations per function | 100 / 200 / 400 independent functions: 2.62 / 10.14 / 40.09 MB per check |
| 5 | Scalar local facts trigger pairwise alias scans | 400 integer assignments: 4.39 MB and 84,369 allocations; 87.7% of profile allocation in `mutableStaticContainers` |
| 6 | Block `String#scan` buffers every match before yielding | Immediate return on a 256 KiB input: 27.0 ms, 37.02 MB, 262,211 allocations |
| 7 | Unused declarations impose per-call setup | Entry point returning `1`: 3.4 KB alone versus 929.8 KB with 1,000 unused classes |
| 8 | A global mutation epoch invalidates unrelated quota caches | Stable 10,000-row graph: 0 nodes walked on a cache hit versus 60,005 nodes / 1.81 ms after an unrelated environment write |

All timing table values above use medians of three focused runs. Retained-heap examples use explicit GC and causal controls. MB denotes decimal bytes; MiB denotes powers of two. The global-epoch numbers are isolated estimator checks, not end-to-end throughput. The popped-frame issue lasts during a live call until slots are overwritten or the execution dies; it is not an engine leak across completed calls.

## Method

The broad baseline used the repository runner with `--count 1 --benchtime 100ms --cpu 1`, covering 106 runtime benchmark cases and 13 tooling/value/capability cases. Candidate findings were repeated three times and profiled individually. Benchmark subprocesses were serialized; source review happened in parallel. Setup is excluded from focused benchmark timers. Validation of returned values prevents meaningless timing of failed work.

`baseline.txt` and `tools-values-baseline.txt` are screening data. `focused-repeated.txt`, `uniq-repeated.txt` and `memory-invalidation.txt` contain publishable repeated measurements. Retention logs compare live heap after GC. Profile summaries are in `profiles/`; raw CPU/heap profiles remain in `/tmp/vibescript-perf-evidence-20260907` on the audit machine.

macOS profiles contain substantial scheduler/VM samples (`kevent`, `madvise`), so whole-profile CPU percentages should not be treated as portable application CPU attribution. Allocation stacks, deterministic node counts, scaling curves and controlled interventions provide the stronger attribution here. The accumulator-section `uniq` benchmark is a diagnostic intervention, not a validated optimization patch.

## Reproduce

Use this audit branch, whose only changes are tagged probes and evidence:

```sh
go test -tags perfaudit ./internal/runtime -run '^TestAudit' -count=3 -v
go test -tags perfaudit ./internal/runtime -run '^$' -bench '^BenchmarkAudit(UnusedDeclarations|CheckerScaling|StringScanEarlyReturn|CompositeUniqSection|MemoryUnrelatedMutation)$' -benchmem -benchtime=100ms -count=3 -cpu=1
go test -tags perfaudit ./internal/runtime -run '^$' -bench '^BenchmarkAuditCompositeUniq$/quota=.*/n=(200|400|800)/' -benchmem -benchtime=100ms -count=3 -cpu=1
```

The original logs predate adding the build tag; that packaging change does not alter the probes. To regenerate a focused profile, pass the same tag directly to `go test` with `-cpuprofile` and `-memprofile`, or set `GOFLAGS=-tags=perfaudit` when using `scripts/bench_profile.sh`.

## Other results and exclusions

- Array-of-hashes construction, array reads and hash reads scale roughly linearly over 250–2,000 elements. The existing broad growth fixes still hold.
- Deep nesting remains quadratic with a quota: depths 500 / 1,000 / 2,000 cost 3.17 / 11.92 / 48.24 ms versus 0.145 / 0.290 / 0.575 ms with metering disabled. This is already documented by closed #1124 and its benchmark; no duplicate issue was created.
- The closed #1223 self-call checker case remains linear in the focused control. New checker findings concern independent function roots and scalar-local alias work.
- Composite equality remains quadratic and allocation-heavy without quotas. Closed #447 describes that fallback; the new `uniq` issue concerns additional quota-accounting amplification, also present with scalar keys.
- Large capability calls are expensive (10,000-row arguments: 22.1 ms and 23.05 MB in the broad baseline), but boundary isolation explains multiple copies. No issue is filed for merely removing required isolation. A possible duplicate context-capability clone needs its own measurement before filing.
- Hash membership screening includes inbound isolation copies, so it does not isolate a new avoidable builtin cost. No issue filed.
- LSP cached diagnostics/navigation remain fast; broad tooling results are recorded without claiming new actionable defects.

## Validation

- All diagnostic retention probes passed three times with `go test -tags perfaudit ./internal/runtime -run '^TestAudit' -count=3 -v`.
- `go test ./...` passed on the audited production code.
- `go vet -tags perfaudit ./...` passed, including probe code.
- Broad runtime/tooling benchmarks and all focused repeated benchmarks completed successfully.

Logs: `retention-repeated.txt`, `tests.txt`, and `vet.txt` (empty successful output).
