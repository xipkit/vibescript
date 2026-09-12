# Indexed hashes and the website corpus

Measured on Apple M4 (ARM64), Rust 1.98.1, and Go 1.27.1. The previous Rust binaries are from `f86f4bcf123005008a3118662ae8b41fc598edf5`; the updated runtime and harness are from `6cd9190260ac6e4a2081d289b79c7bd945879bad`. Go uses Vibescript v0.70.0. Both Rust revisions were rerun together using the same inputs and iteration counts. All Rust timing and allocation binaries use **release mode**, thin LTO, and one codegen unit. Debug symbols remain enabled for profiling.

## Results

Parsing a 2,048-key JSON object is 49.1× faster: 16.616 ms becomes 0.338 ms, versus Go SIMD’s 0.502 ms. Building a 512-entry hash is 18.0× faster and allocates 145.4× fewer bytes. Updating an existing key 128 times is 20.6× faster and allocates 27.8× fewer bytes. These cases previously spent work scanning keys, copying complete entry arrays, and recalculating depth.

The index preserves insertion order and duplicate-key replacement. Runtime hash construction, JSON objects, lookups, equality, imports, and mutation use it. Hashes below 16 entries retain linear lookup. Unaliased writes reuse storage; snapshots copy entries when needed and share an immutable index until adding a key requires a separate index.

Times below are medians of 12 samples, with accounting enabled. Allocation bytes are cumulative requested allocation volume per call, not retained memory or RSS.

| Workload | Rust before µs | Rust after µs | Go SIMD µs | Rust speedup | Rust B/call before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| `hash_lookup` | 359.802 | 125.207 | 844.170 | 2.87× | 62,448 → 63,552 |
| `hash_lookup_8` | 22.145 | 22.805 | 112.271 | 0.97× | 8,688 → 8,696 |
| `hash_lookup_512` | 494.316 | 39.409 | 187.678 | 12.54× | 53,040 → 61,312 |
| `hash_lookup_2048` | 1952.657 | 115.566 | 403.864 | 16.90× | 188,208 → 221,056 |
| `json_object_8` | 1.226 | 1.227 | 4.427 | 1.00× | 1,912 → 1,920 |
| `json_object_512` | 1063.057 | 81.131 | 119.861 | 13.10× | 85,576 → 102,144 |
| `json_object_2048` | 16615.750 | 338.253 | 502.121 | 49.12× | 340,552 → 406,416 |
| `hash_build_512` | 1838.781 | 102.197 | 750.594 | 17.99× | 12,637,752 → 86,896 |
| `hash_replace_512` | 844.514 | 40.984 | 2517.383 | 20.61× | 2,160,488 → 77,840 |
| `hash_equal_512` | 1027.857 | 63.220 | 173.837 | 16.26× | 91,088 → 107,632 |
| `json_duplicates_512` | 2112.818 | 138.692 | 163.331 | 15.23× | 137,800 → 154,368 |

Lookup scaling cases perform 128 reads of the final key and include argument import. The original `hash_lookup` case performs 1,000 reads from a 64-key hash. `hash_build_512` constructs a hash one assignment at a time; `hash_replace_512` replaces one existing value 128 times; `hash_equal_512` compares equal hashes with opposite insertion orders. JSON object cases parse unique keys, and `json_duplicates_512` parses 512 unique keys followed by their replacements. Compilation and input preparation are outside timing.

## Memory tradeoff

Indexes consume memory. For the 2,048-key JSON result, tracked retained memory increases from 274,512 to 307,360 bytes: **32,848 additional bytes**, about 12%. Peak tracked memory increases from 308,032 to 340,880 bytes. Cumulative allocation volume rises about 19%, including temporary indexes during growth. These measurements include index accounting.

Hash construction benefits in both allocation volume and peak memory: `hash_build_512` drops from 12,637,752 to 86,896 allocated bytes and from 142,960 to 90,088 peak tracked bytes. An eight-key JSON object gains eight bytes of metadata and allocates no index.

Headers, bucket capacity, initialization, hashing, collision probes, key comparisons, index resizing, and copying are accounted. Work over key bytes is bounded in 4 KiB chunks. Deliberately colliding keys are tested for correct lookup and step exhaustion; index allocation failure is tested before allocating its buckets. Hashing is deterministic for reproducible counters. Execution quotas bound collision work; the index does not promise adversarial constant-time lookup with quotas disabled.

## Existing workloads and real programs

The integer loop and eight-key JSON parsing remain essentially unchanged. The JSON transformation workload is 2.8% slower and allocates 520 additional bytes per call. No default SIMD workload regressed more than 4% in this run; these are sample comparisons rather than statistical guarantees.

