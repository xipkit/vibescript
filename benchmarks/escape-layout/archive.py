#!/usr/bin/env python3
"""Archive complete cycles, validation, disassembly and provenance compactly."""
import argparse
import gzip
import hashlib
import json
import pathlib

from summarize import summarize


def archive(build, output):
    result = {'experiments': {}, 'summaries': {}, 'context': {}, 'symbols': {}, 'inputs': {},
              'optional_context_absent': []}
    for name in ['isolate-211', 'confirm-223', 'confirm-227', 'confirm-229']:
        path = build / f'{name}.json'
        if name in ['isolate-211', 'confirm-229'] and not path.exists():
            result['optional_context_absent'].append(name)
            continue
        data = json.loads(path.read_text())
        result['summaries'][name] = summarize(data)
        result['experiments'][name] = data
    result['excluded_from_inference'] = {}
    if 'confirm-229' in result['experiments']:
        result['excluded_from_inference']['confirm-229'] = json.loads(
            (build / 'confirm-229-protocol-deviation.json').read_text())
    if (build / 'confirm-233.json').exists():
        result['excluded_from_inference']['confirm-233'] = json.loads(
            (build / 'confirm-233-protocol-deviation.json').read_text())
        result['incomplete_experiments'] = {'confirm-233': json.loads((build / 'confirm-233.json').read_text())}
    else:
        result['optional_context_absent'].append('incomplete_confirm-233')
    result['validation'] = json.loads((build / 'validation.json').read_text())
    fixtures = (build / 'cases.json').read_bytes()
    result['fixtures'] = json.loads(fixtures)
    digest = hashlib.sha256(fixtures).hexdigest()
    for data in [*result['experiments'].values(), result['validation']]:
        if data['fixtures_sha256'] != digest:
            raise ValueError('fixture hash mismatch')
    for data in result['experiments'].values():
        for label, variants in data['variants'].items():
            for key, entry in variants.items():
                binary = pathlib.Path(entry['path'])
                actual = hashlib.sha256(binary.read_bytes()).hexdigest()
                if actual != result['validation']['results'][label][key]['sha256']:
                    raise ValueError(f'binary changed since validation: {binary}')
                checked = result['validation']['results'][label][key]['results']
                if any(signature != checked[case]['signature']
                       for case, signature in data['signatures'].items()):
                    raise ValueError('timing and full-output signatures differ')
    parent = build / 'vibe-200-preserved/phase4.json'
    if parent.exists():
        result['parent_phase4'] = json.loads(parent.read_text())
    else:
        result['optional_context_absent'].append('parent_phase4')
    for name in ['head-string.s', 'head-prefix.s', 'head-run.s', 'signed-string.s',
                 'signed-prefix.s', 'signed-run.s', 'profile.txt', 'environment.json',
                 'profile-result.json']:
        path = build / name
        if path.exists():
            result['context'][name] = path.read_text()
        else:
            result['optional_context_absent'].append(name)
    result['context']['copy-audit.json'] = (build / 'copy-audit.json').read_text()
    for label, targets in [('head', ['string', 'prefix', 'run']), ('signed', ['string'])]:
        for target in targets:
            path = build / 'isolation' / label / target / 'symbols.json'
            result['symbols'][f'{label}/{target}'] = json.loads(path.read_text())
    for label in ['head', 'signed']:
        args = json.loads((build / f'{label}-link.json').read_text())
        result['inputs'][label] = {'arguments': args, 'files': {p: hashlib.sha256(
            pathlib.Path(p).read_bytes()).hexdigest() for p in args if p.endswith(('.o', '.rlib'))}}
    with gzip.GzipFile(filename=str(output), mode='wb', mtime=0) as stream:
        stream.write((json.dumps(result, separators=(',', ':')) + '\n').encode())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    archive(args.build.resolve(), args.output.resolve())
