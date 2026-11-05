# Captured-block release CLI investigation

VIBE-169 follows the approximately 18% captured-loop slowdown reported during
VIBE-135 / PR #1383. The historical comparison is `97eb29832b486719f096d46a9083054b7d36748a`
before that change and `64aa08af57e69fa2f346741d780e495b31078b33` with the import-error
latch. The investigation does not change the runtime or terminal-error policy.

## Method

Apple M4, 16 GiB RAM, arm64 macOS 27.0, Homebrew rustc 1.98.1
(`48a229cea`, LLVM 22.1.8). Both revisions use their checked-in lockfile and release
profile: thin LTO, one codegen unit, debug level 1 and overflow checks. There are no
Rust flags, target, wrapper or release-profile environment overrides. Build output
is isolated by revision. Default SIMD binaries are preserved before building
`--no-default-features` portable binaries. See
[environment.json](results/captured-cli/environment.json) for exact toolchain data.

`scripts/bench_captured_cli.py` runs the real `vibes run FILE` command, using one
symlink entry path for every binary in a comparison. It warms each binary/case
once, then rotates binary order (AB, BA, BA, AB for two binaries). The final pass
rotates and reverses all four binaries, giving each six turns in every execution
position over 24 rounds. Timings include
process startup, compilation, execution and output. Every run checks successful
exit, empty stderr and expected stdout. The default `xhigh` quota profile is used.
These are CLI wall times, not library-only execution times.

The exact fixture initializes the captured variable at top level:

```ruby
t = 0
2000000.times { |i| t = t + (i % 7) * 2 }
```

The CLI prints `2000000`, the final `times` expression's receiver. A second fixture
appends `puts(t)` and verifies `11999990`. The controls compute the same sum with
`while` and `for` loops. A representative record-totaling fixture uses the core
suite's price/quantity/active record pattern: 256 rows, 2,000 passes, nested blocks,
captured total and unit accumulators, and a conditional skip. Its independently
computed totals are checked on every run. The benchmark script contains all five
fixtures; the raw results also retain their source.

This is a shared fleet host. Other agents and system services were active; no
benchmark runs overlap this investigation's own builds or tests. The sizeable
sample spread means these results cannot establish small speed differences or
identify a microarchitectural cause. All samples, execution order and binary
SHA-256 hashes are retained rather than discarding slow runs.

## Measurements

Median whole-process milliseconds, before → with latch; percentage is the ratio
of medians. The SIMD repeat is a separate run using the same preserved binaries.

| Fixture | SIMD, 12 rounds | SIMD repeat, 24 rounds |
|---|---:|---:|
| Exact captured loop | 325.227 → 323.730 (−0.46%) | 329.602 → 331.572 (+0.60%) |
| Captured loop with sum output | 322.688 → 323.720 (+0.32%) | 330.064 → 333.144 (+0.93%) |
| While control | 157.100 → 163.002 (+3.76%) | 161.479 → 168.829 (+4.55%) |
| For-range control | 125.959 → 130.898 (+3.92%) | 134.642 → 142.319 (+5.70%) |
| Record totaling | 167.744 → 179.234 (+6.85%) | 183.143 → 178.462 (−2.56%) |

For the 24-round SIMD repeat, the median of adjacent-pair percentage differences
is +0.79%, +3.21%, +2.82%, +2.18% and −0.34%, respectively. These differ from the
ratios of medians because load varies between runs. The uncaptured controls do
have modest positive differences; the result is not a claim of zero regression
in all workloads. The initial representative-workload increase changes sign on
repeat, while the reported 18% captured-loop increase does not reproduce.

Raw observations:
[initial SIMD](results/captured-cli/historical-simd.json),
[repeat SIMD](results/captured-cli/confirmation-simd.json).

A final 24-round pass interleaves all four preserved binaries using the committed
script. All 500 CLI invocations (including warmups) pass exact output validation:

| Fixture | SIMD | Portable |
|---|---:|---:|
| Exact captured loop | 262.760 → 263.183 (+0.16%) | 257.269 → 257.591 (+0.13%) |
| Captured loop with sum output | 256.994 → 254.220 (−1.08%) | 250.323 → 255.391 (+2.02%) |
| While control | 118.503 → 119.084 (+0.49%) | 121.881 → 119.870 (−1.65%) |
| For-range control | 101.784 → 106.510 (+4.64%) | 103.239 → 100.425 (−2.73%) |
| Record totaling | 136.551 → 136.341 (−0.15%) | 139.383 → 136.069 (−2.38%) |

