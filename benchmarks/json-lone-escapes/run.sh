#!/usr/bin/env bash
# Build both sources before measuring isolated write_string placements on ARM64 macOS.
set -euo pipefail
if [[ $# != 3 ]]; then
  echo 'usage: run.sh BASE_TREE HEAD_TREE BUILD' >&2
  exit 2
fi
base_tree=$(cd "$1" && pwd -P)
head_tree=$(cd "$2" && pwd -P)
mkdir -p "$3"
build=$(cd "$3" && pwd -P)
tools="$head_tree/benchmarks/mixed-text-placement"
isolation="$head_tree/benchmarks/escape-layout/isolate.py"

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
  echo 'This experiment requires native ARM64 macOS.' >&2
  exit 1
fi
{
  uname -a
  sw_vers
  sysctl machdep.cpu.brand_string hw.ncpu hw.memsize
  rustc --version --verbose
  cc --version
  git -C "$base_tree" rev-parse HEAD
  git -C "$head_tree" rev-parse HEAD
} > "$build/environment.txt"

# Virtualized runners may not expose these counters. Fail before making builds
# or treating a zero as an instruction-count result.
/usr/bin/time -l /usr/bin/true 2> "$build/counter-probe.txt"
python3 - "$build/counter-probe.txt" <<'PY'
import pathlib
import re
import sys

probe = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(\d+)\s+instructions retired', probe)
if match is None or int(match[1]) <= 0:
    raise SystemExit('macOS runner does not expose positive retired-instruction counts; see counter-probe.txt')
PY

python3 - "$build/cases.json" <<'PY'
import json
import pathlib
import sys

cases = []
for label, text in [('lines_4k', ('a' * 59 + '\n') * 69),
                    ('unicode_4k', 'é界🙂' * 456),
                    ('lone_escapes_4k', 'ab\n' * 1366)]:
    argument = {'id': 7, 'payload': text}
    cases.append({'name': label,
                  'source': 'def run(input: { id: int, payload: string }) -> string\nJSON.stringify(input)\nend',
                  'args': [argument], 'accounting': False, 'iterations': 6000,
                  'expected': json.dumps(argument, ensure_ascii=False, separators=(',', ':'), sort_keys=True)})
pathlib.Path(sys.argv[1]).write_text(json.dumps(cases, ensure_ascii=False) + '\n')
PY

for mode in simd portable; do
  for label in base head; do
    tree=$base_tree
    if [[ $label == head ]]; then tree=$head_tree; fi
    directory="$build/$mode-$label"
    mkdir -p "$directory/tmp" "$directory/link"
    args=()
    if [[ $mode == portable ]]; then args+=(--no-default-features); fi
    (
      cd "$tree"
      TMPDIR="$directory/tmp" VIBE_LINK_CAPTURE="$directory/link" \
        CARGO_TARGET_DIR="$directory/target" \
        cargo rustc --locked --release --example compare "${args[@]}" -- \
          -C "linker=$tools/capture_link.py"
    ) > "$directory/build.log" 2>&1
    cp "$directory/target/release/examples/compare" "$directory/compare"
    python3 "$isolation" "$directory/link/link.json" "$directory/compare" \
      "$directory/placements" string
    otool -tvV "$directory/compare" > "$directory/assembly.txt"
  done
done

python3 - "$build" <<'PY'
import json
import pathlib
import sys

build = pathlib.Path(sys.argv[1])
groups = {f'{mode}-{label}': json.loads((build / f'{mode}-{label}/placements/variants.json').read_text())
          for mode in ['simd', 'portable'] for label in ['base', 'head']}
(build / 'groups.json').write_text(json.dumps(groups, indent=2) + '\n')
for mode in ['simd', 'portable']:
    # The shared summary expects local/ keys. Only the writer moves here.
    for label in ['base', 'head']:
        variants = {'local/' + phase: entry for phase, entry in groups[f'{mode}-{label}'].items()}
        (build / f'{mode}-{label}.json').write_text(json.dumps(variants, indent=2) + '\n')
PY

python3 "$head_tree/benchmarks/escape-layout/validate.py" \
  "$build/cases.json" "$build/groups.json" "$build/validation.json"
for mode in simd portable; do
  binaries=()
  for label in base head; do
    for phase in {00..31}; do
      binaries+=("$label-$phase=$build/$mode-$label/placements/$phase/compare")
    done
  done
  python3 "$tools/count.py" "$build/cases.json" "$build/$mode-counts.json" "${binaries[@]}"
done

# No compilation, linking or instruction sampling overlaps timed rounds.
for seed in 187 188; do
  for mode in simd portable; do
    python3 "$tools/measure.py" "$build/cases.json" "$build/$mode-$seed.json" \
      "base=$build/$mode-base.json" "head=$build/$mode-head.json" --rounds 15 --seed "$seed"
    python3 "$tools/summarize.py" "$build/$mode-$seed.json" > "$build/$mode-$seed-summary.txt"
    cat "$build/$mode-$seed-summary.txt"
  done
done
