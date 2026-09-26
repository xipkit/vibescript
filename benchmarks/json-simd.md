# JSON SIMD and allocation report

The implementation stays within JSON, its builtin entry point, and shared byte scanning; it does not change the VM or value representation. Measurements compare the implementation at `f697c7c` with the library at `52c9d8e` (baseline harness commit `bfe501d`). The environment files record `68accfa`, whose source tree is identical to `f697c7c`; only its commit message was amended to add the completed measurements. Subsequent changes record measurements, update documentation and relocate one misplaced doc comment and move the scanner test module below both architecture implementations to satisfy x86_64 Clippy. No measured runtime code changes.

## Design and decisions

- A rolling 64-byte structural index classifies punctuation, quotes, backslashes, whitespace, control bytes, high bytes and digits. Quote parity and odd backslash runs carry across blocks. It uses baseline NEON on arm64, SSE2 on x86_64 and exact eight-byte SWAR masks in portable builds. It neither allocates a document-sized index nor reads past the input.
- Valid unescaped runs bypass decoding and copy directly. Long ASCII runs use the shared vector/SWAR prefix scanner; non-ASCII runs use NEON/SSE2 UTF-8 validation with scalar boundary handling. Invalid bytes retain the existing replacement behavior inside JSON strings and existing errors elsewhere. Integers up to 18 decimal digits avoid checked arithmetic; longer integers, bigint promotion and float conversion retain the established paths.
- After entering an array in a document of at least 512 bytes, a lazily initialized cache shares unescaped keys of at most 64 bytes through a bounded 64-slot cache. Cached values remain ordinary immutable Vibescript strings, never source slices, so returned records do not retain the document. There is no global interner. Key materialization keeps its existing logical charge even when it shares storage.
- `parse_as` checks array elements as they finish, including an array directly inside a packet shape. Successful proofs cover plain scalar/array/small-shape schemas and avoid the later full traversal and value clones. Checks have bounded work; unsupported schemas, mismatches, large indexed objects, unions and nominal conversions use the established normalizer. Proof charges are settled after parsing and type preparation, preserving syntax-error precedence, duplicate-key semantics, resolution, diagnostics and quota counters. The enclosing small shape still needs a bounded final field check.
- Stringify keeps its existing NEON/SSE2 all-clear scanning and bulk-copy path. Portable escaping now scans eight bytes together with SWAR; mixed UTF-8 and escape boundaries preserve the original decoding rules. Existing bulk copies, capacity headroom, six-byte ASCII escape guard and growth order remain intact. A full-document sizing pass and eager container reservations were deliberately omitted to preserve allocation-failure order and avoid extra traversal.

## Results and limits

For 1 MiB metered SIMD calls, M4 improves by 10.8% for parse, 13.5% for typed packets, 13.9% for typed arrays and 12.3% for projection/stringify; standalone stringify is essentially unchanged (0.7% faster). Intel improves by 18.8%, 23.8%, 23.4%, 17.7% and 11.5% respectively. Parsing allocations fall from 301,406 to 186,008 (-38.3%), peak tracked bytes from 16,009,540 to 12,143,707 (-24.1%), and retained bytes from 14,956,390 to 11,090,557 (-25.8%).

The gains are workload dependent. In metered calls, flat-object and duplicate-key microbenchmarks regress by up to 8.4%; portable escaped stringify regresses 23.8% on M4 (9.15 to 11.33 µs), and escaped parsing regresses about 10% on Intel (11.58 to 12.75 µs with SIMD). With limits unset, portable escaped stringify regresses 25.1% on M4 and portable escaped parsing 10.3% on Intel. These remain explicit tradeoffs for the API-record and long-string improvements. M4 SIMD Unicode parsing improves 62.5% and Intel 50.3%; portable 64 KiB ASCII parsing improves 73.1% and 48.5%. The previous candidate's M4 escaped parsing regression was removed (33.95 to 12.76 µs, against a 12.78 µs starting runtime).

