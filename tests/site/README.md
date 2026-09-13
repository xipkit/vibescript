# Website example corpus

These 203 files are unchanged copies of `internal/catalog/content` from the local `vibescript.mauriciogomes.com` repository at `5ca06f3b643e56f6b14caeec6cd4671268c55dbb`. Original metadata, source links, and attribution remain in each example. [sources.json](sources.json) records SHA-256 hashes; the comparison and audit tools check every file before execution.

The initial audit found 74 matching examples. Range, slicing, collection, and text support bring the current total to 120 examples whose `run` results match Go Vibescript v0.70.0. [cases.json](cases.json) preserves those Go results for Rust regression tests and the shared comparison harness. These are differential tests against Go, not independently derived mathematical proofs of the programs.

Another 68 examples complete in Go but need Rust features outside the current subset, including blocks, interpolation, typed parameters, bignums, collection methods, and nested mutation. A first error does not enumerate every missing feature in a program.

The remaining 15 are harness gaps: 13 return money or duration values that the harness's JSON encoder cannot represent, and two require SMS/email host capabilities that are not registered in the comparison harness. These are not reported as Go interpreter failures. The Rust [SMS preview](../../examples/sms.rs) separately demonstrates synchronous host registration using the currently supported `sms_send` syntax. Namespaced `sms.send`, interpolation, and native async host calls still need implementation.

[initial-audit.json](initial-audit.json) preserves the initial errors; [current-audit.json](current-audit.json) records the latest full audit. After building comparison binaries with `scripts/compare.py`, rerun all 203 examples with:

```sh
python3 scripts/audit-site.py --out .cache/site-audit
```

The audit fails on mismatched outputs or regressions in the supported examples. Unsupported cases stay visible in its report rather than being silently skipped. It creates no real external capabilities and sends no messages.
