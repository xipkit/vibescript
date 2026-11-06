# Lazy-match design baseline

Supporting evidence for [VIBE-196's design](../docs/lazy-match-data.md), measured
on unchanged runtime commit `4cb18cb4ab042a0968eed3cc5303b7da9cdbc1b8`.
This is a design baseline, **not a before/after optimization result**. The
implementation and candidate measurements belong to VIBE-128.

Machine: Apple M4, macOS 27.0.1 (26A434), arm64, Homebrew Rust 1.98.1
(`48a229cea`, 2026-09-01). Release profile, default SIMD, metered execution.
The timing binary has no allocation instrumentation; a separate
`allocation-stats` build counts allocator calls/bytes over 100 calls. Eight
rounds of 500 calls provide the timing median and min–max range below. Each
round uses the harness's 32-call warmup. These are sequential baseline samples,
not balanced baseline/candidate trials; shared-machine noise is possible.

All eight cases ran through the real `Engine` and matched independently
computed output arrays before measurement. The original `regex_log_lines`
fixture is taken verbatim from `scripts/fixtures.py`: 128 lines, 102 matches,
26 invalid lines. The other cases use exactly the same inputs. Source, inputs,
expected results, counters, timings and binary/fixture hashes are in
[baseline.json](results/lazy-match-design/baseline.json).

| Workload | Allocations/call | Allocated bytes/call | Median µs (range) | Steps | Tracked peak / retained bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| regex_log_lines | 5,558 | 474,974 | 402.566 (400.051–406.187) | 41,165 | 79,212 / 55,768 |
| regex_unread | 5,532 | 472,270 | 387.331 (386.350–389.362) | 40,169 | 26,096 / 2,152 |
| regex_index0 | 5,532 | 472,270 | 394.597 (390.748–403.004) | 41,905 | 26,096 / 13,182 |
| regex_index1 | 5,532 | 472,270 | 395.533 (393.168–408.640) | 41,905 | 37,812 / 13,984 |
| regex_repeat_index | 5,532 | 472,270 | 412.460 (407.647–427.777) | 45,475 | 37,812 / 13,984 |
| regex_whole_store | 5,539 | 476,678 | 410.301 (408.035–430.274) | 42,813 | 275,781 / 13,984 |
| regex_unread_groupless | 4,690 | 380,686 | 340.452 (338.509–345.243) | 34,301 | 24,998 / 2,152 |
| regex_predicate | 1,936 | 216,768 | 265.056 (264.608–268.663) | 32,397 | 23,127 / 2,152 |

Unread, `[0]`, `[1]` and repeated `[1]` have identical allocator counts/bytes:
field selection cannot recover eager construction work. The captures fixture
adds 26 empty arrays for the invalid lines. Removing groups saves 842 of 5,532
allocations in this current unread workload (15.2%); most cost remains. This
supports whole-structure deferral, but is not an attribution of every remaining
allocation to match data: search, VM and input import also allocate.

`regex_whole_store` stores every match in a language array before reading group
one. It tests the proposed mandatory aggregate normalization and its retention
cost. `regex_predicate` is the already-specialized `match?` control; it is not a
measurement of a lazy-match candidate. The original three-group fixture differs
from the issue's historical 6,782 allocations / about 508 KB because the runtime
has changed. Do not compare candidate numbers against that historical binary.

Three additional executed semantic probes confirm:

- `[m == m, m == m.dup, m == separately_created_match]` is `[true, true, false]`,
  and interpolation renders `"a"` for the fixture.
- Script JSON of a complete match raises
  `JSON.stringify key "begin": JSON.stringify unsupported value type builtin`.
  The outer JSON layer adds context to the writer's rejection; this is not the
  writer's raw `cannot encode a builtin` message.
- A nested block reads its captured match local and returns `["a"]`.

The initial JSON expectation used the lower-level writer message; running the
probe exposed the outer error wording, which is recorded above and in the design.
These checks validate current semantics only. The proposed lazy implementation
has not been built or tested. No golden counters were re-recorded.

## Reproduce

From this checkout, put binaries/logs outside the source tree. Generate harness
fixtures from the evidence; the result need not byte-match the original
pretty-printed fixture to execute identical cases:

```sh
mkdir -p /Volumes/AI/Build/tron-vibe-196
python3 - <<'PYCODE'
import json
from pathlib import Path
report = json.loads(Path("benchmarks/results/lazy-match-design/baseline.json").read_text())
cases = [dict(name=c["name"], source=c["source"], args=[report["input"]],
              expected=c["expected"], accounting=True) for c in report["cases"]]
Path("/Volumes/AI/Build/tron-vibe-196/replay.json").write_text(json.dumps(cases) + "\n")
PYCODE
CARGO_TARGET_DIR=/Volumes/AI/Build/tron-vibe-196/target cargo build --release --locked --example compare --features allocation-stats
cp /Volumes/AI/Build/tron-vibe-196/target/release/examples/compare /Volumes/AI/Build/tron-vibe-196/compare-alloc
/Volumes/AI/Build/tron-vibe-196/compare-alloc /Volumes/AI/Build/tron-vibe-196/replay.json 100 alloc
CARGO_TARGET_DIR=/Volumes/AI/Build/tron-vibe-196/target cargo build --release --locked --example compare
/Volumes/AI/Build/tron-vibe-196/target/release/examples/compare /Volumes/AI/Build/tron-vibe-196/replay.json 1 validate
/Volumes/AI/Build/tron-vibe-196/target/release/examples/compare /Volumes/AI/Build/tron-vibe-196/replay.json 500 timing
```

The harness verifies repeated timing results against its initial result; also
compare each validation record's decoded `result_json` with the fixture's
`expected` field. Repeat timing eight times for the baseline. The implementation
PR needs balanced before/after trials, portable/unlimited and x86 coverage,
long-subject and host-export workloads, and RSS as specified in the design.
