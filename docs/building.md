# Building Vibescript

The default build requires Go 1.26 or later and uses the portable string
scanners. Build the CLI from the repository root:

```bash
go build ./cmd/vibes
```

## Optional SIMD build

Go 1.27.1's SIMD experiment enables vector instructions for selected string
scans on ARM64 and AMD64:

```bash
GOTOOLCHAIN=go1.27.1 GOEXPERIMENT=simd go build ./cmd/vibes
```

Use the same environment when building a Go application that embeds Vibescript.
ARM64 uses NEON. AMD64 checks for AVX2 at runtime and uses the portable scanner
when it is unavailable. Short inputs and unsupported architectures also use the
portable implementation. The experiment is optional; `go.mod` and ordinary
release builds continue to target Go 1.26.

To exercise the AMD64 fallback on an AVX2 machine:

```bash
GOTOOLCHAIN=go1.27.1 GOEXPERIMENT=simd GODEBUG=cpu.avx2=off go test ./...
```

## Native validation and measurements

The [SIMD workflow](../.github/workflows/simd.yml) runs the full test suite,
`go vet`, and the estimator and frame-recycling oracles on native Linux AMD64
and macOS ARM64 runners. Linux also runs the full suite with AVX2 disabled.

Each run uploads CPU and toolchain details plus six benchmark samples per
ASCII and Unicode string operation through `Script.Call`. Pull requests also
run the relevant benchmark profiles, including short, mixed, invalid-UTF-8,
and direct classifier controls when the ASCII scanner changes. All variants
use Go 1.27.1 and one CPU. Prebuilt base and head binaries alternate their order across
rounds. The workflow copies selected reviewed fixtures to the PR base so both
versions run the same cases. On pull requests, `base-simd.txt` and `head-simd.txt`
measure the change with the experiment enabled; `base-nosimd.txt` and
`head-nosimd.txt` measure the change with it disabled on the same compiler.

Comparing `head-nosimd.txt` with `head-simd.txt` includes any unrelated effects
of the Go experiment. Linux also records `base-simd-avx2-disabled.txt` and
`head-simd-avx2-disabled.txt` on pull requests to compare the fallback with
matching CPU settings. Comparing a disabled build with an enabled build also
includes standard-library effects of disabling AVX2. Treat these comparisons
as whole build measurements, and inspect bytes and allocations alongside
timing.
