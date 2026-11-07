#!/usr/bin/env python3
"""Compare exact outputs and accounting within target groups, saving compact evidence."""
import argparse
import hashlib
import json
import pathlib
import subprocess

from measure import FIELDS


def validate(fixtures, groups, output):
    expected_names = {case['name'] for case in json.loads(fixtures.read_text())}
    all_outputs, result = {}, {}
    for target, variants in groups.items():
        signatures = {}
        result[target] = {}
        for label, command in variants.items():
            process = subprocess.run([*command, str(fixtures), '0', 'validate'],
                                     check=True, capture_output=True, text=True)
            rows = [json.loads(line) for line in process.stdout.splitlines()]
            if len(rows) != len(expected_names) or {r['name'] for r in rows} != expected_names:
                raise ValueError('missing or duplicate case')
            records = {}
            for row in rows:
                name = row['name']
                exact = [row.get(key) for key in ['result_json', 'stdout_hex', 'stderr_hex']]
                if all_outputs.setdefault(name, exact) != exact:
                    raise ValueError(f'output mismatch: {target}/{label}/{name}')
                signature = [row[field] for field in FIELDS]
                if signatures.setdefault(name, signature) != signature:
                    raise ValueError(f'accounting mismatch: {target}/{label}/{name}')
                records[name] = {'signature': signature, 'output_sha256': hashlib.sha256(
                    json.dumps(exact, ensure_ascii=False).encode()).hexdigest()}
            result[target][label] = records
            print(f'{target}/{label}: {len(rows)} exact outputs and accounting matched', flush=True)
    output.write_text(json.dumps({'commands': groups, 'results': result,
        'fixtures_sha256': hashlib.sha256(fixtures.read_bytes()).hexdigest()}, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fixtures', type=pathlib.Path)
    parser.add_argument('groups', type=pathlib.Path, help='target -> label -> command array')
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    validate(args.fixtures.resolve(), json.loads(args.groups.read_text()), args.output.resolve())
