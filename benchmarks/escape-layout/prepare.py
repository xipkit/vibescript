#!/usr/bin/env python3
"""Rebase the preserved VIBE-200 linker inputs and prepare VIBE-201 controls."""
import argparse
import hashlib
import json
import pathlib

from copy_return import run as copy_return
from isolate import run as isolate


def prepare(build):
    preserved = build / 'vibe-200-preserved'
    fixture = preserved / 'phase-cases.json'
    cases = json.loads(fixture.read_text())
    if (len(cases) != 1 or cases[0]['name'] != 'json/stringify_esc_prose_500k/unlimited'
            or cases[0]['iterations'] != 240 or not cases[0]['args'][0]['payload'].isascii()):
        raise ValueError('expected unchanged original ASCII escape fixture and count')
    (build / 'cases.json').write_bytes(fixture.read_bytes())
    groups = {}
    for label, directory, linkdir in [('head', 'current', 'link'), ('signed', 'signed', 'final-link')]:
        args = json.loads((preserved / directory / linkdir / 'link.json').read_text())
        args = [s.replace('/Volumes/AI/Build/tron-vibe-200', str(preserved)) for s in args]
        link = build / f'{label}-link.json'
        link.write_text(json.dumps(args, indent=2) + '\n')
        for target in (['string', 'prefix', 'run'] if label == 'head' else ['string']):
            output = build / 'isolation' / label / target
            isolate(link, preserved / directory / 'compare', output, target)
            groups[target if label == 'head' else label] = json.loads((output / 'variants.json').read_text())
    for mode in ['anchored', 'moving']:
        output = build / 'copy' / mode
        copy_return(build / 'isolation/head/string/variants.json', output, mode, 0x6e0)
        groups[mode] = json.loads((output / 'variants.json').read_text())
    reference = preserved / 'current/compare'
    entry = {'path': str(reference), 'sha256': hashlib.sha256(reference.read_bytes()).hexdigest()}
    groups = {'reference': {key: entry for key in groups['string']}, **groups}
    (build / 'groups.json').write_text(json.dumps(groups, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    args = parser.parse_args()
    prepare(args.build.resolve())