| Workload | Rust before µs | Rust after µs | Go SIMD µs |
| --- | ---: | ---: | ---: |
| `numeric_loop` | 65.577 | 65.667 | 593.871 |
| `array_growth` | 12.326 | 12.105 | 151.325 |
| `length_unicode` | 35.259 | 35.283 | 23.119 |
| `json_transform` | 38.926 | 40.010 | 187.356 |
| `site_sieve_of_eratosthenes` | 14.778 | 14.669 | 213.660 |
| `site_top_rank_per_group` | 5.842 | 5.538 | 131.324 |
| `site_word_wrap` | 45.304 | 44.602 | 1404.247 |

The website collection contributes **203 unchanged files** pinned at `5ca06f3b643e56f6b14caeec6cd4671268c55dbb`. A complete audit records:

- **74 programs match Go outputs.** They now run in Rust regression tests and the six-build differential suite. Three are also timed without rewriting their source.
- **114 need Rust features outside the current subset.** The most common first blocker is `slice` (28 programs). Four programs also reveal missing implicit string/number concatenation. Other gaps include blocks, ranges, interpolation, typed parameters, bignums, and collection methods. A first error does not identify every missing feature in a program.
- **15 have comparison-harness gaps.** Thirteen return money or duration values that its JSON encoder cannot represent; two require SMS/email capabilities. These are not Go interpreter failures.

The [corpus manifest and expectations](../tests/site/README.md) retain source attribution and hashes. Expected site outputs come from Go, while generated conformance cases use independently computed expectations. The audit records unsupported cases alongside passing programs.

The runnable [Rust SMS preview](../examples/sms.rs) shows an `Engine::register` callback capturing a Rust client, validating arguments, checking cancellation, and returning an accounted value. It uses the currently supported `sms_send` syntax. The site’s `sms.send` syntax and native async host suspension remain future work. No SMS is sent.

## Validation and measurement

- Formatting, Clippy with warnings denied, debug/release tests, portable/SIMD tests, Go vet, and the optional Tokio cancellation tests pass. The SMS example is included in the standard checks.
- New tests cover index thresholds and growth, duplicate replacement, insertion order, string/symbol/raw-byte keys, missing keys, snapshot isolation, self-references, cached depth, deliberate collisions, quota exhaustion, cancellation, and independent imported-index lifetimes. Building 2,000 entries completes within 150,000 steps and 1 MiB of tracked memory.
- **276 cases × six builds = 1,656 passing expected outputs.** Portable and SIMD counters match within each Rust revision. Step counts change across revisions because scanning and copying work changes; Rust and Go quota units remain different.
- **37 workloads × two accounting modes × six builds × 12 rounds = 5,328 timing samples.** Every build occupies every order position twice. Pilot runs select common iteration counts, targeting 75 ms for the slowest build with a minimum of 20 calls.
- Timing binaries have no allocation instrumentation. Separate Rust release binaries count system-allocator requests; Go uses runtime.MemStats, whose size-class accounting differs. Imports and calls are timed; compilation, input construction, and JSON encoding are outside timing. The harness verifies final timed outputs.
- Final verification recomputes medians and ranges from raw samples, checks output digests and iterations, verifies binary hashes including the preserved baseline, and checks all 203 source files against the original website checkout.

Peak RSS is a coarse single-process measurement of this larger fixture set. Compare variants within this run; the previous report used a smaller fixture set.

| Variant | Peak RSS MiB |
| --- | ---: |
| go-portable | 17.22 |
| go-simd | 17.44 |
| rust-portable | 7.95 |
| rust-simd | 8.12 |
| rust-before-portable | 8.31 |
| rust-before-simd | 8.30 |

## Remaining work

Unicode counting and escape-heavy JSON stringification were not changed in this iteration. Unicode length still takes about 1.5× Go’s metered time; escape-heavy JSON stringification without quotas takes about 1.6× Go’s time. These remain performance targets. The example audit gives a concrete compatibility backlog, and native async host capabilities still require VM suspension. These ARM64 measurements do not validate x86_64 SIMD.

## Reproduce

Preserve the four Rust timing/allocation binaries built by `scripts/compare.py --validate-only` at revision `f86f4bcf123005008a3118662ae8b41fc598edf5`, with that full hash in a `revision` file. Keep the checkout, caches, and binaries on the external volume. Then run:

```sh
./scripts/check
python3 scripts/compare.py --baseline /path/to/preserved/binaries --rounds 12 --out .cache/hash-comparison
python3 scripts/audit-site.py --out .cache/site-audit
./scripts/cargo run --release --example sms
```

[Raw timing/allocation results](results/2026-09-12-hashes/summary.json), [environment and binary hashes](results/2026-09-12-hashes/environment.json), [test log](results/2026-09-12-hashes/validation.log), [complete site audit](results/2026-09-12-hashes/site-audit.json), and [verification](results/2026-09-12-hashes/verification.json) preserve the evidence. The [previous iteration](performance-followup.md) and [initial comparison](README.md) remain historical snapshots.

