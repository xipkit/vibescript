# JSON regression fixes and CPU profiles

This report supersedes the tradeoffs in the [initial report](json-simd.md). The comparison baseline is the runtime at `52c9d8e`, preserved in the `bfe501d` benchmark binaries. Both architectures use eight rotating rounds, both accounting modes, both scanner builds, separately instrumented allocation builds, and fresh-process RSS observations. Darwin is an Apple M4; shannon is an Intel Core Ultra 9 285H. Vinci is used only for the final gate.

## Changes

Runtime revision: `d23014d` (later commits contain only this report and evidence).

The parser chooses ordinary/indexed and typed/untyped loops once per document. Flat objects avoid structural-index overhead, and short ASCII strings finish directly. Object-root strategy selection reads at most the first 512 bytes; this avoids scanning an entire long-string payload just to decide whether indexing might help. Arrays first encountered beyond that prefix use the ordinary parser. This affects strategy and storage sharing, not values, diagnostics or logical work.

Escaped input handles empty and one-byte clean runs directly. The writer uses an ASCII escape table and bypasses the span scanner for one-byte runs. Long clean runs retain vector scanning or eight-byte SWAR in portable builds. A full 16-byte clean vector returns without scalar peeling, and NEON ASCII classification checks the vector maximum directly. Shared ASCII scans combine four vectors before reduction. Exact-chunk iteration keeps bounds explicit in both prefix scanning and case conversion; assembly exposed a retained bounds check per vector in an intermediate prefix implementation. These changes remove the escaped-string regressions without changing growth order or pending-charge settlement.

The bounded key cache now retains two colliding keys in each of 32 sets, still using 64 ordinary Vibescript strings. API records repeatedly use colliding field names; retaining both avoids allocating those keys again for every record. The cache stores 32 explicit pairs and borrows the selected hit directly; initialization uses a constant array of nil values. The latter also removes generated initialization code that had shifted a frequently executed, otherwise unchanged destructor across a code-page boundary in the M4 portable binary. The saved control profile and disassembly document that investigation. No VM or value-representation source is changed.

Typed parsing retains the streaming proofs described in the initial report: validate elements as they finish, defer proof charges to the established normalization point, and use the existing normalizer for unsupported shapes or failed proofs. Syntax-error precedence, duplicate keys, nominal conversion, depth and quota behavior remain covered by differential tests.

## Measurements

The final eight-round results below retain every case; the largest positive median change in each group is:

| Architecture | Portable | SIMD |
| --- | ---: | ---: |
| arm64 | +3.23% | +1.82% |
| x86_64 | +1.75% | +2.03% |

447 of 448 comparisons are at or below +3%. The exception is M4 portable `json_object_2048/unlimited`: 282.51 → 291.62 µs (+3.23%). Its metered counterpart is +2.97%. This is at the edge of the requested approximate noise band; it is not a strict +3.00% pass. The raw rounds retain the result.

[All 448 comparisons](results/json-simd-followup/all-cases.csv) include every core control and JSON workload, both accounting modes, allocations, allocation volume, steps, peak/retained tracked bytes and RSS. Raw rounds, allocation records, RSS observations, input fixtures and environment metadata are retained alongside the summaries. RSS includes the harness, compilation and output encoding; it is not call-only memory and small differences are noisy.

Metered regression cases, original baseline → final (µs/call):

| Host/build | Flat object, 512 | Duplicate keys, 512 | Parse escapes, 4 KiB | Stringify escapes, 4 KiB |
| --- | ---: | ---: | ---: | ---: |
| arm64 portable | 69.58 → 70.98 (+2.0%) | 119.62 → 119.63 (+0.0%) | 12.06 → 9.52 (-21.1%) | 9.08 → 7.55 (-16.9%) |
| arm64 simd | 70.47 → 70.13 (-0.5%) | 119.64 → 118.82 (-0.7%) | 12.73 → 9.83 (-22.8%) | 9.92 → 7.55 (-23.9%) |
| x86_64 portable | 98.86 → 99.37 (+0.5%) | 165.02 → 165.70 (+0.4%) | 11.02 → 10.62 (-3.6%) | 10.78 → 8.01 (-25.7%) |
| x86_64 simd | 98.22 → 98.81 (+0.6%) | 164.30 → 164.98 (+0.4%) | 11.59 → 9.20 (-20.6%) | 10.88 → 8.27 (-24.0%) |

The 1 MiB cases below are metered. Time is ms; memory/RSS is MiB; allocations are counts. Both modes have the same allocation and tracked-memory counts.

| Host/build | Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| arm64 portable | parse | 8.397 → 6.670 | 301,406 → 147,542 | 15.27 → 10.36 | 14.26 → 9.35 | 66.86 → 45.98 |
| arm64 portable | parse_as | 11.735 → 9.479 | 301,407 → 147,543 | 15.27 → 10.36 | 14.26 → 9.35 | 67.12 → 46.34 |
| arm64 simd | parse | 8.474 → 6.703 | 301,406 → 147,542 | 15.27 → 10.36 | 14.26 → 9.35 | 66.84 → 47.91 |
| arm64 simd | parse_as | 11.791 → 9.558 | 301,407 → 147,543 | 15.27 → 10.36 | 14.26 → 9.35 | 67.42 → 48.30 |
| x86_64 portable | parse | 13.485 → 9.887 | 301,406 → 147,542 | 15.27 → 10.36 | 14.26 → 9.35 | 64.75 → 43.58 |
| x86_64 portable | parse_as | 18.160 → 13.005 | 301,407 → 147,543 | 15.27 → 10.36 | 14.26 → 9.35 | 64.59 → 43.76 |
| x86_64 simd | parse | 13.400 → 9.395 | 301,406 → 147,542 | 15.27 → 10.36 | 14.26 → 9.35 | 64.36 → 43.68 |
| x86_64 simd | parse_as | 18.031 → 12.481 | 301,407 → 147,543 | 15.27 → 10.36 | 14.26 → 9.35 | 64.45 → 43.68 |

