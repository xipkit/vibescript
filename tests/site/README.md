# Website example corpus

These 203 files are unchanged copies of `internal/catalog/content` from the local `vibescript.mauriciogomes.com` repository at `5ca06f3b643e56f6b14caeec6cd4671268c55dbb`. Original metadata, source links and attribution remain in each example. [sources.json](sources.json) records SHA-256 hashes; the comparison and audit tools check every file before execution.

All 203 programs match Go Vibescript v0.70.0 in Go/Rust portable and SIMD builds, up from 74 in the initial audit. [cases.json](cases.json) preserves their Go results for native Rust regression tests and the shared comparison harness, and the [conformance goldens](../golden/README.md) record the Rust observations. Portable and SIMD Rust accounting also agrees with identical inputs and entropy. These are differential tests against Go, not independently derived mathematical proofs of the programs. Passing the corpus does not complete the [language port](../../docs/language-port.md).

[harness.json](harness.json) registers deterministic SMS/email preview capabilities for two examples and selects a typed result format for thirteen programs returning money or duration values. The adapters reproduce the pinned website's `internal/notifications/sms.go` and `email.go` preview results; they send no messages. The unchanged examples call `sms.send` and `email.send` through explicit per-call grants. The [Rust SMS example](../../examples/sms.rs) demonstrates the same capability API. The passing capability-iteration example returns metadata from `run`; it does not exercise host-driven blocks. Host block invocation and native async callbacks remain unfinished.

Typed results use a host-side `['typed-v1', node]` envelope encoded as JSON. Each node includes a value-kind tag, preserving distinctions such as a money value versus an ordinary array containing the word `money`. Integers, money cents and duration seconds use decimal strings; floats use sixteen hexadecimal digits containing their exact IEEE-754 bits; strings, symbols and normalized hash keys use hexadecimal bytes. Array elements and hash/object entries are recursively tagged, with hash iteration order preserved. Unsupported value kinds and nesting beyond 256 are rejected. This format runs outside script execution and does not change Vibescript's JSON behavior or resource limits. [Independent encoder fixtures](../encoding-cases.json) cover numeric boundaries, raw bytes, nested values, ordering and tag collisions; native Go/Rust tests also cover NaN payloads, bounded traversal and executable-value rejection.

The unchanged money and duration programs retain separate native checks through typed host values in [money.rs](../money.rs) and [duration.rs](../duration.rs). [time_anchors.rs](../time_anchors.rs) verifies duration/time helpers that the corresponding site's `run` does not invoke.

[initial-audit.json](initial-audit.json) preserves the initial errors; [current-audit.json](current-audit.json) records the latest full audit. After building comparison binaries with `scripts/compare.py --with-go`, rerun all 203 examples against Go with:

```sh
python3 scripts/audit-site.py --out .cache/site-audit
```

The audit fails on mismatched outputs or regressions in supported examples. Unsupported cases stay visible in its report. The shared comparison suite verifies all four builds, including equal Rust accounting.
