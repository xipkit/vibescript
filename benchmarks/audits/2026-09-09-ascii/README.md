# Shared ASCII scanner production measurements

Apple M4, darwin/arm64, Go 1.26.3 and Go 1.27.1. Baseline is
`be97d5eb77205a0f9dc8013aedff9129f7c7cd8e`; measured local head is
`3484a86428cd08e582b0c21572617907b842b975`. The final change preserves its
production kernels and selected benchmark bodies. A later prerequisite fix
only changes untimed formatter benchmark setup. Both revisions run identical
isolated controls, selected fixtures, and workload inputs. Five prebuilt binaries rotate their starting order over ten rounds,
with 100 ms per case per round and no concurrent local builds or benchmarks.
Each binary emits 61 cases and 610 measured rows.

The Go 1.27 builds all use `GOEXPERIMENT=simd`, including the original and
word-only control. The latter replaces only the selected classifier wrapper
with its portable counterpart. Compare within one compiler/experiment first;
compiler and executable placement effects also influence Unicode loops.

The following public benchmarks perform 200 operations on roughly 4 KiB
strings through `Script.Call`. These are microseconds per complete loop:

| ASCII operation | Go 1.26 before | Go 1.26 word | Go 1.27 before | Go 1.27 word | Go 1.27 SIMD |
|---|---:|---:|---:|---:|---:|
| length | 277.4 | 147.1 | 279.6 | 147.8 | 117.4 |
| index | 306.9 | 174.7 | 309.8 | 176.1 | 147.2 |
| rindex | 486.1 | 227.7 | 673.7 | 228.2 | 170.1 |
| slice | 347.6 | 212.4 | 347.1 | 212.2 | 182.6 |

All 61 allocation-count medians match across the five variants. Every direct
classifier case allocates zero bytes and zero objects. The four public ASCII
loops retain 18, 18, 18 and 419 allocations respectively. This change saves CPU;
it does not reduce retained value memory or alter logical accounting.

The Go 1.27 SIMD short and mixed-prefix public controls show no statistically
significant regression. Its Unicode length control increases from 429.1 to
444.4 microseconds (+3.56%, p=.005), and Unicode index increases 0.60% (p=.029).
Go 1.26 portable short controls show increases of 0.70-1.22%, and mixed-prefix
index increases of 1.22-1.59%. Go 1.27 word-only short invalid index and short
Unicode slice increase 0.67% and 0.52%. These are retained tradeoffs alongside
the long ASCII gains, not claims that every workload improves.

Direct empty/tiny classifier calls have a bounded call-overhead tradeoff:
empty input is 0.25 ns before, 1.68 ns word, and 2.41 ns SIMD on Go 1.27;
eight-byte ASCII is 2.31, 2.20, and 2.92 ns. Some early high-byte positions also
regress by less than one nanosecond. Large classifier spans improve
substantially, while an immediate high-bit byte benefits from the inline guard.

The portable loop deliberately stays outside callers. In an earlier candidate,
inlining its bounded tile-copy loop enlarged the Unicode helper frame and
coincided with 10–16% slower Unicode length controls. The final outlined word
loop avoids that larger regression; instruction counts alone did not establish
a precise microarchitectural cause. Native CI measures each final PR binary
independently, since executable placement can change these results.

Raw results are [Go 1.26 before](go126-before.txt), [word](go126-word.txt),
[Go 1.27 before](go127-before.txt), [word](go127-word.txt), and
[SIMD](go127-simd.txt). Full [Go 1.26](go126-benchstat.txt) and
[Go 1.27](go127-benchstat.txt) benchstat tables include all controls.

The benchmark selector is
`^Benchmark(SIMDString(Length|Index|RIndex|Slice)Loop(ASCII|Unicode)|StringASCII(ShortCalls|MixedCalls|Classification))$`.
Run `scripts/simd_profiles.py prepare` with head/base checkouts first to copy
all selected fixtures and inputs; this measurement selected 162 CI cases and
timed the 61 cases above. Build runtime test binaries with the pinned toolchain
and identical experiment, then alternate the prebuilt binaries with
`-test.run=^$ -test.benchmem -test.benchtime=100ms`.
[Build provenance](metadata.json) records toolchains, measured revisions, fixture
hash, binary hashes, and the word-only overlay build. The native SIMD workflow
independently measures matching base/head builds on ARM64 and AMD64, including
matched AVX2-disabled builds on AMD64.
