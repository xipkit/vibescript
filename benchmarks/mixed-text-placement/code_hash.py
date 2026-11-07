#!/usr/bin/env python3
"""Hash native instructions or all non-custom WASM sections, excluding debug metadata."""
import hashlib
import json
import pathlib
import subprocess
import sys


def code(path):
    data = path.read_bytes()
    if data[:8] == b'\0asm\x01\0\0\0':
        index, parts = 8, []
        while index < len(data):
            start = index
            tag = data[index]
            index += 1
            size = shift = 0
            while True:
                byte = data[index]
                index += 1
                size |= (byte & 127) << shift
                shift += 7
                if byte < 128:
                    break
                if shift > 35:
                    raise ValueError('invalid section length')
            if index + size > len(data):
                raise ValueError('truncated section')
            if tag:
                parts.append(data[start:index + size])
            index += size
        return b''.join(parts)
    output = subprocess.check_output(['otool', '-s', '__TEXT', '__text', str(path)], text=True)
    return bytes.fromhex(''.join(''.join(line.split()[1:]) for line in output.splitlines()[2:]))


if __name__ == '__main__':
    result = {}
    for name in sys.argv[1:]:
        path = pathlib.Path(name)
        instructions = code(path)
        result[name] = {'binary_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                        'code_sha256': hashlib.sha256(instructions).hexdigest(),
                        'code_bytes': len(instructions)}
    print(json.dumps(result, indent=2))