The API payloads contain 981, 16,343, 262,136 and 1,048,536 bytes, with 6, 102, 1,612 and 6,412 nested records. Every size includes mixed numbers, Unicode, escapes, typed packets, typed arrays and projection into JSON requests. Nested stringify runs at 16 KiB and above.

## Accounting and behavior

Across all 107,418 shared cases on **each architecture**, no logical step count changes relative to the starting runtime. Existing counter drift from the historical golden recordings is unchanged except for the memory differences listed below. Portable and SIMD builds agree exactly on steps, peak tracked bytes and retained tracked bytes. The complete per-case differences are in the architecture-specific `counter-changes.json` files below: 42 peak changes and 24 retained changes; every change reduces tracked memory. Most savings come from sharing repeated keys, a justified memory reduction larger than 5%. Flat objects do not initialize the cache or structural index. Escaped strings use the established decoding path and resume the index at their closing quote, avoiding the regressions found in the first candidate. No golden was re-recorded.

## Measurement method

Both suites ran eight rotating rounds with preserved baseline binaries, separately instrumented allocation builds, thin LTO and one codegen unit. Hosts were checked for competing gates. The environment files list only the untracked `.cache` and `target` symlinks as dirty; measured source files were clean. Darwin is an Apple M4; shannon is an Intel Core Ultra 9 285H. Vinci was not used for measurements. The tables use the paired before/after run, rather than comparing independently scheduled runs. Both limit modes and both scanner builds are included. Timings cover argument import and execution; the stringify workload is a complete call with typed input, not an isolated writer microbenchmark. The `/unlimited` cases unset step and memory limits; they still record steps and tracked memory, so their counters remain comparable. RSS is measured in a fresh process per case and includes the harness, argument preparation, compilation, output encoding and repeated calls; small RSS differences are noisy and are not call-only memory measurements. On Linux, launcher inheritance also creates a roughly 13.5 MiB floor for small processes.

CSV values and retained JSON summaries provide exact counts, allocation volume and all eight timing samples; displayed tables round units for readability. The existing core suite was also measured to expose regressions outside the added API workloads.

## Verification

- Formatting and both all-target Clippy configurations passed with warnings denied.
- Native workspace tests: 1,994 passed, zero failed.
- Full goldens: all eight corpora passed, including 106,389 language cases, 56,827 replay cases and 216 LSP sessions.
- Portable/SIMD validation: 107,418 shared cases passed with exact accounting parity; both golden builds passed.
- WASI: 1,843 tests passed, both Clippy configurations passed, CLI and filesystem witnesses passed under Wasmtime with normal stack limits.
- New differential tests cover every byte in every vector lane, quote/backslash carries, random malformed documents, invalid UTF-8 in every position class, boundary escapes, long strings, huge integers/floats and depths 9,999/10,000/10,001. Valid JSON values and huge integers are also checked against `serde_json` where the semantics coincide. Streaming proofs are compared with ordinary normalization at every step-quota boundary in their fixtures. These tests passed natively on arm64 and x86_64 with SIMD and portable builds, and on WASI.

[Local verification logs](results/json-simd/verification-local.tgz) retain the complete command output. The final distributed gate runs after the report commit; its exact branch head and result are recorded in the delivery message.

## Compact-record proposal

The typed VM could provide a shape-aware record builder that maps decoded field names to precomputed slots and fills a single values buffer while parsing. It must retain last-value-wins duplicates, observable insertion order, optional-field presence, open-shape extras and the existing late diagnostic precedence. Charge the actual builder and retained storage before allocation, use an explicit frame stack for depth limits, and fall back to ordinary hashes for schemas needing nominal normalization or dynamic fields. This would remove per-record key/value pair storage and repeated field lookup beyond the key sharing implemented here. It needs the parallel branch's representation contract and is deliberately not implemented in this branch.

[Exact per-case CSV, including non-JSON controls](results/json-simd/all-cases.csv). Compressed evidence archives beside the summaries retain raw rounds, pilots, allocation records and RSS observations.

## Apple M4 (arm64)

[API summary](results/json-simd/arm64-api-summary.json), [core summary](results/json-simd/arm64-core-summary.json), [every counter change](results/json-simd/arm64-counter-changes.json).

