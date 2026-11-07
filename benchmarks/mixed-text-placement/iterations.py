#!/usr/bin/env python3
"""Vary iteration count with a fixed two-way cohort and randomized placement/order."""
import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess
import time

from measure import FIELDS


def run(fixtures, output, variants, rounds, seed):
    rng = random.Random(seed)
    selected = [f'{mode}/{index:02}' for mode in ['local', 'front'] for index in [1, 16, 32]]
    variants = {label: {key: group[key] for key in selected} for label, group in variants.items()}
    cases = json.loads(fixtures.read_text())
    # Only the random/prose cases, with identical original payloads at each count.
    cases = [case for case in cases if case['iterations'] == 8000]
    if len(cases) != 2:
        raise ValueError('expected the original random and prose fixtures')
    output.parent.mkdir(parents=True, exist_ok=True)
    inputs = {}
    for count in [1000, 2000, 8000]:
        path = output.with_name(f'{output.stem}-{count}-cases.json')
        path.write_text(json.dumps([case | {'iterations': count} for case in cases], ensure_ascii=False) + '\n')
        inputs[count] = path.resolve()
    result = {'samples': [], 'rejected': [], 'signatures': {}, 'variants': variants,
              'seed': seed, 'rounds': rounds, 'load_limit': 6,
              'fixtures_sha256': hashlib.sha256(fixtures.read_bytes()).hexdigest(),
              'counts': list(inputs), 'warmup': 'one 8000-call group per placement; 32 calls per process/case'}
    baseline = next(iter(variants))
    warmups = [(placement, 8000) for placement in selected]
    rng.shuffle(warmups)
    for round_number in range(-1, rounds):
        jobs = warmups if round_number == -1 else [(p, n) for p in selected for n in inputs]
        if round_number >= 0:
            rng.shuffle(jobs)
        for placement, count in jobs:
            deadline = time.monotonic() + 600
            while True:
                if time.monotonic() > deadline:
                    raise RuntimeError('ten minutes without an accepted group')
                if os.getloadavg()[0] >= 6:
                    time.sleep(10)
                    continue
                labels = [*variants, 'control']
                rng.shuffle(labels)
                samples, loads = {}, [os.getloadavg()[0]]
                for label in labels:
                    binary = variants[baseline if label == 'control' else label][placement]['path']
                    process = subprocess.run([binary, str(inputs[count]), '0', 'timing'],
                                             check=True, capture_output=True, text=True)
                    rows = [json.loads(line) for line in process.stdout.splitlines()]
                    if len(rows) != len(cases) or {r['name'] for r in rows} != {c['name'] for c in cases}:
                        raise ValueError('missing or duplicate result')
                    for row in rows:
                        signature = [row[field] for field in FIELDS]
                        if result['signatures'].setdefault(row['name'], signature) != signature:
                            raise ValueError(f'accounting mismatch: {label}/{placement}/{count}')
                        if row['iterations'] != count:
                            raise ValueError('wrong iteration count')
                    samples[label] = rows
                    loads.append(os.getloadavg()[0])
                record = {'round': round_number, 'placement': placement, 'iterations': count,
                          'order': labels, 'loads': loads, 'results': samples}
                accepted = max(loads) < 6
                result['samples' if accepted else 'rejected'].append(record)
                output.write_text(json.dumps(result, indent=2) + '\n')
                if accepted:
                    break
        print(f'round {round_number}: {len(jobs)} groups; exact accounting matched', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('variants', nargs=2, help='LABEL=variants.json; baseline first')
    parser.add_argument('--rounds', type=int, default=4)
    parser.add_argument('--seed', type=int, default=200)
    args = parser.parse_args()
    groups = {}
    for entry in args.variants:
        label, path = entry.split('=', 1)
        if label in groups or label == 'control':
            parser.error('labels must be unique and cannot be control')
        groups[label] = json.loads(pathlib.Path(path).read_text())
    if args.rounds < 1:
        parser.error('rounds must be positive')
    run(args.fixtures.resolve(), args.output.resolve(), groups, args.rounds, args.seed)
