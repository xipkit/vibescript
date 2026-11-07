#!/usr/bin/env python3
"""Independently audit the generated placement and copy-route executables."""
import argparse
import json
import pathlib
import struct
import subprocess

from copy_return import branch, symbols, text_section


def audit(build):
    groups = json.loads((build / 'groups.json').read_text())
    result = {'isolated': {}, 'copy_routes': {}}
    for label in ['string', 'prefix', 'run', 'signed']:
        variants = groups[label]
        first = None
        for index, entry in enumerate(variants.values()):
            current = {k: v for k, v in symbols(entry['path']).items()
                       if not k.startswith('_vibe201_')}
            if first is None:
                first = current
            if current.keys() != first.keys() or any(
                    current[s] != (value[0] + (4 * index if s == entry['symbol'] else 0), value[1])
                    for s, value in first.items()):
                raise ValueError(f'non-isolated movement: {label}/{index}')
        result['isolated'][label] = {'placements': len(variants), 'checked_symbols': len(first)}
    for label in ['anchored', 'moving']:
        result['copy_routes'][label] = {}
        for key, entry in groups[label].items():
            source = pathlib.Path(groups['string'][key if label == 'anchored' else '00']['path'])
            target = pathlib.Path(entry['path'])
            a, b = source.read_bytes(), target.read_bytes()
            geometry = text_section(a)
            if text_section(b) != geometry or symbols(source) != symbols(target):
                raise ValueError('patch changed section geometry or symbol placement')
            address, size, offset = geometry
            differences = {address + i for i in range(0, size, 4)
                           if a[offset + i:offset + i + 4] != b[offset + i:offset + i + 4]}
            expected = [(entry['copy_call'], branch(entry['copy_call'], entry['address'])),
                        (entry['address'], branch(entry['address'], entry['copy_stub'], True)),
                        (entry['copy_return'], branch(entry['copy_return'], entry['resume']))]
            if differences != {pc for pc, _ in expected} or any(
                    struct.unpack_from('<I', b, offset + pc - address)[0] != opcode
                    for pc, opcode in expected):
                raise ValueError('unexpected instruction change')
            bindings = subprocess.check_output(['otool', '-Iv', str(target)], text=True)
            if not any(len(parts := line.split()) == 3 and parts[0].startswith('0x')
                       and int(parts[0], 16) == entry['copy_stub'] and parts[2] == '_memcpy'
                       for line in bindings.splitlines()):
                raise ValueError('copy route does not call memcpy')
            subprocess.run(['codesign', '--verify', str(target)], capture_output=True, check=True)
            result['copy_routes'][label][key] = {'changed_instruction_addresses': sorted(differences),
                                               'copy_stub': entry['copy_stub']}
    (build / 'copy-audit.json').write_text(json.dumps(result, indent=2) + '\n')
    print('All placements isolated; all 64 copy routes change exactly three instructions.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('build', type=pathlib.Path)
    args = parser.parse_args()
    audit(args.build.resolve())