### portable, metered

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 8.81 → 8.56 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 5.94 → 5.94 |
| api_shape_1k | 12.09 → 11.30 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 6.03 → 5.95 |
| api_array_1k | 11.78 → 10.91 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 5.91 → 5.94 |
| api_project_1k | 14.67 → 13.88 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 6.31 → 6.20 |
| api_parse_16k | 138.14 → 124.65 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 7.16 → 6.70 |
| api_shape_16k | 193.00 → 167.55 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 7.22 → 6.86 |
| api_array_16k | 193.16 → 167.90 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 6.98 → 6.73 |
| api_project_16k | 231.32 → 199.71 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 6.92 → 6.64 |
| api_stringify_16k | 131.78 → 135.02 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 6.92 → 6.73 |
| api_parse_256k | 2,145.22 → 1,929.00 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.44 → 17.61 |
| api_shape_256k | 2,953.28 → 2,571.05 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.69 → 17.77 |
| api_array_256k | 2,996.58 → 2,580.57 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.58 → 17.67 |
| api_project_256k | 3,573.96 → 3,103.43 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 11.77 → 10.69 |
| api_stringify_256k | 2,051.38 → 2,072.73 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 17.12 → 15.80 |
| api_parse_1m | 8,434.85 → 7,623.95 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 67.09 → 52.27 |
| api_shape_1m | 11,812.47 → 10,246.61 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 67.56 → 50.52 |
| api_array_1m | 11,882.21 → 10,207.92 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 67.36 → 52.50 |
| api_project_1m | 14,082.18 → 12,306.12 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 28.94 → 24.08 |
| api_stringify_1m | 8,186.22 → 8,255.56 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 49.84 → 44.91 |
| object_8 | 1.84 → 1.91 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 5.94 → 5.88 |
| object_512 | 69.99 → 75.55 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 6.45 → 6.34 |
| object_2048 | 284.42 → 305.66 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 7.34 → 7.14 |
| duplicates_512 | 118.43 → 128.35 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 6.28 → 6.27 |
| parse_ascii_64k | 34.97 → 9.39 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 6.25 → 6.19 |
| stringify_ascii_64k | 35.06 → 11.06 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 6.34 → 6.27 |
| parse_escaped_4k | 12.09 → 12.58 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 5.98 → 6.02 |
| stringify_escaped_4k | 9.15 → 11.33 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 6.09 → 6.03 |
| parse_unicode_4k | 5.12 → 4.20 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 6.03 → 5.84 |
| stringify_unicode_4k | 5.31 → 5.37 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 6.09 → 5.91 |
| transform | 59.36 → 48.79 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 6.36 → 6.36 |

### portable, unlimited

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 8.65 → 8.45 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 5.98 → 5.89 |
| api_shape_1k | 12.06 → 11.29 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 6.11 → 5.97 |
| api_array_1k | 11.80 → 10.94 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 5.98 → 5.94 |
| api_project_1k | 14.63 → 13.84 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 6.25 → 6.14 |
| api_parse_16k | 138.17 → 124.41 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 6.97 → 6.78 |
| api_shape_16k | 193.62 → 167.18 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 7.03 → 6.84 |
| api_array_16k | 193.42 → 167.88 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 7.03 → 6.73 |
| api_project_16k | 230.00 → 199.43 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 6.69 → 6.64 |
| api_stringify_16k | 131.00 → 133.24 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 6.94 → 6.73 |
| api_parse_256k | 2,130.56 → 1,924.14 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.52 → 17.14 |
| api_shape_256k | 2,970.19 → 2,585.95 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.62 → 17.39 |
| api_array_256k | 2,982.47 → 2,580.32 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.52 → 17.27 |
| api_project_256k | 3,562.29 → 3,111.89 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 11.98 → 10.77 |
| api_stringify_256k | 2,045.52 → 2,063.14 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 17.03 → 15.80 |
| api_parse_1m | 8,433.78 → 7,622.26 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 67.09 → 50.19 |
| api_shape_1m | 11,762.25 → 10,228.76 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 67.12 → 50.48 |
| api_array_1m | 11,836.87 → 10,278.95 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 67.34 → 50.44 |
| api_project_1m | 14,022.22 → 12,310.17 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 28.78 → 24.11 |
| api_stringify_1m | 8,143.90 → 8,245.29 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 49.62 → 45.00 |
| object_8 | 1.77 → 1.86 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 5.97 → 5.84 |
| object_512 | 69.69 → 75.43 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 6.45 → 6.39 |
| object_2048 | 284.82 → 305.85 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 7.31 → 7.09 |
| duplicates_512 | 118.35 → 127.50 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 6.47 → 6.36 |
| parse_ascii_64k | 35.03 → 9.40 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 6.27 → 6.22 |
| stringify_ascii_64k | 35.11 → 11.09 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 6.33 → 6.17 |
| parse_escaped_4k | 12.37 → 12.59 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 6.16 → 6.05 |
| stringify_escaped_4k | 9.08 → 11.36 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 6.06 → 6.02 |
| parse_unicode_4k | 5.11 → 4.20 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 5.98 → 5.92 |
| stringify_unicode_4k | 5.31 → 5.39 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 6.06 → 5.89 |
| transform | 59.38 → 48.76 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 6.41 → 6.38 |

