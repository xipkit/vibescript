#!/usr/bin/env python3
"""Check full output and accounting for every binary in a placement cohort."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'mixed-text-placement'))
from measure import FIELDS


def validate(fixtures, groups, output):
    expected_names = {case['name'] for case in json.loads(fixtures.read_text())}
    expected, records, seen = {}, {}, {}
    for label, variants in groups.items():
        records[label] = {}
        for phase, entry in variants.items():
            path = entry['path']
            digest = hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
            if 'sha256' in entry and digest != entry['sha256']:
                raise ValueError(f'binary changed: {path}')
            if path not in seen:
                process = subprocess.run([path, str(fixtures), '0', 'validate'],
                                         check=True, capture_output=True, text=True)
                rows = [json.loads(line) for line in process.stdout.splitlines()]
                if len(rows) != len(expected_names) or {r['name'] for r in rows} != expected_names:
                    raise ValueError('missing or duplicate case')
                result = {}
                for row in rows:
                    name = row['name']
                    if 'result_json' not in row:
                        raise ValueError('validation omitted full output')
                    exact = [row.get(key) for key in ['result_json', 'stdout_hex', 'stderr_hex']]
                    signature = [row[field] for field in FIELDS]
                    if expected.setdefault(name, (exact, signature)) != (exact, signature):
                        raise ValueError(f'output/accounting mismatch: {label}/{phase}/{name}')
                    result[name] = {'signature': signature, 'output_sha256': hashlib.sha256(
                        json.dumps(exact, ensure_ascii=False).encode()).hexdigest()}
                seen[path] = result
            records[label][phase] = {'sha256': digest, 'results': seen[path]}
        print(f'{label}: all outputs and accounting matched', flush=True)
    output.write_text(json.dumps({'results': records,
        'fixtures_sha256': hashlib.sha256(fixtures.read_bytes()).hexdigest()}, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('groups', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    validate(args.fixtures.resolve(), json.loads(args.groups.read_text()), args.output.resolve())