## Complete timings: metered

Median µs per call; lower is better.

| Workload | Go portable | Go SIMD | Rust before portable | Rust before SIMD | Rust after portable | Rust after SIMD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `numeric_loop` | 603.356 | 593.871 | 66.587 | 65.577 | 65.232 | 65.667 |
| `function_calls` | 644.051 | 645.479 | 68.313 | 68.056 | 68.120 | 67.690 |
| `array_sum` | 59.298 | 59.635 | 13.570 | 13.585 | 13.713 | 14.033 |
| `array_growth` | 150.897 | 151.325 | 12.466 | 12.326 | 12.156 | 12.105 |
| `hash_lookup` | 860.255 | 844.170 | 362.691 | 359.802 | 125.147 | 125.207 |
| `hash_lookup_8` | 112.223 | 112.271 | 22.320 | 22.145 | 22.987 | 22.805 |
| `json_object_8` | 4.413 | 4.427 | 1.198 | 1.226 | 1.215 | 1.227 |
| `hash_lookup_512` | 187.343 | 187.678 | 498.130 | 494.316 | 40.052 | 39.409 |
| `json_object_512` | 117.457 | 119.861 | 1022.936 | 1063.057 | 79.987 | 81.131 |
| `hash_lookup_2048` | 396.367 | 403.864 | 1961.862 | 1952.657 | 114.180 | 115.566 |
| `json_object_2048` | 484.774 | 502.121 | 16002.046 | 16615.750 | 332.475 | 338.253 |
| `hash_build_512` | 739.270 | 750.594 | 1800.680 | 1838.781 | 100.577 | 102.197 |
| `hash_replace_512` | 2463.298 | 2517.383 | 832.087 | 844.514 | 41.680 | 40.984 |
| `hash_equal_512` | 172.562 | 173.837 | 1056.740 | 1027.857 | 63.348 | 63.220 |
| `json_duplicates_512` | 161.292 | 163.331 | 2048.802 | 2112.818 | 137.238 | 138.692 |
| `length_16` | 1.344 | 1.362 | 0.178 | 0.194 | 0.180 | 0.171 |
| `upcase_16` | 1.791 | 1.825 | 0.261 | 0.275 | 0.266 | 0.258 |
| `length_4096` | 1.709 | 1.555 | 1.235 | 0.269 | 1.237 | 0.270 |
| `upcase_4096` | 2.944 | 2.716 | 0.422 | 0.428 | 0.401 | 0.422 |
| `length_65536` | 6.747 | 4.063 | 17.055 | 1.594 | 17.052 | 1.556 |
| `upcase_65536` | 16.467 | 16.407 | 3.151 | 3.297 | 2.877 | 3.303 |
| `strip_65536` | 5.709 | 5.630 | 1.598 | 1.637 | 1.587 | 1.608 |
| `length_unicode` | 22.838 | 23.119 | 35.157 | 35.259 | 35.229 | 35.283 |
| `length_mixed_65536` | 23.455 | 20.760 | 17.040 | 1.577 | 17.119 | 1.571 |
| `json_parse_ascii_64k` | 42.264 | 10.863 | 35.186 | 4.199 | 35.204 | 4.200 |
| `json_stringify_ascii_64k` | 64.104 | 16.737 | 28.839 | 5.181 | 28.987 | 5.206 |
| `json_parse_escaped_4k` | 22.511 | 23.433 | 21.162 | 21.367 | 20.744 | 21.135 |
| `json_stringify_escaped_4k` | 101.608 | 101.145 | 39.196 | 40.043 | 40.241 | 40.730 |
| `json_parse_unicode_4k` | 7.846 | 7.766 | 4.514 | 4.459 | 4.510 | 4.502 |
| `json_stringify_unicode_4k` | 7.944 | 7.792 | 4.560 | 4.582 | 4.629 | 4.596 |
| `json_transform` | 187.820 | 187.356 | 38.558 | 38.926 | 39.686 | 40.010 |
| `upstream_fibonacci` | 435.607 | 438.297 | 42.778 | 42.309 | 42.653 | 42.687 |
| `upstream_countdown` | 91.229 | 91.015 | 7.807 | 7.649 | 7.619 | 7.396 |
| `upstream_greeting` | 3.440 | 3.480 | 0.627 | 0.636 | 0.628 | 0.580 |
| `site_sieve_of_eratosthenes` | 214.695 | 213.660 | 14.740 | 14.778 | 14.746 | 14.669 |
| `site_top_rank_per_group` | 129.085 | 131.324 | 5.855 | 5.842 | 5.700 | 5.538 |
| `site_word_wrap` | 1385.065 | 1404.247 | 45.753 | 45.304 | 46.136 | 44.602 |

