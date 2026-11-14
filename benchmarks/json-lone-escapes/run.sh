#!/usr/bin/env bash
# Build both sources before measuring independent writer/helper placements on ARM64 macOS.
set -euo pipefail
prepare_only=false
if [[ ${1:-} == --prepare-only ]]; then
  prepare_only=true
  shift
fi
if [[ $# != 3 ]]; then
  echo 'usage: run.sh [--prepare-only] BASE_TREE HEAD_TREE BUILD' >&2
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
  python3 --version
  cc --version
  git -C "$base_tree" rev-parse HEAD
  git -C "$head_tree" rev-parse HEAD
} > "$build/environment.txt"
if $prepare_only; then
  echo 'Placement and output/accounting validation only; no instruction counts or timings.' > "$build/scope.txt"
else
  echo 'Full placement validation, instruction counts and timing experiment.' > "$build/scope.txt"
fi

# Virtualized runners may not expose these counters. Fail before making builds
# or treating a zero as an instruction-count result.
/usr/bin/time -l /usr/bin/true 2> "$build/counter-probe.txt"
if ! $prepare_only; then
python3 - "$build/counter-probe.txt" <<'PY'
import pathlib
import re
import sys

probe = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(\d+)\s+instructions retired', probe)
if match is None or int(match[1]) <= 0:
    raise SystemExit('macOS runner does not expose positive retired-instruction counts; see counter-probe.txt')
PY
fi

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
validation = [*cases, *[dict(case, name=case['name'] + '/metered', accounting=True) for case in cases]]
pathlib.Path(sys.argv[1]).with_name('validation-cases.json').write_text(
    json.dumps(validation, ensure_ascii=False) + '\n')
PY

for mode in simd portable; do
  for label in base head; do
    tree=$base_tree
    if [[ $label == head ]]; then tree=$head_tree; fi
    directory="$build/$mode-$label"
    mkdir -p "$directory/tmp" "$directory/link"
    (
      cd "$tree"
      set -- rustc --locked --release --example compare
      if [[ $mode == portable ]]; then set -- "$@" --no-default-features; fi
      TMPDIR="$directory/tmp" VIBE_LINK_CAPTURE="$directory/link" \
        CARGO_TARGET_DIR="$directory/target" \
        cargo "$@" -- \
          -C "linker=$tools/capture_link.py"
    ) > "$directory/build.log" 2>&1
    cp "$directory/target/release/examples/compare" "$directory/compare"
    for target in string run; do
      python3 "$isolation" "$directory/link/link.json" "$directory/compare" \
        "$directory/$target-placements" "$target"
    done
    otool -tvV "$directory/compare" > "$directory/assembly.txt"
  done
done

python3 "$head_tree/benchmarks/json-lone-escapes/phases.py" "$build"

python3 "$head_tree/benchmarks/escape-layout/validate.py" \
  "$build/validation-cases.json" "$build/groups.json" "$build/validation.json"
if $prepare_only; then
  cat "$build/scope.txt"
  exit 0
fi
for target in string run; do
  for mode in simd portable; do
    python3 - "$build" "$tools" "$mode" "$target" <<'PY'
import json
import pathlib
import sys

build, tools = map(pathlib.Path, sys.argv[1:3])
mode, target = sys.argv[3:]
sys.path.insert(0, str(tools))
from count import count

binaries = {}
for label in ['base', 'head']:
    variants = json.loads((build / f'{mode}-{label}-{target}.json').read_text())
    binaries.update({label + '-' + key.removeprefix('local/'): entry['path']
                     for key, entry in variants.items()})
count(build / 'cases.json', binaries, build / f'{mode}-{target}-counts.json', runs=3, calls=3000)
PY
  done
done

# No compilation, linking or instruction sampling overlaps timed rounds.
for seed in 187 188; do
  for target in string run; do
    for mode in simd portable; do
      name="$mode-$target-$seed"
      python3 "$tools/measure.py" "$build/cases.json" "$build/$name.json" \
        "base=$build/$mode-base-$target.json" "head=$build/$mode-head-$target.json" --rounds 15 --seed "$seed"
      python3 "$tools/summarize.py" "$build/$name.json" > "$build/$name-summary.txt"
      python3 "$head_tree/benchmarks/escape-layout/summarize.py" "$build/$name.json" \
        > "$build/$name-phases.json"
      cat "$build/$name-summary.txt"
    done
  done
done
