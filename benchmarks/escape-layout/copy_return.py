#!/usr/bin/env python3
"""Diagnostic copy-return relocation in preserved ARM64 executables, not a fix.

Replace write_string's ordinary-run BL with B to a two-instruction island:
BL to the original copy stub, then B back to the original return address.
The function saves/restores LR in its existing prologue/epilogue. This moves
the copy return PC without changing the copy arguments or surrounding code.
"""
import argparse
import hashlib
import json
import pathlib
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'mixed-text-placement'))
from relink import symbols


def text_section(data):
    if struct.unpack_from('<II', data) != (0xfeedfacf, 0x100000c):
        raise ValueError('expected ARM64 Mach-O')
    cursor = 32
    for _ in range(struct.unpack_from('<I', data, 16)[0]):
        command, size = struct.unpack_from('<II', data, cursor)
        if command == 0x19:
            for index in range(struct.unpack_from('<I', data, cursor + 64)[0]):
                section = cursor + 72 + index * 80
                if data[section:section + 16].rstrip(b'\0') == b'__text':
                    return struct.unpack_from('<QQI', data, section + 32)
        cursor += size
    raise ValueError('no text section')


def branch(source, target, link=False):
    delta = target - source
    if delta % 4 or not -(1 << 27) <= delta < (1 << 27):
        raise ValueError('branch out of range or unaligned')
    return (0x94000000 if link else 0x14000000) | ((delta // 4) & 0x3ffffff)


def patch(source, output, call_offset, island):
    data = bytearray(source.read_bytes())
    syms = symbols(source)
    writer = [s for s in syms if s.endswith('json6writer12write_string')]
    if len(writer) != 1:
        raise ValueError('expected one writer')
    call = syms[writer[0]][0] + call_offset
    address, size, offset = text_section(data)
    def location(pc):
        if not address <= pc <= address + size - 4:
            raise ValueError('outside text')
        return offset + pc - address
    instruction = struct.unpack_from('<I', data, location(call))[0]
    if instruction & 0xfc000000 != 0x94000000:
        raise ValueError('expected BL at copy call offset')
    immediate = instruction & 0x3ffffff
    if immediate & 0x2000000:
        immediate -= 0x4000000
    target = call + 4 * immediate
    bindings = subprocess.check_output(['otool', '-Iv', str(source)], text=True)
    if not any(len(parts := line.split()) == 3 and parts[0].startswith('0x')
               and int(parts[0], 16) == target and parts[2] == '_memcpy'
               for line in bindings.splitlines()):
        raise ValueError('selected BL does not target the memcpy import stub')
    for pc in (island, island + 4):
        if struct.unpack_from('<I', data, location(pc))[0] != 0xd503201f:
            raise ValueError('island would overwrite a non-NOP instruction')
    changes = [(call, branch(call, island)),
               (island, branch(island, target, True)),
               (island + 4, branch(island + 4, call + 4))]
    for pc, value in changes:
        struct.pack_into('<I', data, location(pc), value)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(data)
    shutil.copymode(source, output)
    subprocess.run(['codesign', '--force', '--sign', '-', str(output)],
                   capture_output=True, check=True)
    if symbols(output) != syms:
        raise ValueError('patch changed symbol placement')
    return {'path': str(output), 'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
            'sha256': hashlib.sha256(output.read_bytes()).hexdigest(),
            'address': island, 'copy_call': call, 'copy_stub': target,
            'copy_return': island + 4, 'resume': call + 4, 'patches': changes}


def run(manifest, output, mode, call_offset):
    variants = json.loads(manifest.read_text())
    paths = [pathlib.Path(entry['path']) for entry in variants.values()]
    # Every sweep has at least 128 bytes of unused trailing pad. Anchoring at
    # the end fixes the island even as the writer and pad_b entry move.
    end = symbols(paths[0])['_vibe201_pad_a'][0] + 256
    # Function size is the distance from the writer to pad_b in any variant.
    first_symbols = symbols(paths[0])
    writer = next(s for s in first_symbols if s.endswith('json6writer12write_string'))
    end += first_symbols['_vibe201_pad_b'][0] - first_symbols[writer][0]
    results = {}
    for index, (key, entry) in enumerate(variants.items()):
        if mode == 'anchored':
            source, island = pathlib.Path(entry['path']), end - 16
        else:
            # First variant's trailing pad is 252 bytes: a full 128-byte
            # sweep plus both island instructions fits without touching code.
            source = paths[0]
            island = first_symbols['_vibe201_pad_b'][0] + 4 * index
        results[key] = patch(source, output / key / 'compare', call_offset, island)
    (output / 'variants.json').write_text(json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('mode', choices=['anchored', 'moving'])
    parser.add_argument('--call-offset', type=lambda s: int(s, 0), default=0x6e0)
    args = parser.parse_args()
    run(args.manifest.resolve(), args.output.resolve(), args.mode, args.call_offset)