The exact-loop sample ranges are 222.0–343.6 → 226.4–318.3 ms for SIMD and
223.6–341.1 → 224.5–313.3 ms for portable. Median within-round ratios for that
fixture are +0.40% and +0.91%. Absolute times improved as shared-host load changed;
compare binaries within a pass, not across passes.
[All four binaries' samples](results/captured-cli/final-four.json) retain the spread.

## Generated code

The final linked SIMD CLIs were disassembled with Homebrew LLVM 22.1.8
`llvm-objdump -d --demangle --no-show-raw-insn --disassemble-symbols=...`.
Counts cover entire functions, including failure and unwind paths; they are not
dynamic instruction counts. Stack reservations include saved registers.

| Function | Instructions before → after | Stack bytes before → after |
|---|---:|---:|
| `vm::Run::advance` | 24,046 → 23,674 | 10,800 → 10,768 |
| `vm::simple::run` | 3,380 → 3,380 | 880 → 880 |
| `CallContext::import` | 64 → 79 | 112 → 128 |

`Run::advance` calls the separate `CallContext::import` function in **both**
historical SIMD builds. Thus the reported inlining into `Op::Constant` is not
reproduced with this toolchain/build. The general dispatcher has changed register
allocation and block layout; it is not an identical function merely shifted in
the executable. The historical changes include object and time invariant checks
as well as the import latch, so this comparison does not isolate the latch alone.
The portable builds likewise retain the separate import call and have the same
per-function instruction counts as their SIMD counterparts.

`simple::run` retains the same instruction sequence, register operands and local
branch targets after normalizing external branch addresses, static `adrp`/`add`
address formation and dynamic stub addresses. Its 188-, 34- and 34-byte jump
tables are byte-identical. This normalization does not assert equality of every
referenced constant or callee. Its entry moves from `0x1003e9800` to `0x1003e8d88`.
The actual scalar `Op::Constant` arm charges and copies directly; it does not call
the host-value import function. Code placement remains a plausible influence,
but these observations do not prove it caused the earlier report.

The separate import function retains the immediate-value fast path. Only its
non-immediate branch calls `import_value`, tests the result for an internal error
and calls `latch`; there is no justification for weakening that policy. Static
counts, function addresses and prologues are in
[codegen-simd.json](results/captured-cli/codegen-simd.json).

## Decision and validation

No production optimization is justified by this reproduction. The reported
captured-loop slowdown is absent in both feature builds, and the representative
record workload does not sustain an increase. The positive SIMD range-control
results remain recorded; neither these noisy measurements nor the disassembly
prove that the original report was solely layout noise. A future reproduction
should preserve the affected binary and build environment, confirm a representative
workload on a quiet host, and then compare a cold-path outline against that binary.

The terminal internal-error latch is unchanged. On the branch's runtime base
`ef46c2f781b08490410a72fa4e4d78198f4cf8d3`,
`cargo test --profile gate --lib internal_error` passes all 32 selected tests,
including a swallowed host-import error and rescue/ensure suppression. The
benchmark rejects `/usr/bin/true` as an invalid CLI instead of accepting its empty
output as a fast run. No runtime source changes are part of this investigation.

## Reproduction

Build each historical revision in a separate checkout and target directory with
direct `cargo` (the repository's `scripts/cargo` overrides the target directory):

```sh
CARGO_HOME=/Volumes/AI/Cache/cargo CARGO_TARGET_DIR=/Volumes/AI/Build/tron-vibe-169/before cargo build --release --locked -p vibes
```

Repeat from the latch revision with its own target directory. Preserve each CLI
before rebuilding without default features, then compare the preserved binaries:

```sh
python3 scripts/bench_captured_cli.py \
  --binary before=/path/to/before-simd \
  --binary latch=/path/to/latch-simd \
  --rounds 24 --out /path/to/new-output-directory
```

The output directory must be new. The script saves sources, samples, execution
order, hashes and summaries, and refuses unexpected output. The original SIMD
rounds used a scratch predecessor with identical fixture sources and ordering;
it validated stdout after stripping surrounding whitespace. The committed script
checks exact stdout bytes and cleans up its runner symlink.
