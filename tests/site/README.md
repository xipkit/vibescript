# Website example corpus

These 203 files are unchanged copies of `internal/catalog/content` from the local `vibescript.mauriciogomes.com` repository at `5ca06f3b643e56f6b14caeec6cd4671268c55dbb`. Original metadata, source links and attribution remain in each example. [sources.json](sources.json) records SHA-256 hashes; the comparison and audit tools check every file before execution.

The current audit matches 188 programs against Go Vibescript v0.70.0, up from 74 in the initial audit. [cases.json](cases.json) preserves their Go results for native Rust regression tests and the shared comparison harness. These are differential tests against Go, not independently derived mathematical proofs of the programs.

All 188 examples that complete through the Go JSON harness now match Rust. Passing these programs does not by itself complete the [language port](../../docs/language-port.md).

The remaining 15 are harness gaps: 13 return money or duration values that the harness's JSON encoder cannot represent, and two require SMS/email host capabilities that are not registered. The passing capability-iteration example returns metadata from `run`; it does not exercise a real host capability. The Rust [SMS preview](../../examples/sms.rs) separately demonstrates synchronous host registration using `sms_send`. Namespaced `sms.send` and native async host calls still need implementation.

The unchanged money and duration examples also receive separate native checks through typed host values in [money.rs](../money.rs) and [duration.rs](../duration.rs). They remain JSON harness gaps. [time_anchors.rs](../time_anchors.rs) verifies duration/time helpers that the corresponding site's `run` does not invoke.

[initial-audit.json](initial-audit.json) preserves the initial errors; [current-audit.json](current-audit.json) records the latest full audit. After building comparison binaries with `scripts/compare.py`, rerun all 203 examples with:

```sh
python3 scripts/audit-site.py --out .cache/site-audit
```

The audit fails on mismatched outputs or regressions in supported examples. Unsupported cases stay visible in its report. It creates no real external capabilities and sends no messages.