The initial regressions remain in the initial report; intermediate measurements are retained in the `experiments` evidence archives. The final run is a fresh, complete run; individual favorable rounds were not selected. Hosts were checked for gates before measuring. Shannon’s desktop indexer was paused during final measurements and restored afterward.

## CPU profiles

The [profile evidence and method](results/json-simd-followup/profiles/README.md) retain raw Samply profiles, precise symbols including inline frames, and exclusive category summaries. Each profile runs 2,000 metered calls against the 1,048,536-byte fixture at nominal 1 kHz. Profiles cover the whole process, including argument import and returned-value release; the benchmark timer covers a narrower scope. Percentages are sampled CPU time, not wall time or inclusive call-tree percentages.

| CPU category | M4 parse | M4 parse_as | x86 parse | x86 parse_as |
| --- | ---: | ---: | ---: | ---: |
| Structural scanning | 9.1% | 7.1% | 5.0% | 3.9% |
| UTF-8 validation | 1.5% | 1.1% | 0.7% | 0.5% |
| Number decoding | 1.5% | 1.1% | 1.5% | 1.3% |
| String decoding/key lookup | 10.4% | 7.6% | 11.2% | 8.6% |
| Value construction/release | 13.8% | 14.2% | 28.3% | 29.6% |
| Allocation/capacity | 36.5% | 27.1% | 14.1% | 11.3% |
| Step/memory accounting | 14.0% | 14.2% | 29.5% | 24.1% |
| Type validation | 0.0% | 11.5% | 0.0% | 9.5% |
| Parser control | 11.2% | 10.4% | 9.1% | 7.9% |
| Other | 2.0% | 5.7% | 0.5% | 3.3% |

Top sampled leaf functions (percent of all CPU samples):

- M4 parse: `<vibescript::budget::CallContext>::charge` (8.3%), `<vibescript::json::parser::Parser>::read_string::<true>` (5.3%), `<vibescript::json::parser::Parser>::space_with::<true>` (3.1%), `core::ptr::drop_glue::<vibescript::value::Kind>` (2.9%).
- M4 parse_as: `<vibescript::budget::CallContext>::charge` (9.5%), `_platform_memcmp` (5.4%), `<vibescript::json::parser::Parser>::read_string::<true>` (3.9%), `core::ptr::drop_glue::<vibescript::value::Kind>` (2.8%).
- x86 parse: `core::sync::atomic::atomic_sub::<usize, usize>` (5.5%), `<vibescript::budget::CallContext>::reserve` (5.1%), `<vibescript::json::parser::Parser>::read_string::<true>` (4.9%), `core::sync::atomic::atomic_umax::<usize>` (4.8%).
- x86 parse_as: `<alloc::sync::Arc<vibescript::value::Bytes> as core::ops::drop::Drop>::drop` (4.1%), `core::sync::atomic::atomic_sub::<usize, usize>` (4.0%), `vibescript::types::visit` (3.7%), `<vibescript::budget::CallContext>::reserve` (3.7%).

Private macOS allocator symbols are attributed by image; unnamed Linux libc leaves use the first identifiable caller. The saved per-category function lists include those offsets. Category attribution uses the innermost identifiable inline frame, and each sample belongs to one category; percentages are approximate and rounded.

Structural scanning is no longer the dominant cost. Allocation and memory/step accounting are larger, especially the memory ledger's atomic reference-count and peak updates on x86. Number conversion is a small fraction of total time. The two-way key cache addresses allocation directly while retaining the existing representation and charge ordering.

## Accounting and verification

Formatting and both all-target Clippy configurations passed with warnings denied. All 1,996 native workspace tests and all eight golden corpora passed. No golden was re-recorded. Portable/SIMD validation passed all 107,418 shared cases with exact step, peak-memory and retained-memory equality. WASI passed 1,845 tests, both Clippy configurations, and the CLI/filesystem witnesses under Wasmtime. The prescribed three-host gate runs on the final documentation commit; the delivery message records its exact head and result. The full per-case counter audit compares both builds with the original baseline as well as checking portable/SIMD equality: zero step changes across all 107,418 cases; 42 peak-memory reductions and 24 retained-memory reductions, with no increases. Every changed value is listed in the [arm64 audit](results/json-simd-followup/arm64-counter-changes.json) and [x86 audit](results/json-simd-followup/x86_64-counter-changes.json). Key sharing and smaller string temporaries explain the reductions; accounting remains based on actual tracked storage. [Local verification logs](results/json-simd-followup/verification-local.tgz) include the exact commands and totals.

## Next steps

The typed VM's compact-record builder could map field names to precomputed slots and fill one values buffer directly during parsing. It must preserve last-value-wins duplicates, observable field order, optional-field presence, open-shape extras, late diagnostic precedence and the existing normalization fallback. Its accounting must describe the actual new storage and preserve quota failure order. This requires the parallel branch's representation contract and is intentionally a proposal.

Batching memory-ledger or step updates could reduce accounting cost, but merely replacing atomic operations or moving charges is unsafe: reservations, releases, quota failures and periodic callbacks are observable. A bounded builder-owned reservation could amortize updates while settling before growth, failure or reentry. Establish the interruption and error-order contract, then differential-test every quota boundary before changing it. Existing string-unescape batching is retained; this follow-up makes no general accounting change.

Exact container sizing needs similar care: a second traversal costs CPU and eager reservation can change failure order and peak memory. A compact-record builder offers a more direct next experiment than another general document pass.
