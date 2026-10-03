# Fuzzing

The `Fuzz` workflow runs every night at 07:17 UTC and can be started manually
in GitHub Actions. Each parser/compiler target runs for one hour,
with AddressSanitizer and debug assertions, in both SIMD and portable
builds. The checker compares 100,000 programs from a randomly chosen seed
range against the runtime with all type checks retained. Pull requests
run each coverage target for one minute and compare 1,000 checker seeds
starting at zero.

Nightly coverage corpora are cached across runs. Every checkout also seeds
them with the repository's `.vibe` tests and a few small programs, so a cold
cache still exercises valid syntax. Coverage targets accept UTF-8 source up
to 16 KiB. The parser checks token and interpolation spans and exercises
outlining and unreachable-code analysis; the compiler exercises both modes
of retaining proven type checks, under step and memory quotas. Neither
coverage target executes the source.

## Coverage-guided runs

Install [rustup](https://rustup.rs/) and a C++ compiler. The fuzz toolchain
and `cargo-fuzz` version are pinned separately from the normal build:

```sh
rustup toolchain install nightly-2026-10-02 --profile minimal --component rustfmt
./scripts/cargo install cargo-fuzz --version 0.13.2 --locked --root "$PWD/.cache/cargo-fuzz"
export PATH="$PWD/.cache/cargo-fuzz/bin:$PATH"
export RUSTUP_TOOLCHAIN=nightly-2026-10-02
./scripts/cargo fetch --locked --manifest-path fuzz/Cargo.toml
python3 scripts/fuzz-seeds
./scripts/cargo fuzz run parser --debug-assertions --codegen-units 16 -- \
  -max_total_time=3600 -max_len=16384 -timeout=10 -rss_limit_mb=2048 -dict=fuzz/vibescript.dict
./scripts/cargo fuzz run compiler --debug-assertions --codegen-units 16 -- \
  -max_total_time=3600 -max_len=16384 -timeout=10 -rss_limit_mb=2048 -dict=fuzz/vibescript.dict
```

Add `--no-default-features` before `--` for a portable build. `fuzz` is a
separate Cargo workspace with its own committed lock file; the ordinary
workspace needs neither nightly Rust nor fuzz dependencies.

On failure, download the target's artifact from the workflow. Replay and
minimize an input with the same toolchain and feature flags:

```sh
./scripts/cargo fuzz run parser --debug-assertions path/to/crash-input
./scripts/cargo fuzz tmin parser --debug-assertions path/to/crash-input
```

A fixed finding should become a small regression test in the normal suite.

## Checker runs

Use the normal build toolchain for the [checker differential harness](checker-diff.md):

```sh
./scripts/cargo build --locked --profile gate --example checker_diff
python3 scripts/fuzz-checker --count 100000 --jobs 2
```

The driver prints the chosen seed and command, and keeps the log and findings
under `.cache/fuzz-checker`, uploaded even when the job fails. Replay the
same range by passing `--from SEED`; use `--out DIR` to keep runs separate.
The driver fails on new disagreements, a nonzero harness exit, a missing
summary, or a run that completes fewer seeds than requested. Existing known
differences recorded by the harness remain separate from new findings.
