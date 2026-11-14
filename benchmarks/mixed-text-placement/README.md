# Repeated mixed-text stringify investigation

The follow-up [VIBE-200 optimization](optimization.md) folds NEON byte
classification while retaining the short-run shortcut. Its report and raw
evidence keep the new two-way, four-way and cross-target results separate
from the VIBE-199 investigation below.

VIBE-199 compares the scanner before VIBE-180 with its corrected short-run
candidate. See [the findings and measurements](results.md). This directory contains diagnostic tools and the four exact focused
fixtures; it does not change the runtime or its build flags.

The measured sources are:

| Label | Source | Short-run shortcut |
|---|---|---|
| `base` | `79a9cee36fc5613c9e6673c0cfc9964bdf83189c` | absent |
| `base-short` | same baseline, diagnostic `text_span` edit | present |
| `no-short` | `d9e3b9e7d79a561df374c3ecac0f3f14919a822a`, diagnostic edit | absent |
| `candidate` | same candidate, unmodified | present |

`ablate.py TREE on|off` adds or removes precisely the candidate's shortcut.
The candidate also changes the shared ASCII scanner and its two callers; it is
not a one-instruction mask substitution. The integrated `write_text` assembly
and the two within-source shortcut comparisons distinguish the relevant paths.

## Reproduction on Apple Silicon

Use Rust 1.98.1 and the normal release profile: opt-level 3, thin LTO, one
codegen unit, debug level 1, overflow checks enabled, default SIMD feature.
Create disposable detached worktrees at the SHAs above. Keep generated objects,
binaries and logs outside the checkout. The following commands use absolute
paths `TREE`, `BUILD`, and `TOOLS` (this directory).

Build each variant, using a separate build directory and a clean source tree:

```sh
mkdir -p "$BUILD/tmp" "$BUILD/link"
TMPDIR="$BUILD/tmp" VIBE_LINK_CAPTURE="$BUILD/link" CARGO_TARGET_DIR="$BUILD/target" \
  cargo rustc --manifest-path "$TREE/Cargo.toml" --locked --release --example compare -- \
  -C "linker=$TOOLS/capture_link.py"
cp "$BUILD/target/release/examples/compare" "$BUILD/compare"
python3 "$TOOLS/relink.py" "$BUILD/link/link.json" "$BUILD/compare" "$BUILD/placements"
```

The linker wrapper copies all input object/archive files before Rust removes
its temporary files. The saved JSON argument vector is replayed directly,
without shell parsing. These tools require the macOS linker, `cc`, and `nm`.

Each variant gets **32 isolated placements**, in four-byte increments across
128 bytes, and **32 front-pad placements**, in sixteen-byte increments across
512 bytes. The isolated variant orders `pad_a`, `write_text`, `pad_b` first;
the two pads have a constant combined size. Every other symbol must keep its
address. The front variant orders just the pad first; every text symbol must
move sixteen bytes per increment. Both assertions run on every relink, and
`variants.json` records the actual writer address. Use `--front-step 4` for
four-byte front increments; the same audit rejects
any alignment rounding. Its fifteenth placement adds 60 bytes, the offset
used for the preserved-binary layout control. These are two separate
experiments; front padding does not isolate the writer from other hot code.

Run the four variants interleaved, with the first binary repeated as a
same-binary control in every matched placement group. Set `BASE`, `BASE_SHORT`,
`NO_SHORT`, and `CANDIDATE` to the respective `placements/variants.json` paths.
To reproduce the recorded sequence, first make the shorter diagnostic fixture:

```sh
python3 - "$TOOLS/cases.json" "$BUILD/sweep-cases.json" <<'PYTHON'
import json, pathlib, sys
cases = json.loads(pathlib.Path(sys.argv[1]).read_text())
for case in cases:
    case['iterations'] //= 4
pathlib.Path(sys.argv[2]).write_text(json.dumps(cases, ensure_ascii=False) + '\n')
PYTHON
python3 "$TOOLS/measure.py" "$BUILD/sweep-cases.json" "$BUILD/sweep.json" \
  "base=$BASE" "base-short=$BASE_SHORT" "no-short=$NO_SHORT" "candidate=$CANDIDATE" --rounds 6
python3 "$TOOLS/measure.py" "$TOOLS/cases.json" "$BUILD/long-sweep.json" \
  "base=$BASE" "candidate=$CANDIDATE" --rounds 3 --warmup-rounds 0 --seed 200
python3 "$TOOLS/measure.py" "$TOOLS/cases.json" "$BUILD/long-ablations.json" \
  "base=$BASE" "base-short=$BASE_SHORT" "no-short=$NO_SHORT" "candidate=$CANDIDATE" \
  --rounds 2 --warmup-rounds 0 --seed 201
python3 "$TOOLS/summarize.py" "$BUILD/long-sweep.json"
python3 "$TOOLS/summarize.py" "$BUILD/long-ablations.json"
```

