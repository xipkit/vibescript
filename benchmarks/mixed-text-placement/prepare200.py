#!/usr/bin/env python3
"""Prepare exact fixtures and target command manifests for the VIBE-200 run."""
import argparse
import gzip
import json
import pathlib


def prepare(build, wasmtime, cache_config):
    tools = pathlib.Path(__file__).resolve().parent
    build.mkdir(parents=True, exist_ok=True)
    guards = gzip.decompress((tools / 'guards.json.gz').read_bytes())
    (build / 'guards.json').write_bytes(guards)
    escape_names = {'json/stringify_esc_prose_500k/unlimited',
                    'json/stringify_html_prose_500k/unlimited', 'json_stringify_escaped_4k/unlimited'}
    escapes = [case for case in json.loads(guards) if case['name'] in escape_names]
    (build / 'escape-cases.json').write_text(json.dumps(escapes, ensure_ascii=False) + '\n')
    focused = json.loads((tools / 'cases.json').read_text())
    focused.append(next(case for case in json.loads(guards)
                        if case['name'] == 'json/stringify_russian_900k/unlimited'))
    (build / 'placement-cases.json').write_text(json.dumps(focused, ensure_ascii=False) + '\n')
    prefix = [str(wasmtime)]
    if cache_config:
        prefix += ['-C', f'cache-config={cache_config}']
    prefix += ['--dir', str(build)]
    groups = {'simd': {'head': [str(build / 'current/compare')],
                       'signed': [str(build / 'signed/compare')],
                       'legacy': [str(build / 'base')]}, 'portable': {}, 'wasi': {}}
    for label, directory in [('head', 'current'), ('signed', 'signed'), ('legacy', 'legacy')]:
        groups['portable'][label] = [str(build / f'{directory}-portable/release/examples/compare')]
        groups['wasi'][label] = prefix + [str(build / f'{directory}-wasi/wasm32-wasip1/release/vibescript-wasi-compare.wasm')]
    (build / 'validation-groups.json').write_text(json.dumps(groups, indent=2) + '\n')
    for target, variants in groups.items():
        for label, command in variants.items():
            (build / f'{target}-{label}-asbuilt.json').write_text(json.dumps({
                'local/as-built': {'command': command}}) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    parser.add_argument('--wasmtime', required=True, type=pathlib.Path)
    parser.add_argument('--cache-config', type=pathlib.Path)
    args = parser.parse_args()
    prepare(args.build.resolve(), args.wasmtime.resolve(), args.cache_config)
