# Escape-heavy JSON placement investigation (VIBE-201)

This is a diagnostic experiment, not a runtime optimization. No alignment pad,
trampoline, compiler flag or JSON implementation change ships in the runtime.
See [the measured results](results.md) and [raw evidence](evidence.json.gz).

## Controlled inputs

The experiment uses VIBE-200's preserved `head` (`9ead14d5`) and `signed`
(`641fb461`) executables and captured linker objects. They were built on OTIS
with Rust 1.98.1, release opt-level 3, thin LTO, one codegen unit, debug level 1,
overflow checks and default SIMD enabled. The preservation and source history
are in [PR 1427](https://github.com/xipkit/vibescript/pull/1427).

The input remains the original `json/stringify_esc_prose_500k/unlimited` case:
500,016 ASCII payload bytes, 240 measured calls, and 32 warmup calls per process.
Each experiment also starts with one full warmup cycle. The exact fixture is
in [cases.json.gz](cases.json.gz). No input bytes, source, iteration counts or
accounting options change between placements.

`isolate.py` replays the captured linker argument vector with an order file:
`pad_a`, selected function, `pad_b`, then all remaining code. The two pads total
256 bytes; `pad_a` varies from 4 to 128 in four-byte steps. Every step must move
only the selected symbol by four bytes, leaving **every other original symbol**
fixed, including data symbols. This applies within a sweep: moving a function
to the front initially changes other addresses relative to the as-built binary.
The same-binary as-built reference is therefore a noise control, not proof
that all differences between whole cohorts come from the four-byte movement.

The selected functions are `write_string` (including its inlined `prefix`),
`run_prefix`, and `Output::put_run`. There is no separately linked `prefix`
function. `write_text` is not a placement target because the ASCII fixture
bypasses it. The signed build gets its own isolated `write_string` sweep.

Two additional diagnostic cohorts test the ordinary-run copy call:

- **Anchored:** move `write_string`, but route its ordinary-run copy through a
  fixed two-instruction island: `BL memcpy_stub; B original_continuation`.
- **Moving:** fix the writer, all other functions and data; move just that
  island through all 32 four-byte phases inside unused padding.

`copy_return.py` replaces the original `BL` with `B island`. It checks the
original instruction, the `_memcpy` import binding, NOP space and branch range,
then signs the diagnostic executable ad hoc. The existing function prologue
and epilogue save/restore LR. Copy arguments, output, accounting and surrounding
instructions are unchanged. These controls add two branches; their absolute
overhead is not a proposed optimization. They vary a call/return **route**, not
just one abstract predictor input.

The initial isolation experiment uses seed 211, three measured cycles and a
same-binary reference repeat in each group. Confirmation seeds 223 and 227 each
use two measured cycles, the full seven-variant cohort and the same reference
repeat. Each seed independently shuffles both all phases and invocation order.
Seed 227 had substantial reference timing drift; the noisier run is retained.
An intervening seed-229 run accidentally overlapped the independent instruction
audit in its final cycle. Its complete samples and a protocol-deviation note are
retained, but the entire run is excluded from confirmation inference.
Its optional replacement, seed 233, was stopped incomplete when sustained
system load exceeded the cutoff and Spotlight indexing dominated CPU use.
That whole attempt, including its completed cycles, is archived separately and
excluded. The finding uses the complete independent experiments 211, 223 and 227.
The shared measurement runner retains warmups, all timings, orders, sampled
one-minute loads and rejected groups, and rejects a group if any sampled load
reaches 6. Normal scheduling, PIE and ASLR remain enabled. Builds, relinks and
profiling finish before timings; the system is not otherwise isolated.

Every timed invocation checks digest, steps, peak and retained tracked bytes.
`validate.py` additionally compares the complete output and captured stdout/
stderr across every unique binary. `summarize.py` rejects missing/duplicate
phase groups and reports all phases. Estimates from different seeds remain
separate, and no phase is selected as a fix.

## Reproduce

All large artifacts live under `/Volumes/AI/Build/tron-vibe-201`. Its
`vibe-200-preserved` directory is an independent copy of the parent build,
including captured linker inputs; original copies were verified with SHA-256.
The committed parent reproduction tools were also saved separately before
parent cleanup. This run does not read the old VIBE-200 build path.

Set `BUILD` to the absolute build directory and run from this checkout:

```sh
python3 benchmarks/escape-layout/prepare.py "$BUILD"
python3 benchmarks/escape-layout/validate.py "$BUILD/cases.json" \
  "$BUILD/groups.json" "$BUILD/validation.json"
python3 benchmarks/escape-layout/measure.py "$BUILD"
python3 benchmarks/escape-layout/audit.py "$BUILD"
python3 benchmarks/escape-layout/summarize.py "$BUILD/confirm-223.json"
python3 benchmarks/escape-layout/summarize.py "$BUILD/confirm-227.json"
```

`prepare.py` explicitly rebases all captured VIBE-200 input paths into the
preserved directory before linking. It checks the fixture identity and count.
The copy-call offset `0x6e0` is specific to the preserved head's `write_string`;
the import-binding check fails if that offset is no longer the copy call.
The tools require native macOS ARM64 `cc`, `nm`, `otool` and `codesign`.

Without the preserved artifacts, build the two recorded sources using the
[capture/relink workflow](../mixed-text-placement/README.md), with independent
target directories. Put head in `vibe-200-preserved/current` and signed in
`vibe-200-preserved/signed`; capture their inputs into `link` and `final-link`
respectively. Expand `cases.json.gz` to `vibe-200-preserved/phase-cases.json`.
Fresh compiler/linker versions can change function layout, offsets and the
results; verify the archived fingerprints and disassembly instead of treating
a fresh build as the exact preserved binary.

To repeat the initial four-variant isolation experiment, select `reference`,
`string`, `prefix` and `run` from `groups.json` and pass them to the shared
`mixed-text-placement/measure.py`, using seed 211 and three measured rounds.
Each manifest has matching keys `00` through `31`.

The separate sampling run used the original binary with a fixed 16,000 calls,
and `sample PID 5 1 -file profile.txt`. It establishes which code executes; its
timing is not part of any benchmark estimate. The sampling run overlapped
relinking and should not be treated as an unbiased hotspot percentage.

The archive includes every timing sample, signatures, complete phase tables,
binary/object hashes, first-placement symbol inventories, patch instruction
audits, full-output validation, disassembly and the sampling trace. The older
whole-code phase comparison is retained as source evidence from VIBE-200,
not mixed into the new timing cohort. Binaries and objects stay outside Git.

After measurement exits, archive and plot the results:

```sh
python3 benchmarks/escape-layout/archive.py "$BUILD" "$BUILD/evidence.json.gz"
UV_CACHE_DIR=/Volumes/AI/Cache/uv MPLCONFIGDIR="$BUILD/matplotlib" \
  uv run --with matplotlib==3.11.2 python benchmarks/escape-layout/plot.py \
  "$BUILD/evidence.json.gz" "$BUILD/phases.png"
```

Fresh archives require both confirmation runs, full-output validation and
the instruction audit. Historical pilot/invalidated runs, parent timings and
session-specific sampling/environment/disassembly files are optional and their
absence is listed explicitly. The committed archive includes all of them.