### simd, metered

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 8.70 → 8.61 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 5.95 → 5.89 |
| api_shape_1k | 12.08 → 11.40 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 5.91 → 5.95 |
| api_array_1k | 11.86 → 10.98 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 5.88 → 5.88 |
| api_project_1k | 14.65 → 13.86 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 6.22 → 6.19 |
| api_parse_16k | 140.50 → 124.72 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 6.95 → 6.72 |
| api_shape_16k | 195.27 → 167.26 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 6.92 → 6.83 |
| api_array_16k | 194.35 → 167.62 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 6.84 → 6.80 |
| api_project_16k | 231.13 → 201.29 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 6.67 → 6.62 |
| api_stringify_16k | 132.12 → 132.36 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 6.73 → 6.73 |
| api_parse_256k | 2,152.96 → 1,927.08 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.27 → 17.86 |
| api_shape_256k | 2,987.71 → 2,572.33 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.42 → 17.39 |
| api_array_256k | 3,000.19 → 2,583.33 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 20.08 → 17.25 |
| api_project_256k | 3,564.69 → 3,122.79 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 11.77 → 10.61 |
| api_stringify_256k | 2,053.29 → 2,043.31 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 17.02 → 15.83 |
| api_parse_1m | 8,555.41 → 7,632.91 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 61.70 → 52.94 |
| api_shape_1m | 11,853.15 → 10,251.21 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 62.00 → 50.50 |
| api_array_1m | 11,894.56 → 10,238.29 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 67.42 → 50.44 |
| api_project_1m | 14,146.19 → 12,407.69 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 28.88 → 24.12 |
| api_stringify_1m | 8,197.46 → 8,138.81 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 49.73 → 45.03 |
| object_8 | 1.86 → 1.90 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 5.84 → 5.84 |
| object_512 | 71.17 → 75.33 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 6.23 → 6.33 |
| object_2048 | 288.16 → 304.69 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 7.17 → 7.16 |
| duplicates_512 | 119.63 → 129.39 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 6.27 → 6.33 |
| parse_ascii_64k | 5.04 → 5.11 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 6.23 → 6.20 |
| stringify_ascii_64k | 5.82 → 5.85 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 6.25 → 6.17 |
| parse_escaped_4k | 12.78 → 12.76 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 6.06 → 6.05 |
| stringify_escaped_4k | 10.03 → 10.51 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 6.02 → 5.95 |
| parse_unicode_4k | 5.15 → 1.93 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 5.92 → 5.94 |
| stringify_unicode_4k | 5.29 → 5.39 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 5.97 → 5.92 |
| transform | 59.57 → 48.90 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 6.30 → 6.36 |

