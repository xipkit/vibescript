# Memory audit after the CPU optimization batch

Baseline: [`10ebb92c9967d0bad8a4b008071c882bcae9b511`](https://github.com/xipkit/vibescript/commit/10ebb92c9967d0bad8a4b008071c882bcae9b511), including PRs #1295, #1296, and #1297. Measurements use Apple M4, Go 1.26.3, darwin/arm64. Retention findings also reproduce with Go 1.27.1 and `GOEXPERIMENT=simd`.

This branch contains diagnostic evidence. The production prototypes were reverted after measurement. They establish opportunities; implementation still needs normal correctness, accounting, review, and performance gates.

## Retained heap

Each probe forces GC before and after the measured work and keeps the intended result or execution alive. The controls change one ownership condition. Small differences of a few kilobytes include process bookkeeping; the retained megabytes reproduce across runs and toolchains.

| Finding | Reproduction | Retained heap | Control |
| --- | --- | ---: | ---: |
| `Regex.match` returns an attached substring | Keep 24 one-byte matches from separate 512 KiB subjects | 12,772,624 B | 4,416 B using `byteslice` |
| Regex cache keeps a pattern's larger backing | Cache 24 twelve-byte suffix patterns taken from separate 2 MiB strings | 50,558,720 B | 25,992 B when patterns are cloned first |
| Deleted hash order slots keep their keys | Keep 24 one-entry hashes after deleting their 2 MiB final keys | 50,343,984 B | 11–17 KiB after clearing/reinserting the live entry |
| Popped runtime stack slots keep receivers and errors | Discard a 16 MiB instance after a method, or leave a rescue of a 4 MiB message | 16,807,864 B / 4,193,480 B | 6,448 B / 576 B after overwriting the inactive slot |

The hash issue also reproduces inside a running Vibescript call: deleting 24 one-MiB keys leaves approximately 24 MiB live. Reconciliation after host edits to a hash's live map has the same uncleared-tail problem. `popReceiver` and `popRescuedError` shorten slices without clearing their obsolete entries. The earlier environment-stack fix (#1247) does not cover these slots.

The regex cache contains only 25 entries and an estimated 292 instructions in its probe. Its instruction budget bounds compiled programs but does not detach a borrowed pattern from its backing allocation. Both the cache key/entry and the compiled regexp's expression need owned pattern bytes.

Sources and output: [probes](memory_audit_test.go.txt), [regex and struct sizes](retention.txt), [hash deletion](hash-retention.txt), [in-script hash deletion](script-hash-retention.txt), [hash reconciliation](hash-reconciled-retention.txt), [receiver and rescue stacks](stack-retention.txt), and [Go 1.27 SIMD confirmation](retention-go127-simd.txt). The reconciliation-specific probe was run on Go 1.26.3; the other four findings have both toolchain results.

## Allocation opportunities

Two small snapshot-buffer prototypes were compared in six alternating 200 ms trials with one Go scheduler thread. Both revisions use identical benchmark fixtures. These are complete `Script.Call` measurements with quotas enabled.

| Workload | Baseline B/op | Prototype B/op | Allocations before → after |
| --- | ---: | ---: | ---: |
| Group 600 hash rows | 1,291,808 | 1,138,079 (-11.90%) | 5,574 → 4,973 |
| Partition 600 hash rows | 1,269,807 | 1,116,208 (-12.10%) | 5,549 → 4,949 |
| 80 JSON stringify calls | 53,561 | 38,201 (-28.68%) | 520 → 440 |

Outbound hash cloning materializes a `[]HashEntry` for every hash through `HashEntries()`. A local eight-entry buffer removes the small-hash snapshots without removing the containment copy. This accounts for about 150 KiB and 600 allocations per 600-row result. Large hashes, shared entry maps, cycles, key types, order, and reserved capacities still need their existing behavior.

JSON serialization allocates a 48-byte entry record for every member of every object before rendering. The common four-member fixture pays 192 bytes and one allocation per serialization. A local eight-entry buffer removes that heap allocation; an implementation could also evaluate direct iteration over recorded order, since serialization does not call user code. Sorted fallback order, nested recursion, escaping, cycle/depth errors, output charging, and scratch projections must remain correct.

The paired CPU results show grouping -2.43%, partition no significant difference, and stringify -2.57%. This limited matrix supports follow-up work, not a claim about every architecture or workload. See [results](snapshot-paired/benchstat.txt), [sample manifest](snapshot-paired/manifest.json), and the [diagnostic patch](snapshot-prototypes.patch).

A separate compiler prototype skips directive-collision map construction when the source has no top-level class/module statements. The 7,474-byte `massive.vibe` fixture contains 251 functions and no classes. Its allocation falls from 248,888 to 222,032 B/op (-10.79%); allocations fall from 2,221 to 2,208; time falls from 257.1 to 244.9 µs (-4.74%). Control-flow and typed fixtures have unchanged allocations and no significant timing change. Existing compile/visibility/collision tests pass. See [results](compile-paired/benchstat.txt), [tests](compile-paired/tests.txt), and the [patch](compile-prototype.patch).

Finally, a warmed trivial `Script.Call` allocates 3,472 B in eight allocations, including a 2,304-byte allocation for the 2,216-byte `Execution` struct. This is roughly two thirds of the call's allocated bytes. Its inline stacks alone occupy 768 bytes. Reducing this fixed cost is an investigation: no smaller-state implementation is validated here. Preserve escaping closures, re-entry, concurrency, quota calculations, and the CPU improvements before retaining a redesign. This is residual state footprint after #198 and #1253 removed earlier eager setup work.

The [baseline sweep](baseline-bench.txt) also covers parsing, capabilities, recursion, and collection pipelines. Allocation profiles retain both bytes and object counts: [short calls](profiles/short-alloc_space.txt), [grouping](profiles/group-alloc_space.txt), [capability arguments](profiles/capability-alloc_space.txt), [JSON](profiles/json-alloc_space.txt), and [compilation](profiles/compile-alloc_space.txt). Profiles include process/fixture setup, so their aggregate percentages differ from the per-call allocation ratios above. Raw `.pprof` files are alongside the summaries. The parser depth memo is a substantial measured compile cost; its linear-work and nesting-limit requirements remain important.

## Reproduction

Use a clean checkout of the baseline above. Copy `memory_audit_test.go.txt` from this report to `internal/runtime/memory_audit_test.go`, then run serially:

```sh
go test ./internal/runtime -run '^TestMemoryAudit' -count=2 -v
GOTOOLCHAIN=go1.27.1 GOEXPERIMENT=simd go test ./internal/runtime -run '^TestMemoryAudit' -count=2 -v
```

The probes report measurements; passing means the observations completed, not that the retention bugs are fixed. The running-script probes use the baseline repository's `returnedFrameHeapBytes` helper. Avoid parallel heap tests in the same process.

For allocation reproduction, use the baseline benchmarks named in the result files with `-run '^$' -benchmem -cpu=1 -benchtime=200ms -count=6`. Apply each supplied diagnostic patch only in an isolated checkout to measure its corresponding prototype. Keep production changes out of the retention baseline. Prototype acceptance must include full accounting oracles, exact limit/error behavior, ownership tests, and CPU comparisons before implementation is delivered.
