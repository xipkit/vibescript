#!/usr/bin/env python3
"""Compare paired binaries, retaining raw timings and checking accounting."""
import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess


def measure(args):
    binaries = dict(item.split('=', 1) for item in args.binaries)
    rng = random.Random(188)
    result = dict(samples=[], signatures={}, seed=188,
                  fixtures_sha256=hashlib.sha256(args.fixtures.read_bytes()).hexdigest(),
                  binaries={label: dict(path=path, sha256=hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest())
                            for label, path in binaries.items()})
    expected = {case['name']: case['expected'] for case in json.loads(args.fixtures.read_text())}

    def invoke(path, mode):
        run = subprocess.run([path, str(args.fixtures), '0', mode], capture_output=True, text=True, check=True)
        rows = [json.loads(line) for line in run.stdout.splitlines()]
        assert len(rows) == len(expected) and {row['name'] for row in rows} == set(expected)
        for row in rows:
            signature = [row[key] for key in ('digest', 'steps', 'tracked_peak_bytes', 'tracked_retained_bytes')]
            assert result['signatures'].setdefault(row['name'], signature) == signature, (path, row)
        return rows

    result['validation'] = {}
    for label, path in binaries.items():
        rows = invoke(path, 'validate')
        for row in rows:
            assert json.loads(row['result_json']) == expected[row['name']], (label, row)
        result['validation'][label] = rows
    for round_number in range(args.rounds + 1):
        order = list(binaries) + ['control']
        rng.shuffle(order)
        record = dict(round=round_number, warmup=round_number == 0, order=order, times={}, loads=[])
        for label in order:
            path = binaries[next(iter(binaries)) if label == 'control' else label]
            record['loads'].append(os.getloadavg())
            rows = invoke(path, 'timing')
            record['times'][label] = {row['name']: row['ns_per_call'] for row in rows}
        result['samples'].append(record)
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        print(f'round {round_number}: counts and accounting matched', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('binaries', nargs='+', help='LABEL=executable; first is the control')
    parser.add_argument('--rounds', type=int, default=10)
    measure(parser.parse_args())