### simd, unlimited

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 8.56 → 8.49 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 5.89 → 5.81 |
| api_shape_1k | 12.08 → 11.30 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 5.91 → 5.95 |
| api_array_1k | 11.85 → 10.97 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 5.84 → 5.88 |
| api_project_1k | 14.66 → 13.86 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 6.19 → 6.12 |
| api_parse_16k | 139.86 → 124.17 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 6.94 → 6.73 |
| api_shape_16k | 194.18 → 167.05 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 7.02 → 6.83 |
| api_array_16k | 194.04 → 167.62 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 6.92 → 6.72 |
| api_project_16k | 230.54 → 200.58 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 6.67 → 6.64 |
| api_stringify_16k | 132.02 → 131.26 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 6.77 → 6.73 |
| api_parse_256k | 2,152.09 → 1,929.92 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.28 → 17.12 |
| api_shape_256k | 2,988.92 → 2,577.83 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.42 → 17.42 |
| api_array_256k | 2,992.77 → 2,577.03 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 20.03 → 18.08 |
| api_project_256k | 3,569.10 → 3,123.45 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 11.80 → 10.62 |
| api_stringify_256k | 2,040.83 → 2,033.74 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 16.89 → 15.80 |
| api_parse_1m | 8,536.09 → 7,623.18 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 66.81 → 50.17 |
| api_shape_1m | 11,827.49 → 10,242.52 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 62.00 → 50.53 |
| api_array_1m | 11,823.86 → 10,227.71 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 61.91 → 53.28 |
| api_project_1m | 14,104.60 → 12,330.10 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 28.80 → 24.11 |
| api_stringify_1m | 8,169.14 → 8,117.32 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 49.70 → 45.03 |
| object_8 | 1.82 → 1.85 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 5.88 → 5.88 |
| object_512 | 70.69 → 75.21 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 6.20 → 6.42 |
| object_2048 | 288.04 → 302.95 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 7.23 → 7.08 |
| duplicates_512 | 119.25 → 128.29 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 6.25 → 6.34 |
| parse_ascii_64k | 5.07 → 5.17 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 6.20 → 6.20 |
| stringify_ascii_64k | 5.84 → 5.85 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 6.25 → 6.19 |
| parse_escaped_4k | 12.75 → 12.76 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 5.98 → 6.02 |
| stringify_escaped_4k | 9.73 → 10.27 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 6.00 → 6.00 |
| parse_unicode_4k | 5.11 → 1.91 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 5.91 → 5.94 |
| stringify_unicode_4k | 5.30 → 5.39 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 5.95 → 5.92 |
| transform | 59.73 → 48.77 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 6.27 → 6.36 |

## Intel Core Ultra 9 285H (x86_64)

[API summary](results/json-simd/x86_64-api-summary.json), [core summary](results/json-simd/x86_64-core-summary.json), [every counter change](results/json-simd/x86_64-counter-changes.json).

### portable, metered

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 13.01 → 12.35 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 13.43 → 13.47 |
| api_shape_1k | 17.31 → 15.84 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 13.46 → 13.47 |
| api_array_1k | 16.83 → 15.32 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 13.47 → 13.47 |
| api_project_1k | 23.06 → 20.94 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 13.46 → 13.44 |
| api_parse_16k | 201.99 → 177.16 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 13.44 → 13.46 |
| api_shape_16k | 272.38 → 225.84 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 13.46 → 13.46 |
| api_array_16k | 272.06 → 225.77 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 13.47 → 13.36 |
| api_project_16k | 349.79 → 306.51 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 13.46 → 13.46 |
| api_stringify_16k | 236.35 → 235.16 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 13.47 → 13.47 |
| api_parse_256k | 3,161.44 → 2,766.95 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.74 → 17.68 |
| api_shape_256k | 4,204.53 → 3,469.85 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.82 → 17.96 |
| api_array_256k | 4,199.73 → 3,466.33 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.75 → 17.69 |
| api_project_256k | 5,379.16 → 4,648.60 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 13.47 → 13.47 |
| api_stringify_256k | 3,798.43 → 3,784.01 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 18.23 → 17.14 |
| api_parse_1m | 13,461.04 → 11,321.47 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 64.48 → 48.67 |
| api_shape_1m | 18,146.39 → 14,261.04 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 64.65 → 48.72 |
| api_array_1m | 18,135.67 → 14,239.73 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 64.59 → 48.76 |
| api_project_1m | 21,893.88 → 18,546.82 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 30.57 → 25.36 |
| api_stringify_1m | 17,564.92 → 15,799.56 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 52.14 → 46.73 |
| object_8 | 2.51 → 2.62 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 13.45 → 13.45 |
| object_512 | 99.40 → 102.89 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 13.47 → 13.46 |
| object_2048 | 396.40 → 409.43 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 13.47 → 13.46 |
| duplicates_512 | 165.45 → 172.63 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 13.47 → 13.47 |
| parse_ascii_64k | 19.20 → 9.89 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 13.47 → 13.47 |
| stringify_ascii_64k | 15.73 → 10.46 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 13.46 → 13.46 |
| parse_escaped_4k | 11.06 → 12.17 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 13.46 → 13.47 |
| stringify_escaped_4k | 10.82 → 11.73 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 13.43 → 13.46 |
| parse_unicode_4k | 5.09 → 5.01 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 13.47 → 13.47 |
| stringify_unicode_4k | 5.28 → 5.28 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 13.47 → 13.47 |
| transform | 86.73 → 65.59 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 13.47 → 13.47 |