## Complete timings: unlimited

Median µs per call; lower is better.

| Workload | Go portable | Go SIMD | Rust before portable | Rust before SIMD | Rust after portable | Rust after SIMD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `numeric_loop` | 290.828 | 290.917 | 66.603 | 65.747 | 65.105 | 65.346 |
| `function_calls` | 306.637 | 308.461 | 67.611 | 67.485 | 67.709 | 67.414 |
| `array_sum` | 14.704 | 14.567 | 13.573 | 13.596 | 13.693 | 14.031 |
| `array_growth` | 63.682 | 63.173 | 12.394 | 12.333 | 12.166 | 12.123 |
| `hash_lookup` | 358.569 | 351.982 | 339.830 | 337.213 | 103.163 | 103.656 |
| `hash_lookup_8` | 46.103 | 45.197 | 19.625 | 19.553 | 20.230 | 20.111 |
| `json_object_8` | 2.273 | 2.319 | 1.100 | 1.126 | 1.142 | 1.140 |
| `hash_lookup_512` | 76.535 | 76.362 | 478.534 | 478.412 | 26.161 | 26.328 |
| `json_object_512` | 80.867 | 82.565 | 1016.541 | 1059.687 | 78.705 | 80.435 |
| `hash_lookup_2048` | 168.440 | 171.599 | 1921.401 | 1913.562 | 70.533 | 71.550 |
| `json_object_2048` | 319.549 | 334.243 | 16386.202 | 16716.403 | 331.964 | 337.024 |
| `hash_build_512` | 340.103 | 345.860 | 1782.161 | 1829.016 | 89.729 | 90.668 |
| `hash_replace_512` | 330.256 | 330.944 | 821.060 | 831.634 | 27.566 | 27.647 |
| `hash_equal_512` | 128.776 | 127.234 | 1035.533 | 1004.069 | 42.183 | 42.471 |
| `json_duplicates_512` | 126.553 | 129.341 | 2039.470 | 2094.828 | 136.348 | 137.848 |
| `length_16` | 0.741 | 0.752 | 0.149 | 0.149 | 0.149 | 0.148 |
| `upcase_16` | 0.837 | 0.889 | 0.216 | 0.214 | 0.213 | 0.214 |
| `length_4096` | 1.084 | 0.959 | 1.208 | 0.244 | 1.208 | 0.237 |
| `upcase_4096` | 1.918 | 1.703 | 0.369 | 0.371 | 0.341 | 0.371 |
| `length_65536` | 6.077 | 3.428 | 17.010 | 1.522 | 17.008 | 1.528 |
| `upcase_65536` | 15.303 | 15.317 | 3.061 | 3.249 | 2.821 | 3.250 |
| `strip_65536` | 4.872 | 4.891 | 1.543 | 1.563 | 1.541 | 1.577 |
| `length_unicode` | 22.183 | 22.448 | 35.168 | 35.231 | 35.144 | 35.350 |
| `length_mixed_65536` | 22.903 | 20.173 | 17.033 | 1.553 | 16.997 | 1.540 |
| `json_parse_ascii_64k` | 39.494 | 8.377 | 35.084 | 4.104 | 35.128 | 4.159 |
| `json_stringify_ascii_64k` | 61.866 | 14.794 | 28.855 | 5.056 | 28.922 | 5.101 |
| `json_parse_escaped_4k` | 20.001 | 21.009 | 21.141 | 21.474 | 21.050 | 21.128 |
| `json_stringify_escaped_4k` | 27.209 | 25.495 | 39.424 | 40.039 | 39.998 | 40.656 |
| `json_parse_unicode_4k` | 5.524 | 5.460 | 4.497 | 4.416 | 4.483 | 4.470 |
| `json_stringify_unicode_4k` | 5.979 | 6.057 | 4.507 | 4.527 | 4.543 | 4.515 |
| `json_transform` | 70.941 | 71.244 | 36.831 | 37.053 | 37.916 | 37.923 |
| `upstream_fibonacci` | 177.227 | 178.073 | 42.262 | 41.621 | 42.067 | 41.956 |
| `upstream_countdown` | 18.764 | 18.885 | 7.400 | 7.302 | 7.252 | 7.134 |
| `upstream_greeting` | 1.438 | 1.464 | 0.504 | 0.520 | 0.514 | 0.479 |
| `site_sieve_of_eratosthenes` | 90.734 | 90.744 | 14.709 | 14.665 | 14.695 | 14.608 |
| `site_top_rank_per_group` | 21.225 | 21.468 | 4.692 | 4.633 | 4.487 | 4.417 |
| `site_word_wrap` | 143.211 | 143.644 | 41.940 | 41.640 | 42.287 | 41.060 |
