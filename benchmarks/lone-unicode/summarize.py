#!/usr/bin/env python3
"""Report medians of paired timing ratios and same-binary controls."""
import json
import statistics
import sys

for path in sys.argv[1:]:
    result = json.load(open(path))
    rows = [row for row in result['samples'] if not row['warmup']]
    baseline, candidate = result['binaries']
    print(path)
    for case in result['signatures']:
        before = statistics.median(row['times'][baseline][case] for row in rows)
        after = statistics.median(row['times'][candidate][case] for row in rows)
        ratios = [row['times'][candidate][case] / row['times'][baseline][case] for row in rows]
        controls = [row['times']['control'][case] / row['times'][baseline][case] for row in rows]
        print(f'{case}: {before / 1000:.3f} -> {after / 1000:.3f} us; '
              f'paired {100 * (statistics.median(ratios) - 1):+.1f}%; '
              f'control {100 * (statistics.median(controls) - 1):+.1f}%')