### portable, unlimited

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 13.01 → 12.44 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 13.47 → 13.44 |
| api_shape_1k | 17.25 → 15.89 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 13.46 → 13.46 |
| api_array_1k | 16.82 → 15.32 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 13.35 → 13.46 |
| api_project_1k | 22.97 → 20.81 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 13.47 → 13.47 |
| api_parse_16k | 203.12 → 178.55 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 13.46 → 13.46 |
| api_shape_16k | 270.46 → 225.27 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 13.47 → 13.45 |
| api_array_16k | 270.61 → 225.15 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 13.47 → 13.46 |
| api_project_16k | 348.03 → 304.99 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 13.45 → 13.47 |
| api_stringify_16k | 235.19 → 233.25 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 13.47 → 13.46 |
| api_parse_256k | 3,108.05 → 2,733.61 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.72 → 17.92 |
| api_shape_256k | 4,180.27 → 3,453.68 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.73 → 18.03 |
| api_array_256k | 4,189.46 → 3,456.55 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.96 → 17.84 |
| api_project_256k | 5,366.85 → 4,638.74 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 13.47 → 13.46 |
| api_stringify_256k | 3,776.10 → 3,768.46 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 18.52 → 17.00 |
| api_parse_1m | 13,263.09 → 11,174.28 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 64.57 → 48.68 |
| api_shape_1m | 18,068.93 → 14,197.85 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 64.73 → 48.79 |
| api_array_1m | 18,103.28 → 14,176.58 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 64.46 → 48.85 |
| api_project_1m | 21,872.76 → 18,534.33 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 30.60 → 25.48 |
| api_stringify_1m | 17,527.67 → 15,723.59 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 51.92 → 46.86 |
| object_8 | 2.51 → 2.61 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 13.37 → 13.47 |
| object_512 | 98.73 → 102.60 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 13.47 → 13.43 |
| object_2048 | 393.30 → 406.63 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 13.45 → 13.43 |
| duplicates_512 | 165.37 → 172.03 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 13.36 → 13.47 |
| parse_ascii_64k | 19.20 → 9.94 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 13.45 → 13.44 |
| stringify_ascii_64k | 15.71 → 10.41 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 13.47 → 13.36 |
| parse_escaped_4k | 11.01 → 12.15 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 13.46 → 13.47 |
| stringify_escaped_4k | 10.80 → 11.73 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 13.47 → 13.43 |
| parse_unicode_4k | 5.10 → 5.02 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 13.46 → 13.47 |
| stringify_unicode_4k | 5.28 → 5.28 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 13.47 → 13.44 |
| transform | 86.28 → 65.84 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 13.43 → 13.47 |

