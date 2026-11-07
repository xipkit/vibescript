#!/usr/bin/env python3
"""Time matched placements in shuffled pairs; reject load and accounting drift."""
import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess
import time

FIELDS = ('digest', 'steps', 'tracked_peak_bytes', 'tracked_retained_bytes')


def measure(fixtures, variants, output, rounds, limit, seed, warmup_rounds):
    rng = random.Random(seed)
    placements = list(next(iter(variants.values())))
    if any(set(group) != set(placements) for group in variants.values()):
        raise ValueError('variants must have matching placement keys')
    baseline = next(iter(variants))
    result = {'samples': [], 'rejected': [], 'signatures': {}, 'variants': variants,
              'fixtures_sha256': hashlib.sha256(fixtures.read_bytes()).hexdigest(),
              'seed': seed, 'rounds': rounds, 'warmup_rounds': warmup_rounds, 'load_limit': limit}
    for round_number in range(rounds + warmup_rounds):
        rng.shuffle(placements)
        for placement in placements:
            deadline = time.monotonic() + 600
            while True:
                if time.monotonic() > deadline:
                    raise RuntimeError('ten minutes without an accepted placement pair')
                if os.getloadavg()[0] >= limit:
                    time.sleep(10)
                    continue
                labels = list(variants) + ['control']
                rng.shuffle(labels)
                samples, loads = {}, [os.getloadavg()[0]]
                for label in labels:
                    entry = variants[baseline if label == 'control' else label][placement]
                    command = entry['command'] if 'command' in entry else [entry['path']]
                    run = subprocess.run([*command, str(fixtures), '0', 'timing'],
                                         capture_output=True, text=True, check=True)
                    rows = [json.loads(line) for line in run.stdout.splitlines()]
                    expected_names = {case['name'] for case in json.loads(fixtures.read_text())}
                    if len(rows) != len(expected_names) or {r['name'] for r in rows} != expected_names:
                        raise ValueError('missing or duplicate benchmark result')
                    for row in rows:
                        signature = [row[field] for field in FIELDS]
                        expected = result['signatures'].setdefault(row['name'], signature)
                        if signature != expected:
                            raise ValueError(f"accounting mismatch: {label}/{placement}/{row['name']}")
                    samples[label] = {r['name']: r['ns_per_call'] for r in rows}
                    loads.append(os.getloadavg()[0])
                record = {'round': round_number, 'placement': placement,
                          'order': labels, 'loads': loads, 'times': samples}
                if max(loads) >= limit:
                    result['rejected'].append(record)
                    output.write_text(json.dumps(result, indent=2) + '\n')
                    continue
                result['samples'].append(record)
                output.write_text(json.dumps(result, indent=2) + '\n')
                break
        print(f'round {round_number + 1}: {len(placements)} placement pairs, accounting matched', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('variants', nargs='+', help='LABEL=variants.json; first label is baseline')
    parser.add_argument('--rounds', type=int, default=6)
    parser.add_argument('--warmup-rounds', type=int, default=1)
    parser.add_argument('--load-limit', type=float, default=6)
    parser.add_argument('--seed', type=int, default=199)
    options = parser.parse_args()
    if options.rounds < 1 or options.warmup_rounds < 0 or options.load_limit <= 0:
        parser.error('rounds and load limit must be positive; warmup rounds cannot be negative')
    groups = {}
    for entry in options.variants:
        label, path = entry.split('=', 1)
        if label in groups or label == 'control':
            parser.error('variant labels must be unique and cannot be control')
        groups[label] = json.loads(pathlib.Path(path).read_text())
    measure(options.fixtures.resolve(), groups, options.output.resolve(),
            options.rounds, options.load_limit, options.seed, options.warmup_rounds)
