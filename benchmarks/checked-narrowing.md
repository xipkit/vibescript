# Checked narrowing: recover frame construction inlining

VIBE-195 follows the checked-integer adoption in #1420. The narrowing checks
remain enabled and fallible. Two inlining hints recover the three reported
runtime regressions without changing representations or budget accounting.

## Diagnosis

On shannon, the preserved `19a4179b` and `5bed4faa` release binaries reproduce
the integer-power, nested-block and range-iteration regressions. Both embed
Rust 1.98.1 / LLVM 22.1.8 compiler identities. Fresh builds use that toolchain,
not shannon's newer system Rust. The current comparison baseline is
`4cb18cb4ab042a0968eed3cc5303b7da9cdbc1b8`, which includes the adopted checks.

Per-function x86_64 disassembly exposes a changed inlining decision:

| Function | Before adoption | Checked baseline | Constructor forced inline | Both forced inline |
|---|---:|---:|---:|---:|
| `Frame::new` | inlined | 701 bytes | inlined | inlined |
| `new_frame` | inlined | 441 bytes | 807 bytes | inlined |
| `enter_block` | 1,738 bytes | 1,901 bytes | 1,901 bytes | 2,322 bytes |
| Common iterator `advance` | 2,322 bytes | 2,583 bytes | 2,583 bytes | 2,583 bytes |

After adoption, `enter_block` calls `new_frame`, which calls `Frame::new`.
The caller reserves 456 stack bytes rather than 344, and `new_frame` reserves
another 184 bytes. Its successful return copies the large frame result through
stack temporaries. Forcing only the constructor inline removes one boundary;
forcing the wrapper inline as well removes the second. The benchmark below
isolates those two changes. The comparator's text segment grows by only
1,600 bytes (0.021%) from the checked baseline to the final variant.

The range conversion did add instructions and increased the iterator's stack
reservation from 312 to 328 bytes. That code remains unchanged by this fix,
including both range-value and iteration-index checks. The frame/local-slot
narrowing checks also remain. The measured recovery therefore does not require
a representation redesign or unchecked conversions. It establishes that the
two call boundaries account for the recoverable cost in these workloads; it
does not assign every timing difference to a particular machine instruction.

## Measurements

Intel Core Ultra 9 285H, Linux x86_64, CPU 2 pinned with `taskset`, Rust 1.98.1,
default SIMD release profile (thin LTO, one codegen unit, overflow checks).
Eight rounds rotate four binary variants so each occupies every position twice.
Every variant uses the same runner path and the same per-case iteration count,
chosen for approximately 50 ms on the slowest variant. Compilation and output
encoding are outside the timer; argument import and execution are inside.

| Case | Original ns | Checked ns | Constructor only ns | Final ns | Final vs checked |
|---|---:|---:|---:|---:|---:|
| `integer_power/unlimited` | 6,144 | 6,564 | 6,343 | 6,074 | -7.5% |
| `nested_blocks/unlimited` | 152,257 | 166,468 | 158,847 | 151,077 | -9.2% |
| `range_each/metered` | 121,232 | 134,783 | 127,559 | 120,516 | -10.6% |

Across all 32 cases, the final geometric mean is 1.90% faster than the checked
baseline and 0.17% faster than the original baseline. No case regresses more
than 0.83% against the checked baseline. Every binary has zero disturbed rounds
and zero pulled-up medians under `compare.py`'s existing thresholds. These are
targeted workloads, not a whole-application performance estimate.

All 32 cases have identical complete validation records across every variant:
results, output digests, steps, peak tracked memory and retained tracked memory.
Results also match the fixture expectations, and timed output digests are
checked after every round. The frame-boundary regression test accepts maximum
32-bit indices and rejects overflowing function, operand and local indices as
internal errors.

## Reproduce

Preserve `--release --locked --example compare` binaries built with Rust 1.98.1
from `19a4179b`, the checked baseline, the constructor-only hint, and this patch.
Use separate target directories for different source trees and keep default
features enabled. Then run from this checkout:

```sh
taskset -c 2 python3 scripts/bench_narrowing.py --rounds 8 \
  --binary original=/absolute/path/original-compare \
  --binary checked=/absolute/path/checked-compare \
  --binary constructor=/absolute/path/constructor-compare \
  --binary inline=/absolute/path/final-compare \
  --out /absolute/path/new-results-directory
```

The harness saves binary hashes, complete validation records, per-round results,
raw timing samples and reliability diagnostics. It rejects result/accounting
differences and unreliable timing sets. The selected workload prefixes reproduce
the original card's 32 cases, in metered and unlimited modes. The committed
`checked-narrowing.json` retains the measurement summary and shared validation
records for this run.
