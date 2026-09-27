# JSON records and accounting

**This branch does not meet every acceptance condition.** Three current M4 JSON comparisons regress by 3.18–3.56%, wide shape tables can increase temporary tracked memory, and current x86/core measurements plus the final three-host gate are incomplete. Other host jobs block further runs.

The baseline is `f7d8079` on `mgomes/rust-language`. This work is confined to JSON, additive internal accounting helpers and the existing `records::Fields` hook. The VM, bytecode and value/hash representation are unchanged.

## Records

`JSON.parse_as` follows statically known array, tuple, hash and shape containers while parsing. Each reusable shape owns one lazy `Fields` table for the parse; names are imported at their original source materialization point and reused across array elements. The root record needs no sharing table because it is built once. A small cache of schema paths avoids repeated shape searches while visiting nested records. Required closed shapes reserve their exact pair count at the first insertion. Optional/open shapes retain ordinary growth because the number of supplied fields is not known. The result remains an ordinary hash, populated in source order, including the existing duplicate-key replacement behavior.

`Fields::of`, `key` and `len` remain compatible. The additive lazy-slot interface uses inline storage for up to eight names and tracked storage for larger shapes. JSON keeps sixteen schema tables inline, with tracked overflow. Tests cover source order, duplicate and escaped names, long names, shared names across shapes, open-shape extras, exact capacity, overflow, 8/9/16/17/65-field shapes and complete release.

The indexed parser also shares repeated unescaped scalar strings of at most 64 bytes in a bounded two-way cache. Numbered strings bypass this cache so unique identifiers do not churn it. The cache copies strings into ordinary owned values; it never retains slices of the input. Duplicate object replacement clears scalar entries before the next allocation so discarded subtrees do not remain live. Parse errors clear scalar entries before diagnostic allocation.

## Accounting order

Step allowances end before the next existing 16-step checkpoint and never exceed remaining quota. Local draws settle before every mutable context operation and on drop. A charge that crosses either boundary uses the original `CallContext::charge`, including its multi-step overshoot and latched error. Existing string decoding/encoding batches continue to use their settled context, avoiding an extra wrapper on each escape. No public charging API changes.

Indexed payloads use unescaped token copies that reserve only available memory headroom for the backing bytes and value header together. Credit acquisition cannot fail and does not publish a speculative peak. Each draw performs the original checkpoint; an insufficient draw releases unused credit and calls the original reservation at exactly that point, after the same steps. Actual live peaks are accumulated locally and published before return, error or another allocator. The two pieces share one retained charge, removing an atomic release and ledger reference count per string. Reservation scopes cannot call host code or expose speculative credit. Flat, non-indexed objects retain the existing copy path because short-lived reservations added overhead on M4.

The accounting oracle runs the same parser/writer with reservations disabled. It compares every step quota crossed with every byte quota for small valid and invalid documents, typed/untyped and indexed/portable paths. Comparisons include the value or error, parser position, syntax failure, partial writer output, steps, peak/retained memory and subsequent latched error. Separate tests cover empty strings, a token larger than the 4 KiB chunk, and cancellation/deadline boundaries. Intentional storage reductions from records/string sharing are independently audited against the baseline.

Additional probes check [singleton shapes](results/json-records/width-counters.json) and [arrays of wide shapes](results/json-records/array-width-counters.json). The sampled singleton widths (1, 2, 4, 8, 9, 16, 17, 32, 65 and 128) never increase peak or retained memory. Wide array field tables have a temporary cost when the original key cache already shares every name: a 16-field array adds 256 peak bytes, and singleton arrays of 32/128-field records add 512/2,048 bytes. Retained bytes and steps do not increase. This is a limitation of the current implementation: the existing golden and benchmark fixtures only decrease, but arbitrary wide shapes do not meet a universal no-increase guarantee. The extra table storage remains fully tracked.

## Measurements

The [tables](results/json-records/tables.md) include metered JSON workloads from 1 KiB through 1 MiB, with time, allocations, peak/retained tracked bytes and RSS. The [CSV](results/json-records/all-cases.csv) includes both accounting modes and builds; every row identifies its measured revision and whether it is current or historical. The [regression list](results/json-records/regressions.json) preserves every comparison above +3%.

Current M4 JSON measurements used eight rotating rounds at a 250 ms target, foreground priority zero, preserved `f7d8079` binaries, validation before timing, separate allocation-instrumented binaries and process RSS sampling. The measured snapshot is `532eddb7`; the delivered `src/` tree is identical (`cc2c271debeb9b9bacfc9ba2024270b089ea0b9a`). M4 SIMD 1 MiB parsing improves from 6.812 to 6.115 ms, and typed parsing from 7.921 to 7.406 ms. Allocations fall from 147,540 to 89,841 for parsing and from 147,541 to 89,842 for typed parsing. Typed peak/retained bytes fall from 10,861,631/9,808,357 to 7,219,735/6,166,461.

Three small current JSON comparisons fail the requested limit:

| M4 workload | Before | After | Change |
| --- | ---: | ---: | ---: |
| Portable shape 1 KiB, metered | 9.067 µs | 9.383 µs | +3.49% |
| SIMD shape 1 KiB, metered | 8.922 µs | 9.205 µs | +3.18% |
| Portable array 1 KiB, unlimited | 8.625 µs | 8.932 µs | +3.56% |

**The x86 and M4 core figures are historical, not current-head validation.** They measure `8bdc2c45`, with eight rounds at the script's original 75 ms target. All 284 x86 timing comparisons passed the +3% limit at that revision; fifteen M4 core comparisons did not. Later M4 core runs were stopped when superseded, so they are retained as incomplete experiments rather than promoted to complete results. Current x86 measurement was blocked by `sh /tmp/text-regex-host/gate.sh` on Shannon; a later attempt on Darwin also encountered `/tmp/footprint-followup-gate.sh`. Waiting launchers were removed before delivery and no other job's process or release marker was changed. No measurements ran on Vinci.

Rejected revisions, partial runs, source patches and diagnostic profiles are preserved in [experiments.tar.gz](results/json-records/experiments.tar.gz). A normal-priority rerun did not reproduce a proposed scheduling explanation for one regression; that explanation is rejected. Diagnostic profile symbolication did not resolve most Rust frames, so those profiles are not used for function-level percentage claims.

## Counter audit and verification

The paired golden audit compares 197,838 shared cases from the same checkout: 197,834 stable observations are identical, with four existing clock-dependent cases excluded. Fifteen counters intentionally decrease: two conformance, five language and eight replay entries. Every step count and observation is unchanged. Only these entries were re-recorded; the [Counter log](../tests/golden/README.md#counter-log) and [exact audit](results/json-records/counter-changes.json) describe each reduction.

On the identical delivered runtime source, formatting, Clippy with all features and without default features, all 2,009 workspace tests, all eight golden corpora and `scripts/check-wasi` passed. A clean `compare.py --validate-only` passed: all 107,448 shared cases match and portable/SIMD steps and memory counters are identical. Logs and source identity are in [verification.tar.gz](results/json-records/verification.tar.gz). Existing random/adversarial and serde differential tests passed, together with the exhaustive quota-boundary and record-sharing tests.

`mgomes/rust-language` remained at `f7d8079` when checked before delivery, so no rebase or upstream counter reconciliation was needed. The required final `gate-all.sh mgomes/json-records` was not run: other host gates remained active. This report does not claim a zero-failure three-host gate or completed performance acceptance.
