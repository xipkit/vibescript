#!/usr/bin/env python3
"""Confirm escaped-ASCII placement sensitivity with a fresh four-byte front sweep."""
import argparse
import json
import pathlib

from measure import measure
from relink import run as relink


def run(build, head_link, signed_link):
    variants = {}
    for label, link, directory in [('head', head_link, 'current'),
                                   ('signed', signed_link, 'signed')]:
        output = build / 'phase4' / label
        relink(link, build / directory / 'compare', output, 32, 4)
        all_placements = json.loads((output / 'variants.json').read_text())
        variants[label] = {key: value for key, value in all_placements.items()
                           if key.startswith('front/')}
        (build / 'phase4' / f'{label}-front.json').write_text(json.dumps(variants[label]) + '\n')
    cases = [case for case in json.loads((build / 'guards.json').read_text())
             if case['name'] == 'json/stringify_esc_prose_500k/unlimited']
    if len(cases) != 1 or not cases[0]['args'][0]['payload'].isascii():
        raise ValueError('expected the original escaped-ASCII fixture')
    fixtures = build / 'phase-cases.json'
    fixtures.write_text(json.dumps(cases, ensure_ascii=False) + '\n')
    measure(fixtures, variants, build / 'phase4.json', 3, 6, 207, 1)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    parser.add_argument('head_link', type=pathlib.Path)
    parser.add_argument('signed_link', type=pathlib.Path)
    args = parser.parse_args()
    run(args.build.resolve(), args.head_link.resolve(), args.signed_link.resolve())
