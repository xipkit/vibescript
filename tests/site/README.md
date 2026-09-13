# Website example corpus

These 203 files are unchanged copies of `internal/catalog/content` from the local `vibescript.mauriciogomes.com` repository at `5ca06f3b643e56f6b14caeec6cd4671268c55dbb`. Original metadata, source links, and attribution remain in each example. [sources.json](sources.json) records SHA-256 hashes; the comparison and audit tools check every file before execution.

The initial audit found 74 matching examples. Range, slicing, collection, text, control-flow, destructuring, nested writes, hash fields, mutators, richer function binding, synchronous blocks, builtin iteration, ordering, recursive hash transforms, interpolation and arbitrary-precision integers bring the current total to 156 examples whose `run` results match Go Vibescript v0.70.0. [cases.json](cases.json) preserves those Go results for Rust regression tests and the shared comparison harness. These are differential tests against Go, not independently derived mathematical proofs of the programs.

The builtin-iteration audit added nine examples covering block transforms, yield patterns, numeric and collection iteration, matrix reports, string squeezing and run-length encoding. The mutating-block audit retained those 144 matches; sorting added anagrams, best shuffle, natural sorting and priority queue. Hash transforms added the unchanged upstream transformations example, bringing the total to 149. Interpolation added Brazilian numbers, range extraction and taxicab numbers for a total of 152. Arbitrary-precision integers add factorial, Egyptian fractions, Lychrel numbers and the big-integer showcase for a total of 156. Numeric rounding, division and predicates retain those 156 matches; the rounding showcase also needs typed parameters. The previously passing capability-iteration example returns metadata from `run`; that match does not exercise a real host capability. Another 32 examples complete in Go but need Rust features outside the current subset, including remaining block methods, typed parameters, `Math` and collection methods. A first error does not enumerate every missing feature in a program.

The remaining 15 are harness gaps: 13 return money or duration values that the harness's JSON encoder cannot represent, and two require SMS/email host capabilities that are not registered in the comparison harness. These are not reported as Go interpreter failures. The Rust [SMS preview](../../examples/sms.rs) separately demonstrates synchronous host registration using the currently supported `sms_send` syntax. Namespaced `sms.send` and native async host calls still need implementation.

[initial-audit.json](initial-audit.json) preserves the initial errors; [current-audit.json](current-audit.json) records the latest full audit. After building comparison binaries with `scripts/compare.py`, rerun all 203 examples with:

```sh
python3 scripts/audit-site.py --out .cache/site-audit
```

The audit fails on mismatched outputs or regressions in the supported examples. Unsupported cases stay visible in its report rather than being silently skipped. It creates no real external capabilities and sends no messages.