### simd, metered

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 13.07 → 11.98 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 13.47 → 13.47 |
| api_shape_1k | 17.29 → 15.46 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 13.48 → 13.47 |
| api_array_1k | 16.82 → 14.81 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 13.36 → 13.47 |
| api_project_1k | 23.24 → 20.24 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 13.47 → 13.47 |
| api_parse_16k | 203.54 → 170.91 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 13.46 → 13.47 |
| api_shape_16k | 273.25 → 219.41 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 13.45 → 13.46 |
| api_array_16k | 271.79 → 218.01 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 13.45 → 13.47 |
| api_project_16k | 350.55 → 298.72 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 13.47 → 13.46 |
| api_stringify_16k | 237.61 → 231.84 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 13.46 → 13.47 |
| api_parse_256k | 3,164.67 → 2,666.78 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.76 → 17.95 |
| api_shape_256k | 4,216.67 → 3,350.49 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 22.00 → 18.00 |
| api_array_256k | 4,214.59 → 3,351.17 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.93 → 17.70 |
| api_project_256k | 5,397.16 → 4,523.37 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 13.36 → 13.37 |
| api_stringify_256k | 3,808.65 → 3,740.71 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 18.56 → 17.21 |
| api_parse_1m | 13,409.02 → 10,891.17 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 64.76 → 49.04 |
| api_shape_1m | 18,086.48 → 13,789.41 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 64.61 → 49.09 |
| api_array_1m | 18,063.10 → 13,834.89 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 64.71 → 48.66 |
| api_project_1m | 22,001.95 → 18,096.79 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 30.69 → 25.40 |
| api_stringify_1m | 17,616.96 → 15,585.81 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 52.28 → 46.70 |
| object_8 | 2.50 → 2.61 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 13.47 → 13.47 |
| object_512 | 98.16 → 102.21 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 13.47 → 13.44 |
| object_2048 | 392.88 → 407.59 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 13.46 → 13.47 |
| duplicates_512 | 164.26 → 171.90 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 13.47 → 13.36 |
| parse_ascii_64k | 4.46 → 4.55 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 13.47 → 13.47 |
| stringify_ascii_64k | 4.93 → 4.94 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 13.46 → 13.47 |
| parse_escaped_4k | 11.58 → 12.75 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 13.47 → 13.47 |
| stringify_escaped_4k | 10.93 → 10.92 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 13.47 → 13.36 |
| parse_unicode_4k | 5.07 → 2.52 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 13.46 → 13.47 |
| stringify_unicode_4k | 5.43 → 5.42 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 13.47 → 13.44 |
| transform | 87.16 → 64.10 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 13.47 → 13.47 |

### simd, unlimited

Before → after. Time is µs, tracked memory is KiB, RSS is MiB, and allocations are counts.

