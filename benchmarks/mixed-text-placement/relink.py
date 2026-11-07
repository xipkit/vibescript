#!/usr/bin/env python3
"""Relink captured Mach-O objects, auditing isolated and whole-code movement."""
import argparse
import json
import pathlib
import re
import subprocess


def symbols(binary):
    result = {}
    for line in subprocess.check_output(['nm', '-n', str(binary)], text=True).splitlines():
        fields = line.split()
        if len(fields) == 3:
            address, kind, name = fields
            result[name] = (int(address, 16), kind)
    return result


def run(link, reference, output, count, front_step):
    args = json.loads(link.read_text())
    original = symbols(reference)
    selected = [s for s in original if re.search(r'json6writer10write_text$', s)]
    if len(selected) != 1:
        raise ValueError(f'expected one write_text symbol: {selected}')
    selected = selected[0]
    output.mkdir(parents=True, exist_ok=True)
    variants = {}
    for mode in ['local', 'front']:
        prior = None
        for index in range(1, count + 1):
            work = output / f'{mode}-{index:02}'
            work.mkdir(exist_ok=True)
            size = 4 * index if mode == 'local' else front_step * index
            pads = [('_vibe199_pad_a', size)]
            order = ['_vibe199_pad_a']
            if mode == 'local':
                pads.append(('_vibe199_pad_b', 4 * (count + 32) - size))
                order.extend([selected, '_vibe199_pad_b'])
            assembly = '.section __TEXT,__text,regular,pure_instructions\n'
            for name, length in pads:
                assembly += f'.p2align 2\n.globl {name}\n.no_dead_strip {name}\n{name}:\n'
                assembly += 'nop\n' * (length // 4)
            assembly += '.subsections_via_symbols\n'
            (work / 'pad.s').write_text(assembly)
            (work / 'order.txt').write_text('\n'.join(order) + '\n')
            subprocess.run(['cc', '-c', '-arch', 'arm64', '-mmacosx-version-min=11.0.0',
                            str(work / 'pad.s'), '-o', str(work / 'pad.o')], check=True)
            binary = work / 'compare'
            command = args.copy()
            command[command.index('-o') + 1] = str(binary)
            command += [str(work / 'pad.o'), '-Wl,-order_file,' + str(work / 'order.txt')]
            subprocess.run(['cc', *command], capture_output=True, text=True, check=True)
            current = {s: v for s, v in symbols(binary).items() if not s.startswith('_vibe199_')}
            if current.keys() != original.keys():
                raise ValueError('relink changed symbol inventory')
            if prior is not None:
                differences = {s: current[s][0] - prior[s][0] for s in current}
                if mode == 'local':
                    unexpected = {s: d for s, d in differences.items() if d != (4 if s == selected else 0)}
                else:
                    # Check the actual increment; reject linker alignment rounding.
                    unexpected = {s: d for s, d in differences.items()
                                  if current[s][1].lower() == 't' and s != '__mh_execute_header' and d != front_step}
                if unexpected:
                    raise ValueError(f'{mode} placement audit failed: {unexpected}')
            prior = current
            variants[f'{mode}/{index:02}'] = {
                'path': str(binary), 'address': current[selected][0], 'pad_bytes': size,
            }
        print(f'{output.name}: {mode}, {count} placements audited', flush=True)
    (output / 'variants.json').write_text(json.dumps(variants, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('link', type=pathlib.Path)
    parser.add_argument('reference', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('--count', type=int, default=32)
    parser.add_argument('--front-step', type=int, choices=[4, 16], default=16)
    options = parser.parse_args()
    if not 1 <= options.count <= 256:
        parser.error('count must be 1..256')
    run(options.link.resolve(), options.reference.resolve(), options.output.resolve(), options.count, options.front_step)
