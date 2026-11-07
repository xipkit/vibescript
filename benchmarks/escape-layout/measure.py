#!/usr/bin/env python3
"""Run independent complete-cycle confirmation cohorts."""
import argparse
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'mixed-text-placement'))
from measure import measure


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    parser.add_argument('--seeds', nargs='+', type=int, default=[223, 227])
    parser.add_argument('--rounds', type=int, default=2)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error('rounds must be positive')
    build = args.build.resolve()
    for seed in args.seeds:
        measure(build / 'cases.json', json.loads((build / 'groups.json').read_text()),
                build / f'confirm-{seed}.json', args.rounds, 6, seed, 1)