| Case | Time | Allocations | Peak tracked | Retained tracked | RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| api_parse_1k | 13.05 → 12.08 | 314 → 224 | 19.89 → 16.95 | 14.43 → 11.48 | 13.47 → 13.47 |
| api_shape_1k | 17.30 → 15.36 | 315 → 225 | 21.47 → 18.52 | 14.43 → 11.48 | 13.47 → 13.47 |
| api_array_1k | 16.76 → 14.83 | 304 → 214 | 20.48 → 17.53 | 13.77 → 10.82 | 13.46 → 13.47 |
| api_project_1k | 23.07 → 20.31 | 370 → 280 | 28.34 → 25.40 | 0.34 → 0.34 | 13.47 → 13.46 |
| api_parse_16k | 203.45 → 171.71 | 4,830 → 3,012 | 253.52 → 194.04 | 233.05 → 173.58 | 13.47 → 13.43 |
| api_shape_16k | 271.78 → 218.10 | 4,831 → 3,013 | 255.09 → 195.62 | 233.05 → 173.58 | 13.47 → 13.44 |
| api_array_16k | 270.48 → 217.35 | 4,820 → 3,002 | 254.10 → 194.62 | 232.39 → 172.92 | 13.47 → 13.47 |
| api_project_16k | 349.12 → 297.64 | 5,666 → 3,848 | 302.09 → 242.62 | 4.09 → 4.09 | 13.44 → 13.44 |
| api_stringify_16k | 236.56 → 230.86 | 1,978 → 1,978 | 225.14 → 225.14 | 16.09 → 16.09 | 13.47 → 13.47 |
| api_parse_256k | 3,135.66 → 2,626.03 | 75,804 → 46,806 | 3,932.82 → 2,984.15 | 3,672.35 → 2,723.68 | 21.77 → 17.77 |
| api_shape_256k | 4,190.98 → 3,344.42 | 75,805 → 46,807 | 3,934.39 → 2,985.73 | 3,672.35 → 2,723.68 | 21.95 → 18.03 |
| api_array_256k | 4,180.01 → 3,344.46 | 75,794 → 46,796 | 3,933.40 → 2,984.73 | 3,671.69 → 2,723.02 | 21.85 → 17.92 |
| api_project_256k | 5,393.40 → 4,520.68 | 88,726 → 59,728 | 4,630.46 → 3,681.79 | 64.09 → 64.09 | 13.47 → 13.47 |
| api_stringify_256k | 3,803.25 → 3,723.13 | 30,672 → 30,672 | 3,498.97 → 3,498.97 | 256.09 → 256.09 | 18.59 → 17.10 |
| api_parse_1m | 13,211.02 → 10,731.94 | 301,406 → 186,008 | 15,634.32 → 11,859.09 | 14,605.85 → 10,830.62 | 64.60 → 48.84 |
| api_shape_1m | 17,975.09 → 13,756.11 | 301,407 → 186,009 | 15,635.89 → 11,860.66 | 14,605.85 → 10,830.62 | 64.71 → 48.83 |
| api_array_1m | 17,981.59 → 13,710.68 | 301,396 → 185,998 | 15,634.90 → 11,859.67 | 14,605.19 → 10,829.96 | 64.53 → 48.94 |
| api_project_1m | 21,860.89 → 18,044.77 | 352,734 → 237,336 | 18,399.46 → 14,624.23 | 256.09 → 256.09 | 30.65 → 25.67 |
| api_stringify_1m | 17,559.77 → 15,531.66 | 121,874 → 121,874 | 13,913.47 → 13,913.47 | 1,024.09 → 1,024.09 | 52.09 → 46.79 |
| object_8 | 2.50 → 2.60 | 45 → 45 | 5.75 → 5.75 | 1.16 → 1.16 | 13.45 → 13.48 |
| object_512 | 97.73 → 101.85 | 1,575 → 1,575 | 87.65 → 87.65 | 75.18 → 75.18 | 13.46 → 13.46 |
| object_2048 | 391.17 → 405.97 | 6,189 → 6,189 | 336.65 → 336.65 | 300.18 → 300.18 | 13.46 → 13.47 |
| duplicates_512 | 163.33 → 171.35 | 3,111 → 3,111 | 95.65 → 95.65 | 75.18 → 75.18 | 13.47 → 13.46 |
| parse_ascii_64k | 4.46 → 4.56 | 30 → 30 | 133.41 → 133.41 | 64.65 → 64.65 | 13.47 → 13.44 |
| stringify_ascii_64k | 4.93 → 4.95 | 30 → 30 | 132.26 → 132.26 | 64.11 → 64.11 | 13.46 → 13.47 |
| parse_escaped_4k | 11.61 → 12.73 | 41 → 41 | 24.91 → 24.91 | 8.65 → 8.65 | 13.47 → 13.44 |
| stringify_escaped_4k | 10.97 → 10.97 | 31 → 31 | 24.28 → 24.28 | 8.14 → 8.14 | 13.47 → 13.47 |
| parse_unicode_4k | 5.08 → 2.52 | 30 → 30 | 17.16 → 17.16 | 4.66 → 4.66 | 13.47 → 13.47 |
| stringify_unicode_4k | 5.44 → 5.42 | 30 → 30 | 12.28 → 12.28 | 4.12 → 4.12 | 13.47 → 13.45 |
| transform | 87.09 → 64.19 | 1,253 → 497 | 66.76 → 42.09 | 0.12 → 0.12 | 13.47 → 13.47 |

