#!/usr/bin/env python3
"""Alternate two- and four-variant cohorts at the same counts and placements."""
import argparse
import json
import pathlib

from measure import measure


def run(fixtures, output, variants):
    output.mkdir(parents=True, exist_ok=True)
    selected = [f'{mode}/{index:02}' for mode in ['local', 'front'] for index in [1, 16, 32]]
    variants = {label: {key: group[key] for key in selected} for label, group in variants.items()}
    for phase, cohort in enumerate('ABBAABBA'):
        labels = ['base', 'candidate'] if cohort == 'A' else ['base', 'base-short', 'no-short', 'candidate']
        measure(fixtures, {label: variants[label] for label in labels},
                output / f'{phase + 1:02}-{cohort}.json', 1, 6, 300 + phase, 0)
        print(f'phase {phase + 1}: {cohort}', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('variants', nargs='+', help='LABEL=variants.json for all four variants')
    options = parser.parse_args()
    groups = {}
    for value in options.variants:
        label, path = value.split('=', 1)
        groups[label] = json.loads(pathlib.Path(path).read_text())
    if set(groups) != {'base', 'base-short', 'no-short', 'candidate'}:
        parser.error('supply base, base-short, no-short and candidate')
    run(options.fixtures.resolve(), options.output.resolve(), groups)
