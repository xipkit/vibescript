#!/bin/sh
# Run after all builds, tests and relinks finish; never overlap timing jobs.
set -eu
if [ "$#" -ne 1 ]; then
    echo "usage: $0 BUILD_DIRECTORY (see optimization.md)" >&2
    exit 2
fi
BUILD=$(cd "$1" && pwd)
TOOLS=$(cd "$(dirname "$0")" && pwd)
python3 "$TOOLS/measure.py" "$BUILD/guards.json" "$BUILD/native-guards.json" "head=$BUILD/simd-head-asbuilt.json" "signed=$BUILD/simd-signed-asbuilt.json" --rounds 7 --seed 201 > "$BUILD/native-guards.log" 2>&1
python3 "$TOOLS/measure.py" "$BUILD/placement-cases.json" "$BUILD/two-way.json" "head=$BUILD/placements/head/variants.json" "signed=$BUILD/placements/signed/variants.json" --rounds 3 --seed 202 > "$BUILD/two-way.log" 2>&1
python3 "$TOOLS/measure.py" "$BUILD/placement-cases.json" "$BUILD/four-way.json" "head=$BUILD/placements/head/variants.json" "signed=$BUILD/placements/signed/variants.json" "legacy=$BUILD/placements/base/variants.json" "original=$BUILD/placements/current/variants.json" --rounds 2 --seed 203 > "$BUILD/four-way.log" 2>&1
python3 "$TOOLS/measure.py" "$BUILD/guards.json" "$BUILD/portable-guards.json" "head=$BUILD/portable-head-asbuilt.json" "signed=$BUILD/portable-signed-asbuilt.json" "legacy=$BUILD/portable-legacy-asbuilt.json" --rounds 7 --seed 204 > "$BUILD/portable-guards.log" 2>&1
python3 "$TOOLS/measure.py" "$BUILD/guards.json" "$BUILD/wasi-guards.json" "head=$BUILD/wasi-head-asbuilt.json" "signed=$BUILD/wasi-signed-asbuilt.json" "legacy=$BUILD/wasi-legacy-asbuilt.json" --rounds 7 --seed 205 > "$BUILD/wasi-guards.log" 2>&1
python3 "$TOOLS/measure.py" "$BUILD/escape-cases.json" "$BUILD/escape-sweep.json" "head=$BUILD/placements/head/variants.json" "signed=$BUILD/placements/signed/variants.json" --rounds 2 --seed 206 > "$BUILD/escape-sweep.log" 2>&1