The four fixtures are the original script-facing `JSON.stringify` calls:
fixed eight-byte words (4 KB), random 1–15-byte ASCII runs (40 KB), prose-like
runs (40 KB), and shuffled French (40 KB). They retain the card's longer
confirmation counts (32,000 / 8,000 / 8,000 / 4,800). Only the initial diagnostic
sweep divides these counts by four. The two-way and four-way comparisons give
different magnitudes even at full counts; retain both and do not pool them.
See the report for the execution-context control. The source, arguments, and
metering options remain identical in all sweeps.

`measure.py` defaults to one complete warmup round before measured rounds.
This avoids the first-invocation artifact seen on newly linked executables,
particularly on the first, small fixed-word case. Warmup samples stay in the
raw file but are excluded by `summarize.py`. The two later commands skip an
additional warmup because these exact executables have already completed the
first sweep; use a warmup when running either command alone.

`measure.py` randomizes placement order and variant order with seed 199 by default. It
checks output digest, steps, peak tracked memory, and retained tracked memory
on every invocation. A missing case or accounting difference aborts the run.
It rejects complete placement groups when any sampled one-minute load is at
least 6 and stops after ten minutes without an accepted group. It saves raw
accepted and rejected samples, execution order, loads, fixture hash and
addresses after each group. The existing compare harness warms up 32 calls,
checks the final timed output against its initial result, and excludes setup
and result encoding from elapsed time.

Report the median and range of per-placement minima, plus median matched
ratios. Do not choose one best address or mix local and front samples.

`count.py FIXTURES OUTPUT LABEL=EXECUTABLE...` estimates retired instructions
per call using `/usr/bin/time -l` at N and 3N calls, alternating order across
three repetitions. N is a quarter of the case's iterations, kept between 32
and 500. The 32-call floor matches `compare`'s warmup, so both processes run
the same warmup calls and the difference cancels them with the fixed
process/setup work. It is an estimate, not a hardware branch-miss or stall
counter. `test_count.py` checks this with a stand-in for `/usr/bin/time`.

For portable comparisons build `compare` with `--no-default-features`. For
WASI build `tests/platforms/wasi/Cargo.toml`'s `vibescript-wasi-compare` for
`wasm32-wasip1`, explicitly applying the root release settings to that separate
workspace. Run with Wasmtime 48.0.2 and a preopened fixture directory. WASI's
witness dependency already disables native SIMD. Keep those results separate
from the native placement experiments.

## Raw evidence and execution-context checks

`evidence.json.gz` contains all accepted/rejected samples, warmup metadata,
orders, loads, signatures, per-placement addresses, source/binary hashes,
retired-instruction estimates, the five cross-target fixtures, and the
profiling limitation. It includes the original shorter sweep as well as the
longer confirmations; none of those samples are silently discarded.

For example, extract and summarize a recorded confirmation:

```sh
python3 - "$TOOLS/evidence.json.gz" "$BUILD/recorded.json" <<'PYTHON'
import gzip, json, pathlib, sys
with gzip.open(sys.argv[1], 'rt') as source:
    evidence = json.load(source)
pathlib.Path(sys.argv[2]).write_text(json.dumps(evidence['long-sweep']) + '\n')
PYTHON
python3 "$TOOLS/summarize.py" "$BUILD/recorded.json"
```

`cohorts.py` alternates two-way and four-way groups in `ABBAABBA` order at six
fixed placements, with unchanged full-count fixtures. Run it only after the
binaries are warmed, without overlapping other benchmark jobs:

```sh
python3 "$TOOLS/cohorts.py" "$TOOLS/cases.json" "$BUILD/cohorts" \
  "base=$BASE" "base-short=$BASE_SHORT" "no-short=$NO_SHORT" "candidate=$CANDIDATE"
```

The raw archive keys `bridge` and `bridge-audit` cover the additional
60-byte front-pad check. `profiling` records why the attempted Instruments
counter captures were excluded: only startup intervals were exported, not
usable coverage of the measured scan. Retired instruction estimates come
from `time -l`; they are distinct from those unusable captures.
