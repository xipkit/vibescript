#!/usr/bin/env python3
"""Move one executed JSON function through a complete 128-byte cycle on ARM64."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'mixed-text-placement'))
from relink import symbols

SUFFIXES = {'string': 'json6writer12write_string',
            'prefix': 'scan10run_prefix', 'run': '6Output7put_run'}


def run(link, reference, output, target):
    args = json.loads(link.read_text())
    original = symbols(reference)
    selected = [s for s in original if s.endswith(SUFFIXES[target])]
    if len(selected) != 1:
        raise ValueError(f'expected one {target} symbol: {selected}')
    selected = selected[0]
    output.mkdir(parents=True, exist_ok=True)
    variants, first = {}, None
    for index in range(32):
        work = output / f'{index:02}'
        work.mkdir(exist_ok=True)
        size = 4 * (index + 1)
        assembly = '.section __TEXT,__text,regular,pure_instructions\n'
        for name, length in [('_vibe201_pad_a', size), ('_vibe201_pad_b', 256 - size)]:
            assembly += f'.p2align 2\n.globl {name}\n.no_dead_strip {name}\n{name}:\n'
            assembly += 'nop\n' * (length // 4)
        assembly += '.subsections_via_symbols\n'
        (work / 'pad.s').write_text(assembly)
        (work / 'order.txt').write_text(f'_vibe201_pad_a\n{selected}\n_vibe201_pad_b\n')
        subprocess.run(['cc', '-c', '-arch', 'arm64', '-mmacosx-version-min=11.0.0',
                        str(work / 'pad.s'), '-o', str(work / 'pad.o')], check=True)
        binary = work / 'compare'
        command = args.copy()
        command[command.index('-o') + 1] = str(binary)
        command += [str(work / 'pad.o'), '-Wl,-order_file,' + str(work / 'order.txt')]
        process = subprocess.run(['cc', *command], capture_output=True, text=True)
        (work / 'link.log').write_text(process.stdout + process.stderr)
        process.check_returncode()
        current = {s: v for s, v in symbols(binary).items() if not s.startswith('_vibe201_')}
        if current.keys() != original.keys():
            raise ValueError('relink changed symbol inventory')
        if first is None:
            first = current
            (output / 'symbols.json').write_text(json.dumps(first, indent=2) + '\n')
        differences = {s: current[s][0] - first[s][0] for s in current}
        unexpected = {s: d for s, d in differences.items() if d != (4 * index if s == selected else 0)}
        if unexpected:
            raise ValueError(f'isolated placement audit failed: {unexpected}')
        variants[f'{index:02}'] = {
            'path': str(binary), 'symbol': selected, 'address': current[selected][0],
            'pad_bytes': size, 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        }
    (output / 'variants.json').write_text(json.dumps(variants, indent=2) + '\n')
    print(f'{output}: {target}, 32 placements audited', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('link', type=pathlib.Path)
    parser.add_argument('reference', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('target', choices=SUFFIXES)
    options = parser.parse_args()
    run(options.link.resolve(), options.reference.resolve(), options.output.resolve(), options.target)
