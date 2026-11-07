#!/usr/bin/env python3
"""Archive completed VIBE-200 experiments, including rejected and warmup samples."""
import argparse
import gzip
import hashlib
import json
import pathlib


def archive(build, destination):
    evidence = {}
    for key in ['iterations', 'probe-signed', 'native-guards', 'two-way', 'four-way',
                'portable-guards', 'wasi-guards', 'escape-sweep', 'validation', 'environment', 'code-hashes',
                'test-summary', 'handoff']:
        evidence[key] = json.loads((build / f'{key}.json').read_text())
    for key in ['probe-signed', 'native-guards', 'two-way', 'four-way', 'portable-guards', 'wasi-guards', 'escape-sweep']:
        data = evidence[key]
        placements = set(next(iter(data['variants'].values())))
        expected = {(r, p) for r in range(data['rounds'] + data['warmup_rounds']) for p in placements}
        actual = [(r['round'], r['placement']) for r in data['samples']]
        if len(actual) != len(expected) or set(actual) != expected:
            raise ValueError(f'incomplete or duplicate groups: {key}')
        if any(max(r['loads']) >= data['load_limit'] for r in data['samples']):
            raise ValueError(f'accepted load violation: {key}')
    evidence['placement_fixtures'] = json.loads((build / 'placement-cases.json').read_text())
    evidence['probe_fixtures'] = json.loads((build / 'probe-cases.json').read_text())
    evidence['disassembly'] = {label: (build / label / 'write_text.s').read_text()
                               for label in ['current', 'signed']}
    evidence['rebuild_audit'] = json.loads((build / 'signed/rebuild-audit.json').read_text())
    paths = {entry['path'] for key in ['two-way', 'four-way']
             for group in evidence[key]['variants'].values() for entry in group.values()}
    evidence['placement_binary_sha256'] = {
        name: hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() for name in sorted(paths)}
    paths = {command[-1] for variants in evidence['validation']['commands'].values()
             for command in variants.values()}
    evidence['as_built_binary_sha256'] = {
        name: hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() for name in sorted(paths)}
    with gzip.GzipFile(destination, 'wb', mtime=0) as output:
        output.write(json.dumps(evidence, ensure_ascii=False).encode() + b'\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    archive(args.build.resolve(), args.output.resolve())
