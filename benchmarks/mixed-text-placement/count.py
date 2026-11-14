#!/usr/bin/env python3
"""Estimate retired instructions per call from differential macOS process counts."""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import tempfile

# `compare` warms up with min(iterations, 32) untimed calls. Both samples must
# run at least this many calls, or the 3N process gets more warmup calls than
# the N process and the difference overstates the per-call count.
WARMUP = 32


def sample_size(iterations: int) -> int:
    """Return N for a case; the counts come from N and 3N calls."""
    return min(500, max(WARMUP, iterations // 4))


def count(fixtures: pathlib.Path, binaries: dict[str, str], output: pathlib.Path,
          runs: int, calls: int | None = None) -> None:
    """Capture N/3N process counts with identical warmups and retain every estimate."""
    if calls is not None and calls < WARMUP:
        raise ValueError(f'calls must be at least {WARMUP}')
    cases = json.loads(fixtures.read_text())
    results = []
    with tempfile.TemporaryDirectory(dir=output.parent, prefix='counts-') as temporary:
        fixture = pathlib.Path(temporary) / 'case.json'
        for case in cases:
            n = sample_size(case['iterations']) if calls is None else calls
            for label, binary in binaries.items():
                for repeat in range(runs):
                    counts = {}
                    for iterations in ([n, 3 * n] if repeat % 2 == 0 else [3 * n, n]):
                        fixture.write_text(json.dumps([dict(case, iterations=iterations)]) + '\n')
                        run = subprocess.run(['/usr/bin/time', '-l', binary, str(fixture), '0', 'timing'],
                                             capture_output=True, text=True, check=True)
                        match = re.search(r'(\d+)\s+instructions retired', run.stderr)
                        if not match:
                            raise RuntimeError('macOS did not report retired instructions')
                        counts[iterations] = int(match[1])
                    results.append({'case': case['name'], 'binary': label, 'repeat': repeat,
                                    'n': n, 'counts': counts,
                                    'instructions_per_call': (counts[3 * n] - counts[n]) / (2 * n)})
                print(case['name'], label, flush=True)
                output.write_text(json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('binaries', nargs='+', help='LABEL=executable')
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--calls', type=int, help='N calls in the smaller process; larger process uses 3N')
    options = parser.parse_args()
    if options.runs < 1:
        parser.error('runs must be positive')
    if options.calls is not None and options.calls < WARMUP:
        parser.error(f'calls must be at least {WARMUP}')
    count(options.fixtures, dict(v.split('=', 1) for v in options.binaries),
          options.output.resolve(), options.runs, options.calls)
